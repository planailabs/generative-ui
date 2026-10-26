//! The axum application: shared state, router, and request handlers.
//!
//! Routes resolve generic paths against the discovered [`Index`]; the directory decides
//! what each path means. The two-segment route `/{agent}/{x}/` disambiguates a deck from a
//! `main`-deck surface at request time.

use std::convert::Infallible;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        Html, IntoResponse, Json, Response,
    },
    routing::{get, post},
    Router,
};
use gui_schema::{atomic_write, Event, Scope, SurfacePath, Version, DEFAULT_DECK, EVENT_SCHEMA_VERSION};
use serde::Deserialize;
use tokio::sync::broadcast;
use tokio_stream::{wrappers::BroadcastStream, Stream, StreamExt};

use crate::discovery::Index;
use crate::{api, render};

/// Shared, hot-swappable application state.
#[derive(Clone)]
pub struct AppState {
    pub index: Arc<ArcSwap<Index>>,
    pub root: PathBuf,
    /// Fan-out for SSE: a small JSON line per discovery/backflow change.
    pub events_tx: broadcast::Sender<String>,
    /// Whether matching backflow actions run their surface handler. On by default (handlers are
    /// sandboxed — default-deny capabilities + limits); turn off with `--disable-handlers`.
    pub enable_handlers: bool,
}

impl AppState {
    /// Build state by scanning `root` once. Handler execution is enabled by default.
    pub fn new(root: PathBuf) -> Self {
        let index = Index::build(&root);
        let (events_tx, _) = broadcast::channel(64);
        Self {
            index: Arc::new(ArcSwap::from_pointee(index)),
            root,
            events_tx,
            enable_handlers: true,
        }
    }

    /// Enable (or disable) handler execution on backflow.
    pub fn with_handlers(mut self, enable: bool) -> Self {
        self.enable_handlers = enable;
        self
    }

    /// Re-scan and atomically swap the index (used by the watcher).
    pub fn reload(&self) -> Arc<Index> {
        let fresh = Arc::new(Index::build(&self.root));
        self.index.store(fresh.clone());
        fresh
    }

    /// Broadcast a change notification to SSE subscribers (best-effort).
    pub fn notify(&self, msg: &str) {
        let _ = self.events_tx.send(msg.to_string());
    }
}

/// Whether `p` is `{agent}/{deck}/{surface}/stream[/…]` under `root` (an event, its atomic-write
/// temp, or the dir itself) — by position, so a surface named `stream` isn't one.
fn in_stream(root: &std::path::Path, p: &std::path::Path) -> bool {
    let Ok(rel) = p.strip_prefix(root) else { return false };
    let parts: Vec<_> = rel.components().collect();
    matches!(parts.len(), 4 | 5) && parts[3].as_os_str() == "stream"
}

/// Event files in `{agent}/{deck}/{surface}/stream/` dirs at or below `dir`.
fn collect_stream_files(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(rel) = dir.strip_prefix(root) else { return };
    let depth = rel.components().count();
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for p in rd.filter_map(|e| e.ok()).map(|e| e.path()) {
        if depth == 4 && in_stream(root, &p) && is_event_file(&p) {
            out.push(p);
        } else if depth < 4 && p.is_dir() {
            collect_stream_files(root, &p, out);
        }
    }
}

/// A `{ms}-{name}.json` event file, not an atomic-write `.…tmp…` temp.
fn is_event_file(p: &std::path::Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| !n.starts_with('.') && n.ends_with(".json"))
}

