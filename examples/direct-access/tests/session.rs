//! The full create → wait → respond loop, driven in-process by a swiftide agent with a
//! deterministic mock LLM — no network, no transport.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gui_agent::Workspace;
use gui_schema::{atomic_write, Event, SurfacePath, EVENT_SCHEMA_VERSION};

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

async fn wait_until(mut cond: impl FnMut() -> bool, timeout_ms: u64) {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not met within {timeout_ms}ms");
}

#[tokio::test]
async fn full_loop_create_wait_respond_with_mock_llm() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = Workspace::new(tmp.path());
    let path =
        SurfacePath::new(direct_access::AGENT, direct_access::DECK, direct_access::SURFACE).unwrap();

    // Run the agent session loop in the background with the deterministic mock LLM.
    let task = tokio::spawn(direct_access::run_session(ws.clone(), direct_access::MockLlm));

    // 1) CREATE: the agent publishes the chat surface.
    {
        let ws = ws.clone();
        let path = path.clone();
        wait_until(move || ws.live_version(&path).is_some(), 10_000).await;
    }

    // 2) The user submits a prompt — backflow lands in the directory.
    let ev = Event {
        schema_version: EVENT_SCHEMA_VERSION.into(),
        action: "ask".into(),
        component: Some("ui-form".into()),
        value: serde_json::json!({ "prompt": "hello there" }),
        surface: path.id(),
        version: None,
        received_at: "t".into(),
    };
    let ms = now_ms() + 2_000;
    atomic_write(
        &path.events_dir(tmp.path()).join(format!("{ms}-ask.json")),
        &serde_json::to_vec(&ev).unwrap(),
    )
    .unwrap();

    // 3) RESPOND: the agent asks the (mock) LLM and republishes with the answer.
    {
        let ws = ws.clone();
        let path = path.clone();
        wait_until(
            move || {
                ws.live_version(&path)
                    .and_then(|v| ws.read_version(&path, v).ok())
                    .map(|(_, html)| html.contains("You said: hello there"))
                    .unwrap_or(false)
            },
            10_000,
        )
        .await;
    }

    task.abort();
}
