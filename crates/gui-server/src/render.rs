//! Server-side HTML via Mustache templates embedded with `rust-embed`.
//!
//! The page chrome (surface wrapper, head snippet, inspector, agent page) lives in
//! `templates/*.mustache` — one file each — so markup is edited as markup, not Rust string
//! literals. Templates render through `mustache` over `Serialize` data; user-controlled
//! strings use `{{ }}` (HTML-escaped) and only trusted raw HTML/JSON uses `{{{ }}}`.

use rust_embed::RustEmbed;
use serde::Serialize;

use crate::api::{self, DeckGraph};
use crate::discovery::Index;

#[derive(RustEmbed)]
#[folder = "templates/"]
struct Templates;

/// Compile + render an embedded template. Templates are first-party and validated by tests, so
/// a missing/broken template is a programmer error (panic), not a runtime input failure.
fn render(name: &str, data: &impl Serialize) -> String {
    let file = Templates::get(name).unwrap_or_else(|| panic!("embedded template {name} present"));
    let src = std::str::from_utf8(&file.data).expect("template is utf-8");
    let tmpl = mustache::compile_str(src).unwrap_or_else(|e| panic!("compile {name}: {e}"));
    let mut out = Vec::new();
    tmpl.render(&mut out, data).unwrap_or_else(|e| panic!("render {name}: {e}"));
    String::from_utf8(out).expect("rendered template is utf-8")
}

/// Everything the browser runtime needs to know about the surface being served.
pub struct PageCtx {
    /// `agent/deck/surface`.
    pub surface_id: String,
    pub version: String,
    pub title: String,
    /// `/{agent}/{deck}/{surface}/v/{version}/` — the `<base>` so relative assets resolve.
    pub base_url: String,
    /// Serialized [`DeckGraph`] for navigation.
    pub deck_json: String,
}

#[derive(Serialize)]
struct HeadData {
    base: String,
    surface_id: String,
    version: String,
    deck: String,
}

#[derive(Serialize)]
struct SurfaceDoc {
    title: String,
    head: String,
    body: String,
}

/// The `<head>` snippet injected into every served surface.
fn head_snippet(ctx: &PageCtx) -> String {
    render(
        "head.html.mustache",
        &HeadData {
            base: ctx.base_url.clone(),
            surface_id: ctx.surface_id.clone(),
            version: ctx.version.clone(),
            // Embedding JSON inside a <script> requires neutralizing any "</" sequence.
            deck: ctx.deck_json.replace("</", "<\\/"),
        },
    )
}

/// Case-insensitive substring search, returning the byte offset of the match.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.to_ascii_lowercase();
    h.find(&needle.to_ascii_lowercase())
}

/// Render an artifact's `index.html` into a full page with the suite wired in.
///
/// A full document (has `</head>`) gets the snippet injected before `</head>`. A bare fragment
/// of `<ui-*>` elements is wrapped via the `surface` template.
pub fn render_surface(html: &str, ctx: &PageCtx) -> String {
    let snippet = head_snippet(ctx);

    if let Some(pos) = find_ci(html, "</head>") {
        let mut out = String::with_capacity(html.len() + snippet.len() + 1);
        out.push_str(&html[..pos]);
        out.push_str(&snippet);
        out.push('\n');
        out.push_str(&html[pos..]);
        return out;
    }

    render(
        "surface.html.mustache",
        &SurfaceDoc {
            title: ctx.title.clone(),
            head: snippet,
            body: html.to_string(),
        },
    )
}

/// Build the [`PageCtx`] for a surface version. `deck` must be the surface's deck.
pub fn page_ctx(
    path: &gui_schema::SurfacePath,
    version: &gui_schema::Version,
    title: &str,
    deck: &crate::discovery::Deck,
) -> PageCtx {
    let graph: DeckGraph = api::deck_graph(deck);
    PageCtx {
        surface_id: path.id(),
        version: version.to_string(),
        title: title.to_string(),
        base_url: format!("/{}/{}/{}/v/{}/", path.agent, path.deck, path.surface, version),
        deck_json: serde_json::to_string(&graph).unwrap_or_else(|_| "{}".to_string()),
    }
}

// --- inspector + agent pages ----------------------------------------------

#[derive(Serialize)]
struct InspectorData {
    agent_count: usize,
    surface_count: usize,
    empty: bool,
    agents: Vec<AgentView>,
}

#[derive(Serialize)]
struct AgentView {
    slug: String,
    decks: Vec<DeckView>,
}

