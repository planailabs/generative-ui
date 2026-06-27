//! The swiftide agent authors an app through the in-process MCP server. Driven by a
//! deterministic mock LLM (which issues a `gui:publish_surface` tool call), so the full
//! agent → MCP → published-surface flow runs offline.

use std::time::{Duration, Instant};

use gui_agent::Workspace;
use gui_schema::SurfacePath;

#[tokio::test]
async fn agent_authors_app_via_mcp() {
    let tmp = tempfile::tempdir().unwrap();
    let artifacts = tmp.path().to_path_buf();

    // The agent (mock LLM) builds the app by calling the MCP publish_surface tool.
    direct_access::run_agent(direct_access::MockLlm, artifacts.clone(), "Build a home screen")
        .await
        .expect("agent run");

    // The MCP server wrote the surface into the directory — assert it exists with the
    // agent-composed content.
    let ws = Workspace::new(&artifacts);
    let path = SurfacePath::new("app", "main", "home").unwrap();

    // The publish happens via a spawned in-process server task; allow a brief moment.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && ws.live_version(&path).is_none() {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let version = ws.live_version(&path).expect("agent published the home surface via MCP");
    let (manifest, html) = ws.read_version(&path, version).unwrap();
    assert_eq!(manifest.title, "Welcome");
    assert!(html.contains("<ui-heading"));
    assert!(html.contains(r#"<ui-button data-action="start""#));
}
