//! Stage 8: drive the MCP tool methods directly (no transport) and assert they mutate the
//! artifacts directory and surface backflow correctly.

use std::time::Duration;

use gui_agent::Workspace;
use gui_mcp::{
    DefineDeckParams, DeleteBackflowParams, DrainBackflowParams, GuiServer, ListBackflowParams,
    PublishParams, ReadSurfaceParams, SaveTemplateParams, ScaffoldParams, ScaffoldSurface,
    TemplateNameParams, WaitParams,
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
        html: Some("<ui-heading level=\"1\">Sales</ui-heading><ui-button data-action=\"refresh\">Refresh</ui-button>".into()),
        template: None,
        data_json: None,
        nav: Some(vec![gui_mcp::NavParam { to: "settings".into(), label: Some("Settings".into()) }]),
        handlers: None,
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
        html: Some("<ui-text>hi</ui-text>".into()),
        template: None,
        data_json: None,
        nav: None,
        handlers: None,
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

#[tokio::test]
async fn delete_and_drain_backflow_remove_event_files() {
    let (tmp, srv) = server();
    let path = gui_schema::SurfacePath::new("demo", "main", "sales").unwrap();
    let events_dir = path.events_dir(tmp.path());
    for (ms, action) in [(100u64, "refresh"), (200, "save"), (300, "refresh")] {
        let ev = serde_json::json!({
            "schema_version": "generative-ui/event.v1",
            "action": action,
            "surface": "demo/main/sales",
            "received_at": "t"
        });
        gui_schema::atomic_write(
            &events_dir.join(format!("{ms}-{action}.json")),
            ev.to_string().as_bytes(),
        )
        .unwrap();
    }

    // No filter → error, drain_backflow is the explicit delete-all.
    srv.delete_backflow(Parameters(DeleteBackflowParams {
        agent: "demo".into(),
        deck: None,
        surface: "sales".into(),
        file: None,
        action: None,
        since: None,
        until: None,
    }))
    .await
    .expect_err("empty filter must be rejected");

    // Delete by exact action.
    let deleted = srv
        .delete_backflow(Parameters(DeleteBackflowParams {
            agent: "demo".into(),
            deck: None,
            surface: "sales".into(),
            file: None,
            action: Some("refresh".into()),
            since: None,
            until: None,
        }))
        .await
        .unwrap();
    let text = result_text(&deleted);
    assert!(text.contains("100-refresh.json") && text.contains("300-refresh.json"), "got: {text}");
    assert!(!text.contains("200-save.json"), "got: {text}");
    assert!(events_dir.join("200-save.json").exists());

    // Delete by file id.
    srv.delete_backflow(Parameters(DeleteBackflowParams {
        agent: "demo".into(),
        deck: None,
        surface: "sales".into(),
        file: Some("200-save.json".into()),
        action: None,
        since: None,
        until: None,
    }))
    .await
    .unwrap();
    assert!(!events_dir.join("200-save.json").exists());

    // Drain removes whatever is left (including files list_backflow would skip).
    gui_schema::atomic_write(&events_dir.join("400-junk.json"), b"not json").unwrap();
    let drained = srv
        .drain_backflow(Parameters(DrainBackflowParams {
            agent: "demo".into(),
            deck: None,
            surface: "sales".into(),
        }))
        .await
        .unwrap();
    assert!(result_text(&drained).contains("400-junk.json"));
    assert_eq!(std::fs::read_dir(&events_dir).unwrap().count(), 0);
}

#[tokio::test]
async fn publish_surface_with_js_handler_writes_entry_and_manifest() {
    let (tmp, srv) = server();
    srv.publish_surface(Parameters(PublishParams {
        agent: "demo".into(),
        deck: None,
        surface: "counter".into(),
        title: "Counter".into(),
        alt_text: "a".into(),
        html: Some("<ui-button data-action=\"bump\">Bump</ui-button><span data-kv=\"hits\"></span>".into()),
        template: None,
        data_json: None,
        nav: None,
        handlers: Some(vec![gui_mcp::HandlerParam {
            action: "bump".into(),
            lang: "js".into(),
            entry: None,
            source: Some("function handle(e,h){ h.kv.set('hits',(h.kv.get('hits')||0)+1); return {hits:h.kv.get('hits')}; }".into()),
            source_base64: None,
            http_allow: None,
            fs: None,
            kv: Some(true),
            wall_ms: None,
            mem_mb: None,
        }]),
        metadata_json: None,
    }))
    .await
    .expect("publish with handler");

    let ws = Workspace::new(tmp.path());
    let path = gui_schema::SurfacePath::new("demo", "main", "counter").unwrap();
    let version = ws.live_version(&path).unwrap();
    let (manifest, _html) = ws.read_version(&path, version).unwrap();
    let def = &manifest.handlers["bump"];
    assert_eq!(def.entry, "main.js");
    assert!(def.capabilities.kv);
    // The entry file was written alongside the manifest.
    assert!(path.version_dir(tmp.path(), version).join("main.js").exists());
}

fn live_html(root: &std::path::Path, deck: &str, surface: &str) -> (gui_schema::Manifest, String) {
    let ws = Workspace::new(root);
    let path = gui_schema::SurfacePath::new("demo", deck, surface).unwrap();
    ws.read_version(&path, ws.live_version(&path).expect("published")).unwrap()
}

fn scaffold_surface(surface: &str, links: &[&str]) -> ScaffoldSurface {
    ScaffoldSurface {
        surface: surface.into(),
        title: surface.to_uppercase(),
        purpose: Some(format!("{surface} purpose")),
        template: None,
        data_json: None,
        links: Some(links.iter().map(|l| gui_mcp::NavParam { to: (*l).into(), label: None }).collect()),
        alt_text: None,
    }
}

#[tokio::test]
async fn scaffold_deck_publishes_linked_skeletons_and_wires_the_deck() {
    let (tmp, srv) = server();
    let mut settings = scaffold_surface("settings", &["home"]);
    settings.template = Some("form".into());
    settings.data_json = Some(r#"{"action":"save","fields":[{"name":"email","type":"email"}]}"#.into());
    srv.scaffold_deck(Parameters(ScaffoldParams {
        agent: "demo".into(),
        deck: Some("app".into()),
        name: Some("App".into()),
        entry: None,
        surfaces: vec![scaffold_surface("home", &["settings"]), settings],
    }))
    .await
    .expect("scaffold");

    let (manifest, html) = live_html(tmp.path(), "app", "home");
    assert_eq!(manifest.alt_text, "home purpose");
    assert_eq!(manifest.nav[0].to, "settings");
    assert!(html.contains("<ui-heading level=\"1\">HOME</ui-heading>"), "{html}");
    assert!(html.contains(r#"<ui-link to="settings">settings</ui-link>"#), "{html}");
    let (_, html) = live_html(tmp.path(), "app", "settings");
    assert!(html.contains(r#"<ui-form data-action="save">"#), "{html}");
    assert!(html.contains(r#"<ui-input name="email" label="email" type="email">"#), "{html}");

    let deck: gui_schema::DeckManifest =
        serde_json::from_slice(&std::fs::read(tmp.path().join("demo/app/deck.json")).unwrap()).unwrap();
    assert_eq!(deck.entry.as_deref(), Some("home"));
    assert_eq!(deck.order, ["home", "settings"]);

    // A bad template fails before anything is written.
    let mut bad = scaffold_surface("x", &[]);
    bad.template = Some("nope".into());
    assert!(srv
        .scaffold_deck(Parameters(ScaffoldParams {
            agent: "demo".into(),
            deck: Some("other".into()),
            name: None,
            entry: None,
            surfaces: vec![scaffold_surface("ok", &[]), bad],
        }))
        .await
        .is_err());
    assert!(!tmp.path().join("demo/other").exists());
}

#[tokio::test]
async fn publish_renders_a_template_and_escapes_slot_values() {
    let (tmp, srv) = server();
    let publish = |html: Option<&str>, template: Option<&str>| PublishParams {
        agent: "demo".into(),
        deck: None,
        surface: "items".into(),
        title: "Items <b>".into(),
        alt_text: "items".into(),
        html: html.map(Into::into),
        template: template.map(Into::into),
        data_json: Some(r#"{"items":[{"title":"<script>","action":"pick","value":{"id":"a'b"}}]}"#.into()),
        nav: None,
        handlers: None,
        metadata_json: None,
    };
    srv.publish_surface(Parameters(publish(None, Some("list")))).await.expect("publish");
    let (_, html) = live_html(tmp.path(), "main", "items");
    assert!(html.contains("Items &lt;b&gt;") && html.contains("&lt;script&gt;"), "{html}");
    assert!(!html.contains("a'b"), "value must not break out of the attribute: {html}");

    // Exactly one of html/template.
    assert!(srv.publish_surface(Parameters(publish(Some("x"), Some("list")))).await.is_err());
    assert!(srv.publish_surface(Parameters(publish(None, None))).await.is_err());
}

#[tokio::test]
async fn saved_templates_list_compose_and_shadow_builtins() {
    let (tmp, srv) = server();
    let srv = srv.with_templates_dir(tmp.path().join("templates"));
    let save = |name: &str, source: &str| {
        srv.save_template(Parameters(SaveTemplateParams { name: name.into(), source: source.into() }))
    };
    save("card", "{# A card. Slots: title, body #}<ui-card><ui-heading level=\"2\">{{ title }}</ui-heading>{% block body %}{% endblock %}</ui-card>{% include \"nav\" %}")
        .await
        .expect("save");
    save("note", "{% extends \"card\" %}{% block body %}<ui-text>{{ body }}</ui-text>{% endblock %}")
        .await
        .expect("save extending");
    assert!(save("Bad/Name", "x").await.is_err());
    assert!(save("broken", "{% if %}").await.is_err());

    let list = result_text(&srv.list_templates().await.unwrap());
    assert!(list.contains(r#"\"name\":\"card\""#) && list.contains("A card. Slots: title, body"), "{list}");
    assert!(list.contains(r#"\"name\":\"skeleton\""#), "{list}");

    srv.publish_surface(Parameters(PublishParams {
        agent: "demo".into(),
        deck: None,
        surface: "n".into(),
        title: "Note".into(),
        alt_text: "note".into(),
        html: None,
        template: Some("note".into()),
        data_json: Some(r#"{"body":"hello"}"#.into()),
        nav: Some(vec![gui_mcp::NavParam { to: "home".into(), label: Some("Home".into()) }]),
        handlers: None,
        metadata_json: None,
    }))
    .await
    .expect("publish custom");
    let (_, html) = live_html(tmp.path(), "main", "n");
    assert!(html.contains("<ui-heading level=\"2\">Note</ui-heading><ui-text>hello</ui-text>"), "{html}");
    assert!(html.contains(r#"<ui-link to="home">Home</ui-link>"#), "{html}");

    // A saved template shadows the built-in of the same name.
    save("skeleton", "custom {{ title }}").await.expect("shadow");
    let read = result_text(&srv.read_template(Parameters(TemplateNameParams { name: "skeleton".into() })).await.unwrap());
    assert!(read.contains("custom") && read.contains(r#"\"source\":\"custom\""#), "{read}");
}

/// Serialize a tool result to JSON for substring assertions (robust to the exact Content
/// representation across rmcp versions).
fn result_text(res: &rmcp::model::CallToolResult) -> String {
    serde_json::to_string(res).unwrap_or_default()
}
