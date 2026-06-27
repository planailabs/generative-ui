//! Server-side HTML: inject the suite runtime + surface context into an artifact, and
//! render the inspector / agent pages.

use gui_schema::esc;

use crate::api::{self, DeckGraph};
use crate::discovery::Index;

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

/// The `<head>` snippet injected into every served surface.
fn head_snippet(ctx: &PageCtx) -> String {
    // Embedding JSON inside a <script> requires neutralizing any "</" sequence.
    let deck = ctx.deck_json.replace("</", "<\\/");
    format!(
        concat!(
            "<base href=\"{base}\">\n",
            "<meta name=\"surface\" content=\"{sid}\">\n",
            "<meta name=\"version\" content=\"{ver}\">\n",
            "<link rel=\"stylesheet\" href=\"/suite/suite.css\">\n",
            "<script type=\"application/json\" id=\"gui-deck\">{deck}</script>\n",
            "<script type=\"module\" src=\"/suite/runtime.js\"></script>"
        ),
        base = esc(&ctx.base_url),
        sid = esc(&ctx.surface_id),
        ver = esc(&ctx.version),
        deck = deck,
    )
}

/// Case-insensitive substring search, returning the byte offset of the match.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.to_ascii_lowercase();
    h.find(&needle.to_ascii_lowercase())
}

/// Render an artifact's `index.html` into a full page with the suite wired in.
///
/// A full document (has `</head>`) gets the snippet injected before `</head>`. A bare
/// fragment of `<ui-*>` elements is wrapped in a minimal document.
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

    // Fragment (or full <html> without a head): wrap it.
    format!(
        concat!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n",
            "<meta charset=\"utf-8\">\n",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
            "<title>{title}</title>\n{snippet}\n</head>\n<body>\n{body}\n</body>\n</html>\n"
        ),
        title = esc(&ctx.title),
        snippet = snippet,
        body = html,
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

/// Minimal server-rendered inspector (Stage 7 rebuilds this with the suite + live SSE).
pub fn inspector_page(idx: &Index) -> String {
    let mut body = String::new();
    body.push_str("<h1>Generative UI Suite</h1>");
    body.push_str(&format!(
        "<p>{} agent(s), {} surface(s).</p>",
        idx.agents.len(),
        idx.surface_count()
    ));
    if idx.agents.is_empty() {
        body.push_str("<p>No surfaces discovered yet.</p>");
    }
    for agent in idx.agents.values() {
        body.push_str(&format!("<h2>{}</h2><ul>", esc(&agent.slug)));
        for deck in agent.decks.values() {
            body.push_str(&format!(
                "<li><strong>{}</strong> <a href=\"/{}/{}/\">deck</a><ul>",
                esc(&deck.slug),
                esc(&agent.slug),
                esc(&deck.slug)
            ));
            for surface in deck.ordered_surfaces() {
                let live = surface.live();
                body.push_str(&format!(
                    "<li><a href=\"{}\">{}</a> — v{} ({} version(s))</li>",
                    esc(&surface.path.url()),
                    esc(&live.manifest.title),
                    live.version,
                    surface.versions.len()
                ));
            }
            body.push_str("</ul></li>");
        }
        body.push_str("</ul>");
    }
    document("Inspector — Generative UI Suite", &body)
}

fn document(title: &str, body: &str) -> String {
    format!(
        concat!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n",
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
            "<title>{title}</title>\n</head>\n<body>\n{body}\n</body>\n</html>\n"
        ),
        title = esc(title),
        body = body,
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
        // Snippet lands before </head>, body untouched.
        let head_end = out.find("</head>").unwrap();
        assert!(out[..head_end].contains(r#"<meta name="version" content="100">"#));
        assert!(out.contains("<body>z</body>"));
        // Not double-wrapped.
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
}
