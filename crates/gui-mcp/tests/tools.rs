//! Stage 8: drive the MCP tool methods directly (no transport) and assert they mutate the
//! artifacts directory and surface backflow correctly.

use std::time::Duration;

use gui_agent::Workspace;
use gui_mcp::{
    DefineDeckParams, GuiServer, ListBackflowParams, PublishParams, ReadSurfaceParams, WaitParams,
};
use rmcp::handler::server::wrapper::Parameters;

fn server() -> (tempfile::TempDir, GuiServer) {
    let tmp = tempfile::tempdir().unwrap();
    let srv = GuiServer::new(Workspace::new(tmp.path()));
    (tmp, srv)
}

#[tokio::test]
async fn publish_and_define_deck_write_the_directory() {
    let (tmp, srv) = server();

    srv.publish_surface(Parameters(PublishParams {
        agent: "demo".into(),
        deck: None,
        surface: "sales".into(),
        title: "Sales".into(),
        alt_text: "Revenue".into(),
        html: "<ui-heading level=\"1\">Sales</ui-heading><ui-button data-action=\"refresh\">Refresh</ui-button>".into(),
        nav: Some(vec![gui_mcp::NavParam { to: "settings".into(), label: Some("Settings".into()) }]),
        metadata_json: None,
    }))
    .await
    .expect("publish");

    // A version directory now exists with frame.json + index.html composed from components.
    let ws = Workspace::new(tmp.path());
    let path = gui_schema::SurfacePath::new("demo", "main", "sales").unwrap();
    let version = ws.live_version(&path).expect("surface published");
    let (manifest, html) = ws.read_version(&path, version).unwrap();
    assert_eq!(manifest.title, "Sales");
    assert_eq!(manifest.nav[0].to, "settings");
    assert!(html.contains(r#"<ui-button data-action="refresh">"#));

    srv.define_deck(Parameters(DefineDeckParams {
        agent: "demo".into(),
        deck: "main".into(),
        entry: Some("sales".into()),
        order: Some(vec!["sales".into()]),
        name: Some("Demo".into()),
    }))
    .await
    .expect("define_deck");
    assert!(tmp.path().join("demo/main/deck.json").exists());

    // read_surface returns the live manifest + html.
    let read = srv
        .read_surface(Parameters(ReadSurfaceParams {
            agent: "demo".into(),
            deck: None,
            surface: "sales".into(),
        }))
        .await
        .expect("read_surface");
    let text = result_text(&read);
    assert!(text.contains("Sales"));
}

#[tokio::test]
async fn wait_for_backflow_returns_a_written_event() {
    let (tmp, srv) = server();
    // Publish so the surface exists.
    srv.publish_surface(Parameters(PublishParams {
        agent: "demo".into(),
        deck: None,
        surface: "sales".into(),
        title: "Sales".into(),
        alt_text: "alt".into(),
        html: "<ui-text>hi</ui-text>".into(),
        nav: None,
        metadata_json: None,
    }))
    .await
    .unwrap();

    // Simulate a user interaction landing in the directory shortly after.
    let path = gui_schema::SurfacePath::new("demo", "main", "sales").unwrap();
    let events_dir = path.events_dir(tmp.path());
    let writer = events_dir.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        let ev = serde_json::json!({
            "schema_version": "generative-ui/event.v1",
            "action": "refresh",
            "surface": "demo/main/sales",
            "received_at": "t"
        });
        gui_schema::atomic_write(
            &writer.join("99999999999999-refresh.json"),
            ev.to_string().as_bytes(),
        )
        .unwrap();
    });

    let res = srv
        .wait_for_backflow(Parameters(WaitParams {
            agent: "demo".into(),
            deck: None,
            surface: "sales".into(),
            since: Some(0),
            timeout_ms: Some(5000),
        }))
        .await
        .expect("wait");
    let text = result_text(&res);
    assert!(text.contains("received"), "got: {text}");
    assert!(text.contains("refresh"), "got: {text}");

    // list_backflow sees it too.
    let listed = srv
        .list_backflow(Parameters(ListBackflowParams {
            agent: "demo".into(),
            deck: None,
            surface: "sales".into(),
            since: None,
        }))
        .await
        .unwrap();
    assert!(result_text(&listed).contains("refresh"));
}

/// Serialize a tool result to JSON for substring assertions (robust to the exact Content
/// representation across rmcp versions).
fn result_text(res: &rmcp::model::CallToolResult) -> String {
    serde_json::to_string(res).unwrap_or_default()
}