#[derive(Serialize)]
struct DeckView {
    slug: String,
    url: String,
    surfaces: Vec<SurfaceView>,
}

#[derive(Serialize)]
struct SurfaceView {
    url: String,
    title: String,
    version: String,
    versions: usize,
    events: usize,
    versions_url: String,
}

/// Server-rendered inspector: the discovered tree with live versions, backflow counts, and
/// deck links. Live-updates via an `EventSource('/events')` that reloads on any change.
pub fn inspector_page(idx: &Index, root: &std::path::Path) -> String {
    let agents = idx
        .agents
        .values()
        .map(|agent| AgentView {
            slug: agent.slug.clone(),
            decks: agent
                .decks
                .values()
                .map(|deck| DeckView {
                    slug: deck.slug.clone(),
                    url: format!("/{}/{}/", agent.slug, deck.slug),
                    surfaces: deck
                        .ordered_surfaces()
                        .into_iter()
                        .map(|s| {
                            let live = s.live();
                            SurfaceView {
                                url: s.path.url(),
                                title: live.manifest.title.clone(),
                                version: live.version.to_string(),
                                versions: s.versions.len(),
                                events: crate::server::event_count(root, &s.path),
                                versions_url: format!("/api{}/versions.json", s.path.url().trim_end_matches('/')),
                            }
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();

    render(
        "inspector.html.mustache",
        &InspectorData {
            agent_count: idx.agents.len(),
            surface_count: idx.surface_count(),
            empty: idx.agents.is_empty(),
            agents,
        },
    )
}

#[derive(Serialize)]
struct AgentPage {
    slug: String,
    decks: Vec<DeckLink>,
}

#[derive(Serialize)]
struct DeckLink {
    slug: String,
    url: String,
}

/// A single agent's page: its decks, each linking to the deck's entry surface.
pub fn agent_page(slug: &str, deck_slugs: impl Iterator<Item = String>) -> String {
    render(
        "agent.html.mustache",
        &AgentPage {
            slug: slug.to_string(),
            decks: deck_slugs
                .map(|d| DeckLink {
                    url: format!("/{slug}/{d}/"),
                    slug: d,
                })
                .collect(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> PageCtx {
        PageCtx {
            surface_id: "demo/main/sales".into(),
            version: "100".into(),
            title: "Sales".into(),
            base_url: "/demo/main/sales/v/100/".into(),
            deck_json: r#"{"order":["sales"]}"#.into(),
        }
    }

    #[test]
    fn wraps_a_fragment() {
        let out = render_surface("<ui-text>hi</ui-text>", &ctx());
        assert!(out.starts_with("<!doctype html>"));
        assert!(out.contains("<ui-text>hi</ui-text>"));
        assert!(out.contains(r#"<meta name="surface" content="demo/main/sales">"#));
        assert!(out.contains(r#"<base href="/demo/main/sales/v/100/">"#));
        assert!(out.contains(r#"<script type="module" src="/suite/runtime.js"></script>"#));
    }

    #[test]
    fn injects_into_existing_head() {
        let doc = "<!doctype html><html><head><title>x</title></head><body>z</body></html>";
        let out = render_surface(doc, &ctx());
        let head_end = out.find("</head>").unwrap();
        assert!(out[..head_end].contains(r#"<meta name="version" content="100">"#));
        assert!(out.contains("<body>z</body>"));
        assert_eq!(out.matches("<body>").count(), 1);
    }

    #[test]
    fn neutralizes_script_close_in_deck_json() {
        let mut c = ctx();
        c.deck_json = r#"{"x":"</script>"}"#.into();
        let out = render_surface("<ui-text/>", &c);
        assert!(!out.contains("</script></script>"));
        assert!(out.contains(r#"<\/script>"#));
    }

    #[test]
    fn inspector_lists_surfaces() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let d = root.join("demo/main/sales/v/1");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("frame.json"),
            r#"{"schema_version":"generative-ui/surface.v1","title":"Sales","alt_text":"a"}"#,
        )
        .unwrap();
        std::fs::write(d.join("index.html"), "<ui-text>hi</ui-text>").unwrap();
        let idx = Index::build(root);
        let html = inspector_page(&idx, root);
        assert!(html.contains("Generative UI Suite"));
        assert!(html.contains(r#"href="/demo/main/sales/"#));
        assert!(html.contains("Sales"));
        assert!(html.contains("gui-topbar"));
    }
}
