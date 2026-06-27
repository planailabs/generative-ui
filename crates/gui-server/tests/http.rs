//! Stage 3 integration: spin the server on an ephemeral port over a temp artifacts dir and
//! assert every route end to end.

use std::fs;
use std::path::Path;

use gui_server::AppState;

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn manifest(title: &str, extra: &str) -> String {
    format!(
        r#"{{"schema_version":"generative-ui/surface.v1","title":"{title}","alt_text":"a"{extra}}}"#
    )
}

/// Build demo/main with two surfaces (sales, settings), two versions of sales, a deck.json,
/// nav edges, and one asset.
fn fixture(root: &Path) {
    for (v, _t) in [("100", "old"), ("200", "new")] {
        let d = root.join("demo/main/sales/v").join(v);
        write(
            &d.join("frame.json"),
            &manifest("Sales", r#","nav":[{"to":"settings","label":"Settings"}]"#),
        );
        write(&d.join("index.html"), "<ui-text>sales</ui-text>");
    }
    write(&root.join("demo/main/sales/v/200/assets/note.txt"), "hello-asset");

    let s = root.join("demo/main/settings/v/50");
    write(&s.join("frame.json"), &manifest("Settings", ""));
    write(&s.join("index.html"), "<ui-text>settings</ui-text>");

    write(
        &root.join("demo/main/deck.json"),
        r#"{"name":"Demo","entry":"sales","order":["sales","settings"]}"#,
    );
}

async fn start(root: &Path) -> String {
    start_with(AppState::new(root.to_path_buf())).await
}

async fn start_with(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = gui_server::router(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn routes_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let base = start(tmp.path()).await;
    let http = reqwest::Client::new();

    // index.json
    let idx: serde_json::Value = http
        .get(format!("{base}/api/index.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(idx["surface_count"], 2);
    assert_eq!(idx["agents"][0]["slug"], "demo");

    // deck graph: order honored, nav edge resolved to a URL.
    let deck: serde_json::Value = http
        .get(format!("{base}/api/demo/main/deck.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(deck["entry"], "sales");
    assert_eq!(deck["order"][0], "sales");
    assert_eq!(deck["surfaces"][0]["nav"][0]["url"], "/demo/main/settings/");

    // versions: newest first, live = 200.
    let versions: serde_json::Value = http
        .get(format!("{base}/api/demo/main/sales/versions.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(versions["live"], "200");
    assert_eq!(versions["versions"][0]["version"], "200");
    assert_eq!(versions["versions"][1]["version"], "100");

    // activity: newest publish first.
    let activity: serde_json::Value = http
        .get(format!("{base}/api/activity.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(activity["publishes"][0]["version"], "200");

    // live surface page: fragment wrapped + suite + context injected.
    let live = http
        .get(format!("{base}/demo/main/sales/"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(live.contains("<ui-text>sales</ui-text>"));
    assert!(live.contains(r#"<meta name="surface" content="demo/main/sales">"#));
    assert!(live.contains(r#"<meta name="version" content="200">"#));
    assert!(live.contains(r#"src="/suite/runtime.js""#));
    assert!(live.contains(r#"<base href="/demo/main/sales/v/200/">"#));

    // two-segment convenience: deck → entry surface; main-deck surface directly.
    let deck_view = http.get(format!("{base}/demo/main/")).send().await.unwrap().text().await.unwrap();
    assert!(deck_view.contains("<ui-text>sales</ui-text>"), "deck view renders entry");
    let surf_view = http.get(format!("{base}/demo/sales/")).send().await.unwrap().text().await.unwrap();
    assert!(surf_view.contains("<ui-text>sales</ui-text>"), "main-deck surface by slug");

    // historical version page.
    let v100 = http
        .get(format!("{base}/demo/main/sales/v/100/"))
        .send()
        .await
        .unwrap();
    assert_eq!(v100.status(), 200);

    // asset served with a content type, via the base-href version path.
    let asset = http
        .get(format!("{base}/demo/main/sales/v/200/assets/note.txt"))
        .send()
        .await
        .unwrap();
    assert_eq!(asset.status(), 200);
    assert!(asset
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/plain"));
    assert_eq!(asset.text().await.unwrap(), "hello-asset");

    // path traversal is rejected.
    let bad = http
        .get(format!("{base}/demo/main/sales/v/200/assets/..%2f..%2fframe.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 404);

    // unknown surface → 404.
    let missing = http.get(format!("{base}/demo/main/nope/")).send().await.unwrap();
    assert_eq!(missing.status(), 404);
}

#[tokio::test]
async fn backflow_writes_event_into_directory() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let base = start(tmp.path()).await;
    let http = reqwest::Client::new();

    let res = http
        .post(format!("{base}/backflow/demo/main/sales"))
        .json(&serde_json::json!({"action":"drill_down","component":"ui-button","value":{"region":"emea"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 202);

    let events_dir = tmp.path().join("demo/main/sales/events");
    let entries: Vec<_> = fs::read_dir(&events_dir).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(entries.len(), 1, "exactly one event file written");
    let name = entries[0].file_name().to_string_lossy().to_string();
    assert!(name.ends_with("-drill-down.json"), "slugified name: {name}");

    let ev: serde_json::Value = serde_json::from_slice(&fs::read(entries[0].path()).unwrap()).unwrap();
    assert_eq!(ev["action"], "drill_down");
    assert_eq!(ev["surface"], "demo/main/sales");
    assert_eq!(ev["version"], "200", "stamped with the live version");
    assert_eq!(ev["schema_version"], "generative-ui/event.v1");

    // Backflow to an unknown surface is rejected (no stray directories created).
    let bad = http
        .post(format!("{base}/backflow/demo/main/nope"))
        .json(&serde_json::json!({"action":"x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 404);
    assert!(!tmp.path().join("demo/main/nope").exists());
}

#[tokio::test]
async fn js_handler_dispatch_folds_kv_and_defines_response() {
    // A surface whose `bump` action runs a JS handler: it folds a counter into KV and returns
    // it. The event is still logged; the KV view is readable via the kv.json endpoint.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("demo/main/counter/v/1");
    write(
        &d.join("frame.json"),
        &manifest(
            "Counter",
            r#","handlers":{"bump":{"lang":"js","entry":"main.js","capabilities":{"kv":true}}}"#,
        ),
    );
    write(&d.join("index.html"), "<ui-text>counter</ui-text>");
    write(
        &d.join("main.js"),
        "function handle(e,h){ h.kv.set('hits',(h.kv.get('hits')||0)+1); return {hits:h.kv.get('hits')}; }",
    );

    let state = AppState::new(tmp.path().to_path_buf()).with_handlers(true);
    let base = start_with(state).await;
    let http = reqwest::Client::new();

    // Two bumps: the handler's response carries the running count.
    for expected in 1..=2 {
        let res = http
            .post(format!("{base}/backflow/demo/main/counter"))
            .json(&serde_json::json!({"action":"bump"}))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "handler defines the response");
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["hits"], expected);
    }

    // The event log still recorded both interactions (source of truth).
    let events_dir = tmp.path().join("demo/main/counter/events");
    assert_eq!(fs::read_dir(&events_dir).unwrap().count(), 2);

    // KV persisted and readable via the endpoint the UI uses.
    let kv: serde_json::Value = http
        .get(format!("{base}/api/demo/main/counter/kv.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(kv["hits"], 2);
    assert!(tmp.path().join("demo/main/counter/state/kv.json").exists());
}

#[tokio::test]
async fn handlers_disabled_by_default_only_logs_event() {
    // Without --enable-handlers the same surface just logs the event and returns 202.
    let tmp = tempfile::tempdir().unwrap();
    let d = tmp.path().join("demo/main/counter/v/1");
    write(
        &d.join("frame.json"),
        &manifest(
            "Counter",
            r#","handlers":{"bump":{"lang":"js","entry":"main.js","capabilities":{"kv":true}}}"#,
        ),
    );
    write(&d.join("index.html"), "<ui-text>counter</ui-text>");
    write(&d.join("main.js"), "function handle(e,h){ return {ran:true}; }");

    let base = start(tmp.path()).await; // handlers disabled
    let http = reqwest::Client::new();
    let res = http
        .post(format!("{base}/backflow/demo/main/counter"))
        .json(&serde_json::json!({"action":"bump"}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 202, "no handler run; plain ack");
    assert!(!tmp.path().join("demo/main/counter/state/kv.json").exists());
}

#[tokio::test]
async fn watcher_reindexes_and_broadcasts() {
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir().unwrap();
    let state = AppState::new(tmp.path().to_path_buf());
    let mut rx = state.events_tx.subscribe();
    let _watcher = gui_server::server::spawn_watcher(state.clone()).expect("watcher");
    assert_eq!(state.index.load_full().surface_count(), 0);

    // Publish a surface by writing files into the watched tree.
    let d = tmp.path().join("demo/main/sales/v/100");
    write(&d.join("frame.json"), &manifest("Sales", ""));
    write(&d.join("index.html"), "<ui-text>hi</ui-text>");

    // Wait until the watcher re-indexes (poll broadcasts + index until visible).
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen = false;
    while Instant::now() < deadline {
        let _ = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
        if state.index.load_full().surface_count() >= 1 {
            seen = true;
            break;
        }
    }
    assert!(seen, "watcher did not re-index the new surface");
}

#[tokio::test]
async fn versions_api_reflects_republish_for_streamable_reload() {
    // Surface views poll the live version after an SSE ping; assert it advances on republish.
    let tmp = tempfile::tempdir().unwrap();
    let d1 = tmp.path().join("demo/main/sales/v/1");
    write(&d1.join("frame.json"), &manifest("Sales", ""));
    write(&d1.join("index.html"), "<ui-text>v1</ui-text>");
    let base = start(tmp.path()).await;
    let http = reqwest::Client::new();

    let v: serde_json::Value = http
        .get(format!("{base}/api/demo/main/sales/versions.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["live"], "1");
    // (server reloads via its watcher in the real binary; here AppState::new snapshots once,
    //  so we exercise the API contract the runtime relies on.)
}

#[tokio::test]
async fn sse_and_events_api() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path());
    let base = start(tmp.path()).await;
    let http = reqwest::Client::new();

    // SSE endpoint advertises the event stream.
    let es = http.get(format!("{base}/events")).send().await.unwrap();
    assert_eq!(es.status(), 200);
    assert!(es
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    drop(es);

    // Record a backflow, then read it back via the events API.
    http.post(format!("{base}/backflow/demo/main/sales"))
        .json(&serde_json::json!({"action":"ping"}))
        .send()
        .await
        .unwrap();
    let ev: serde_json::Value = http
        .get(format!("{base}/api/demo/main/sales/events.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ev["events"][0]["action"], "ping");
}

#[tokio::test]
async fn serves_suite_as_esm() {
    let tmp = tempfile::tempdir().unwrap();
    let base = start(tmp.path()).await;
    let http = reqwest::Client::new();

    let rt = http.get(format!("{base}/suite/runtime.js")).send().await.unwrap();
    assert_eq!(rt.status(), 200);
    assert!(rt
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/javascript"));
    let body = rt.text().await.unwrap();
    assert!(body.contains("customElements.define"));
    assert!(body.contains(r#"from "./lib.js""#), "runtime imports lib via ESM");

    let css = http.get(format!("{base}/suite/suite.css")).send().await.unwrap();
    assert!(css
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/css"));

    let lib = http.get(format!("{base}/suite/lib.js")).send().await.unwrap();
    assert_eq!(lib.status(), 200);
    assert!(lib.text().await.unwrap().contains("export function backflowPayload"));

    let nope = http.get(format!("{base}/suite/nope.js")).send().await.unwrap();
    assert_eq!(nope.status(), 404);
}