/// Watch the artifacts tree; on any change, re-index and notify SSE subscribers. The
/// returned watcher must be kept alive for watching to continue.
///
/// ponytail: reloads on every fs event (a publish fires a few). Swap in `notify-debouncer`
/// if churn ever matters — the index rebuild is cheap for modest trees.
pub fn spawn_watcher(state: AppState) -> notify::Result<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    std::fs::create_dir_all(&state.root).ok();
    let st = state.clone();
    // notify reports absolute paths; `--artifacts` may be relative.
    let root = std::fs::canonicalize(&state.root).map_err(notify::Error::io)?;
    // inotify reports one renamed-in file several times; push each stream file once.
    let mut pushed: std::collections::VecDeque<PathBuf> = Default::default();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        // Stream events don't change the index: push their payload instead of a ping.
        let mut files: Vec<PathBuf> =
            ev.paths.iter().filter(|p| in_stream(&root, p) && is_event_file(p)).cloned().collect();
        // A file written into a just-created dir lands before inotify watches it and never
        // fires; walk new dirs for stream files.
        if ev.kind.is_create() {
            for p in ev.paths.iter().filter(|p| p.is_dir()) {
                collect_stream_files(&root, p, &mut files);
            }
        }
        for file in &files {
            if pushed.contains(file) {
                continue;
            }
            {
                match std::fs::read(file).map_err(|e| e.to_string()).and_then(|b| {
                    serde_json::from_slice::<gui_schema::Event>(&b).map_err(|e| e.to_string())
                }) {
                    Ok(e) => {
                        st.notify(
                            &serde_json::json!({
                                "type": "stream", "surface": e.surface, "name": e.action, "value": e.value,
                            })
                            .to_string(),
                        );
                        pushed.push_back(file.clone());
                        if pushed.len() > 64 {
                            pushed.pop_front();
                        }
                    }
                    // A temp file renamed away before we read it; its final name fires too.
                    Err(e) => tracing::trace!(file = %file.display(), error = %e, "stream event not readable"),
                }
            }
        }
        if !ev.paths.is_empty() && ev.paths.iter().all(|p| in_stream(&root, p)) {
            return;
        }
        {
            let idx = st.reload();
            tracing::debug!(surfaces = idx.surface_count(), "re-indexed after fs change");
            // A simple ping; clients decide what to refetch. Surface views compare the live
            // version (robust against fs-watch races and ignores backflow, which never bumps
            // the version); the inspector just reloads.
            st.notify("{\"type\":\"changed\"}");
        }
    })?;
    watcher.watch(&state.root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

// The component suite, embedded at compile time (native ESM + CSS, no build step).
const SUITE_RUNTIME: &str = include_str!("../../../suite/runtime.js");
const SUITE_LIB: &str = include_str!("../../../suite/lib.js");
const SUITE_CSS: &str = include_str!("../../../suite/suite.css");

/// Construct the router. Exposed so tests can serve it on an ephemeral port.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(inspector))
        .route("/events", get(sse))
        .route("/suite/{file}", get(suite_file))
        .route("/api/index.json", get(api_index))
        .route("/api/activity.json", get(api_activity))
        .route("/api/{agent}/{deck}/deck.json", get(api_deck))
        .route("/api/{agent}/{deck}/{surface}/versions.json", get(api_versions))
        .route("/api/{agent}/{deck}/{surface}/events.json", get(api_events))
        .route("/api/{agent}/{deck}/{surface}/kv.json", get(api_kv))
        .route(
            "/{agent}/{deck}/{surface}/v/{version}/assets/{*path}",
            get(asset),
        )
        .route("/{agent}/{deck}/{surface}/v/{version}/", get(version_page))
        .route("/{agent}/{deck}/{surface}/", get(live_page))
        .route("/backflow/{agent}/{deck}/{surface}", post(backflow))
        .route("/{agent}/{seg}/", get(two_seg))
        .route("/{agent}/", get(agent_page))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state)
}

/// Bind and serve until the process is signalled.
pub async fn serve(addr: &str, state: AppState) -> anyhow::Result<()> {
    // Keep the watcher alive for the lifetime of the server.
    let _watcher = match spawn_watcher(state.clone()) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("warning: filesystem watch disabled: {e}");
            None
        }
    };
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    eprintln!("listening on http://{bound}");
    axum::serve(listener, router(state)).await?;
    Ok(())
}

/// Recent backflow events for a surface, newest first.
fn read_surface_events(root: &FsPath, path: &SurfacePath, limit: usize) -> Vec<Event> {
    let dir = path.events_dir(root);
    let mut evs: Vec<(u64, Event)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !name.ends_with(".json") {
                continue;
            }
            let ms = name.split('-').next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
            if let Ok(ev) = std::fs::read(e.path()).map_err(|_| ()).and_then(|b| {
                serde_json::from_slice::<Event>(&b).map_err(|_| ())
            }) {
                evs.push((ms, ev));
            }
        }
    }
    evs.sort_by_key(|e| std::cmp::Reverse(e.0));
    evs.into_iter().take(limit).map(|(_, e)| e).collect()
}

