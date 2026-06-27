//! `gui-server` binary. Stage 2: scan the artifacts dir and print the index.
//! Stage 3 replaces this with the axum HTTP server.

use std::path::Path;

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| "artifacts".to_string());
    let idx = gui_server::Index::build(Path::new(&root));

    for w in &idx.warnings {
        eprintln!("warning: {w}");
    }
    println!(
        "discovered {} agent(s), {} surface(s) under {root}",
        idx.agents.len(),
        idx.surface_count()
    );
    for agent in idx.agents.values() {
        for deck in agent.decks.values() {
            for surface in deck.surfaces.values() {
                println!(
                    "  {}/{}/{}  live=v{} ({} version(s))",
                    agent.slug,
                    deck.slug,
                    surface.path.surface,
                    surface.live().version,
                    surface.versions.len()
                );
            }
        }
    }
}
