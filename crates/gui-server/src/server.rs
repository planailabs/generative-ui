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
use gui_schema::{atomic_write, Event, SurfacePath, Version, DEFAULT_DECK, EVENT_SCHEMA_VERSION};
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
}

impl AppState {
    /// Build state by scanning `root` once.
    pub fn new(root: PathBuf) -> Self {
        let index = Index::build(&root);
        let (events_tx, _) = broadcast::channel(64);
        Self {
            index: Arc::new(ArcSwap::from_pointee(index)),
            root,
            events_tx,
        }
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

/// Watch the artifacts tree; on any change, re-index and notify SSE subscribers. The
/// returned watcher must be kept alive for watching to continue.
///
/// ponytail: reloads on every fs event (a publish fires a few). Swap in `notify-debouncer`
/// if churn ever matters — the index rebuild is cheap for modest trees.
pub fn spawn_watcher(state: AppState) -> notify::Result<notify::RecommendedWatcher> {
    use notify::{RecursiveMode, Watcher};
    std::fs::create_dir_all(&state.root).ok();
    let st = state.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            st.reload();
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
        .route(
            "/{agent}/{deck}/{surface}/v/{version}/assets/{*path}",
            get(asset),
        )
        .route("/{agent}/{deck}/{surface}/v/{version}/", get(version_page))
        .route("/{agent}/{deck}/{surface}/", get(live_page))
        .route("/backflow/{agent}/{deck}/{surface}", post(backflow))
        .route("/{agent}/{seg}/", get(two_seg))
        .route("/{agent}/", get(agent_page))
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
    evs.sort_by(|a, b| b.0.cmp(&a.0));
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
    let events = read_surface_events(&st.root, &path, 100);
    Json(serde_json::json!({ "surface": path.id(), "events": events })).into_response()
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
    if let Ok(p) = SurfacePath::new(&agent, DEFAULT_DECK, &seg) {
        if idx.surface(&p).is_some() {
            return serve_surface(&idx, &p, None);
        }
    }
    not_found()
}

async fn agent_page(State(st): State<AppState>, Path(agent): Path<String>) -> Response {
    let idx = st.index.load_full();
    let Some(a) = idx.agent(&agent) else {
        return not_found();
    };
    let mut body = format!("<h1>{}</h1><ul>", gui_schema::esc(&a.slug));
    for deck in a.decks.values() {
        body.push_str(&format!(
            "<li><a href=\"/{}/{}/\">{}</a></li>",
            gui_schema::esc(&a.slug),
            gui_schema::esc(&deck.slug),
            gui_schema::esc(&deck.slug)
        ));
    }
    body.push_str("</ul>");
    Html(format!("<!doctype html><meta charset=utf-8><body>{body}")).into_response()
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
        received_at: iso,
    };
    let target = path.events_dir(&st.root).join(event.file_name(ms));
    let bytes = serde_json::to_vec_pretty(&event).expect("event serializes");
    match atomic_write(&target, &bytes) {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "ok": true, "event": target.file_name().and_then(|s| s.to_str()) })),
        )
            .into_response(),
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Html(format!("write failed: {e}"))).into_response()
        }
    }
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