/// Count backflow events recorded for a surface.
pub fn event_count(root: &FsPath, path: &SurfacePath) -> usize {
    std::fs::read_dir(path.events_dir(root))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| {
                    let n = e.file_name();
                    let n = n.to_string_lossy();
                    !n.starts_with('.') && n.ends_with(".json")
                })
                .count()
        })
        .unwrap_or(0)
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Html("<h1>404 Not Found</h1>".to_string())).into_response()
}

/// Serve an embedded suite file (`runtime.js`, `lib.js`, `suite.css`).
async fn suite_file(Path(file): Path<String>) -> Response {
    let (body, mime) = match file.as_str() {
        "runtime.js" => (SUITE_RUNTIME, "text/javascript; charset=utf-8"),
        "lib.js" => (SUITE_LIB, "text/javascript; charset=utf-8"),
        "suite.css" => (SUITE_CSS, "text/css; charset=utf-8"),
        _ => return not_found(),
    };
    ([(header::CONTENT_TYPE, mime)], body).into_response()
}

/// Render a surface's live or specific version. `index.html` is read synchronously —
/// ponytail: artifacts are small; switch to `spawn_blocking` only if large media lands here.
fn serve_surface(idx: &Index, path: &SurfacePath, version: Option<Version>) -> Response {
    let Some(surface) = idx.surface(path) else {
        return not_found();
    };
    let sv = match version {
        Some(v) => match surface.version(v) {
            Some(sv) => sv,
            None => return not_found(),
        },
        None => surface.live(),
    };
    let deck = idx
        .deck(&path.agent, &path.deck)
        .expect("a resolved surface always has a deck");
    let html = std::fs::read_to_string(sv.dir.join("index.html")).unwrap_or_default();
    let ctx = render::page_ctx(path, &sv.version, &sv.manifest.title, deck);
    Html(render::render_surface(&html, &ctx)).into_response()
}

async fn inspector(State(st): State<AppState>) -> Html<String> {
    Html(render::inspector_page(&st.index.load_full(), &st.root))
}

