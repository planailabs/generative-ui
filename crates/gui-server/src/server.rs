//! The axum application: shared state, router, and request handlers.
//!
//! Routes resolve generic paths against the discovered [`Index`]; the directory decides
//! what each path means. The two-segment route `/{agent}/{x}/` disambiguates a deck from a
//! `main`-deck surface at request time.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Json, Response},
    routing::get,
    Router,
};
use gui_schema::{SurfacePath, Version, DEFAULT_DECK};

use crate::discovery::Index;
use crate::{api, render};

/// Shared, hot-swappable application state.
#[derive(Clone)]
pub struct AppState {
    pub index: Arc<ArcSwap<Index>>,
    pub root: PathBuf,
}

impl AppState {
    /// Build state by scanning `root` once.
    pub fn new(root: PathBuf) -> Self {
        let index = Index::build(&root);
        Self {
            index: Arc::new(ArcSwap::from_pointee(index)),
            root,
        }
    }

    /// Re-scan and atomically swap the index (used by the Stage 7 watcher).
    pub fn reload(&self) -> Arc<Index> {
        let fresh = Arc::new(Index::build(&self.root));
        self.index.store(fresh.clone());
        fresh
    }
}

// The component suite, embedded at compile time (native ESM + CSS, no build step).
const SUITE_RUNTIME: &str = include_str!("../../../suite/runtime.js");
const SUITE_LIB: &str = include_str!("../../../suite/lib.js");
const SUITE_CSS: &str = include_str!("../../../suite/suite.css");

/// Construct the router. Exposed so tests can serve it on an ephemeral port.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(inspector))
        .route("/suite/{file}", get(suite_file))
        .route("/api/index.json", get(api_index))
        .route("/api/activity.json", get(api_activity))
        .route("/api/{agent}/{deck}/deck.json", get(api_deck))
        .route("/api/{agent}/{deck}/{surface}/versions.json", get(api_versions))
        .route(
            "/{agent}/{deck}/{surface}/v/{version}/assets/{*path}",
            get(asset),
        )
        .route("/{agent}/{deck}/{surface}/v/{version}/", get(version_page))
        .route("/{agent}/{deck}/{surface}/", get(live_page))
        .route("/{agent}/{seg}/", get(two_seg))
        .route("/{agent}/", get(agent_page))
        .with_state(state)
}

/// Bind and serve until the process is signalled.
pub async fn serve(addr: &str, state: AppState) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    eprintln!("listening on http://{bound}");
    axum::serve(listener, router(state)).await?;
    Ok(())
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
    Html(render::inspector_page(&st.index.load_full()))
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