/// Server-Sent Events: one message per discovery/backflow change.
async fn sse(State(st): State<AppState>) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = st.events_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| match msg {
        Ok(s) => Some(Ok(SseEvent::default().data(s))),
        Err(_) => None, // lagged subscriber — drop and continue
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn api_events(
    State(st): State<AppState>,
    Path((agent, deck, surface)): Path<(String, String, String)>,
) -> Response {
    let idx = st.index.load_full();
    let Ok(path) = SurfacePath::new(agent, deck, surface) else {
        return not_found();
    };
    if idx.surface(&path).is_none() {
        return not_found();
    }
    // Public events only — the events API is browser-facing; the agent reads all via gui-agent.
    let events: Vec<_> = read_surface_events(&st.root, &path, 100)
        .into_iter()
        .filter(|e| e.scope.is_public())
        .collect();
    Json(serde_json::json!({ "surface": path.id(), "events": events })).into_response()
}

/// The surface's materialized KV view (folded from backflow by its handlers). The UI reads
/// this and live-binds values; `{}` when no handler has produced state yet.
async fn api_kv(
    State(st): State<AppState>,
    Path((agent, deck, surface)): Path<(String, String, String)>,
) -> Response {
    let idx = st.index.load_full();
    let Ok(path) = SurfacePath::new(agent, deck, surface) else {
        return not_found();
    };
    if idx.surface(&path).is_none() {
        return not_found();
    }
    Json(serde_json::Value::Object(path.public_kv(&st.root))).into_response()
}

async fn api_index(State(st): State<AppState>) -> Json<api::IndexResponse> {
    Json(api::index_response(&st.index.load_full()))
}

async fn api_activity(State(st): State<AppState>) -> Json<api::ActivityResponse> {
    Json(api::activity_response(&st.index.load_full()))
}

async fn api_deck(State(st): State<AppState>, Path((agent, deck)): Path<(String, String)>) -> Response {
    let idx = st.index.load_full();
    match idx.deck(&agent, &deck) {
        Some(d) => Json(api::deck_graph(d)).into_response(),
        None => not_found(),
    }
}

async fn api_versions(
    State(st): State<AppState>,
    Path((agent, deck, surface)): Path<(String, String, String)>,
) -> Response {
    let idx = st.index.load_full();
    match SurfacePath::new(agent, deck, surface).ok().and_then(|p| idx.surface(&p)) {
        Some(s) => Json(api::versions_response(s)).into_response(),
        None => not_found(),
    }
}

async fn live_page(
    State(st): State<AppState>,
    Path((agent, deck, surface)): Path<(String, String, String)>,
) -> Response {
    let idx = st.index.load_full();
    match SurfacePath::new(agent, deck, surface) {
        Ok(p) => serve_surface(&idx, &p, None),
        Err(_) => not_found(),
    }
}

async fn version_page(
    State(st): State<AppState>,
    Path((agent, deck, surface, version)): Path<(String, String, String, String)>,
) -> Response {
    let idx = st.index.load_full();
    let (Ok(p), Ok(v)) = (SurfacePath::new(agent, deck, surface), Version::parse(&version)) else {
        return not_found();
    };
    serve_surface(&idx, &p, Some(v))
}

/// `/{agent}/{x}/` — a deck (render its entry surface) or a `main`-deck surface.
async fn two_seg(State(st): State<AppState>, Path((agent, seg)): Path<(String, String)>) -> Response {
    let idx = st.index.load_full();
    if let Some(deck) = idx.deck(&agent, &seg) {
        return match deck.entry() {
            Some(entry) => serve_surface(&idx, &entry.path.clone(), None),
            None => not_found(),
        };
    }
    if let Ok(p) = SurfacePath::new(&agent, DEFAULT_DECK, &seg)
        && idx.surface(&p).is_some() {
            return serve_surface(&idx, &p, None);
        }
    not_found()
}

async fn agent_page(State(st): State<AppState>, Path(agent): Path<String>) -> Response {
    let idx = st.index.load_full();
    let Some(a) = idx.agent(&agent) else {
        return not_found();
    };
    Html(render::agent_page(&a.slug, a.decks.values().map(|d| d.slug.clone()))).into_response()
}

/// Body of a backflow POST. Unknown fields (e.g. the client `timestamp`) are ignored; the
/// server stamps its own `received_at`.
#[derive(Deserialize)]
struct BackflowInput {
    action: String,
    #[serde(default)]
    component: Option<String>,
    #[serde(default)]
    value: serde_json::Value,
    #[serde(default)]
    version: Option<String>,
}

/// Current time as `(unix_millis, rfc3339_string)`.
fn now() -> (u64, String) {
    use time::{format_description::well_known::Rfc3339, OffsetDateTime};
    let now = OffsetDateTime::now_utc();
    let ms = (now.unix_timestamp_nanos() / 1_000_000) as u64;
    (ms, now.format(&Rfc3339).unwrap_or_default())
}

/// Backflow: write a user interaction back into the surface's `events/` directory.
async fn backflow(
    State(st): State<AppState>,
    Path((agent, deck, surface)): Path<(String, String, String)>,
    Json(input): Json<BackflowInput>,
) -> Response {
    let Ok(path) = SurfacePath::new(agent, deck, surface) else {
        return not_found();
    };
    let idx = st.index.load_full();
    let Some(found) = idx.surface(&path) else {
        return not_found();
    };

    let (ms, iso) = now();
    let event = Event {
        schema_version: EVENT_SCHEMA_VERSION.to_string(),
        action: input.action,
        component: input.component,
        value: input.value,
        surface: path.id(),
        version: input.version.or_else(|| Some(found.live().version.to_string())),
        scope: Scope::Public, // a direct UI submission is visible to the browser
        source: Some("ui".to_string()),
        received_at: iso,
    };
    let target = path.events_dir(&st.root).join(event.file_name(ms));
    tracing::info!(surface = %path.id(), action = %event.action, "backflow received");
    // The event log is the source of truth — write it first (audit), regardless of handlers.
    let bytes = serde_json::to_vec_pretty(&event).expect("event serializes");
    if let Err(e) = atomic_write(&target, &bytes) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Html(format!("write failed: {e}"))).into_response();
    }

    // If a handler is registered for this action, run it (when enabled): it folds KV from the
    // event and defines the HTTP response. Handlers never touch the surface HTML.
    if let Some(def) = found.live().manifest.handlers.get(&event.action).cloned() {
        if st.enable_handlers {
            return run_surface_handler(&st, &path, found.live().dir.clone(), def, event).await;
        }
        // Disabled via --disable-handlers — don't silently no-op; say why nothing ran.
        tracing::warn!(
            surface = %path.id(),
            action = %event.action,
            "a {:?} handler is declared for this action but handler execution is off \
             (--disable-handlers); remove the flag to run it (event was still logged)",
            def.lang,
        );
    }

    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "ok": true, "event": target.file_name().and_then(|s| s.to_str()) })),
    )
        .into_response()
}

/// Read a KV file as a JSON object, or an empty object if absent/invalid.
fn load_kv_file(path: &FsPath) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| match v {
            serde_json::Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

fn persist_kv(target: &FsPath, view: &gui_runtime::KvView, surface: &str) {
    if !view.changed {
        return;
    }
    let bytes =
        serde_json::to_vec_pretty(&serde_json::Value::Object(view.map.clone())).expect("kv serializes");
    if let Err(e) = atomic_write(target, &bytes) {
        tracing::error!(surface = %surface, error = %e, "kv persist failed");
    }
}

/// Run a surface's handler on a blocking worker (the engines + host I/O are synchronous),
/// persist any public/private KV change (the public change fires the watcher → SSE), write any
/// emitted events into the log, and return the handler's response.
async fn run_surface_handler(
    st: &AppState,
    path: &SurfacePath,
    version_dir: PathBuf,
    def: gui_schema::HandlerDef,
    event: Event,
) -> Response {
    let root = st.root.clone();
    let path = path.clone();
    let kv_public_path = path.kv_path(&root);
    let kv_private_path = path.kv_private_path(&root);
    let state_dir = path.state_dir(&root);
    let kv_public = load_kv_file(&kv_public_path);
    let kv_private = load_kv_file(&kv_private_path);
    let live_version = event.version.clone();

    let joined = tokio::task::spawn_blocking(move || {
        gui_runtime::run_handler(&def, &version_dir, &state_dir, &event, kv_public, kv_private)
    })
    .await;

    let outcome = match joined {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            tracing::error!(surface = %path.id(), error = %e, "handler failed");
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "ok": false, "error": e.to_string() })),
            )
                .into_response();
        }
        Err(join) => {
            tracing::error!(surface = %path.id(), error = %join, "handler worker panicked");
            return (StatusCode::INTERNAL_SERVER_ERROR, Html("handler crashed".to_string()))
                .into_response();
        }
    };

    persist_kv(&kv_public_path, &outcome.public, &path.id());
    persist_kv(&kv_private_path, &outcome.private, &path.id());

    // Write events the handler emitted into the log for the agent (and public events API);
    // public ones also go to the surface's stream, so viewers see them live.
    let (ms, iso) = now();
    for em in &outcome.emitted {
        let ev = Event {
            schema_version: EVENT_SCHEMA_VERSION.to_string(),
            action: em.name.clone(),
            component: None,
            value: em.data.clone(),
            surface: path.id(),
            version: live_version.clone(),
            scope: em.scope,
            source: Some("handler".to_string()),
            received_at: iso.clone(),
        };
        if let Err(e) = gui_schema::write_event(&path.events_dir(&root), &ev, ms) {
            tracing::error!(surface = %path.id(), error = %e, "emit persist failed");
        }
        if ev.scope.is_public()
            && let Err(e) = gui_schema::write_event(&path.stream_dir(&root), &ev, ms)
        {
            tracing::error!(surface = %path.id(), error = %e, "stream persist failed");
        }
    }

    let status = StatusCode::from_u16(outcome.response.status).unwrap_or(StatusCode::OK);
    (status, Json(outcome.response.body)).into_response()
}

async fn asset(
    State(st): State<AppState>,
    Path((agent, deck, surface, version, rest)): Path<(String, String, String, String, String)>,
) -> Response {
    let (Ok(p), Ok(v)) = (SurfacePath::new(agent, deck, surface), Version::parse(&version)) else {
        return not_found();
    };
    // Path-traversal guard: no empty or `..` components.
    if rest.split('/').any(|c| c.is_empty() || c == "..") {
        return not_found();
    }
    let mut full = p.version_dir(&st.root, v).join("assets");
    for c in rest.split('/') {
        full.push(c);
    }
    match std::fs::read(&full) {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&full).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref().to_string())], bytes).into_response()
        }
        Err(_) => not_found(),
    }
}
