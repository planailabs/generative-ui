//! End-to-end engine tests: drive the full host API through each built engine against a real
//! `Host` over a tempdir, plus shared limit/sandbox assertions. New engines (lua, wasm) plug
//! into the same `cases!` list as their cargo features are enabled, so all three stay in
//! lock-step.
// Helpers are exercised only when an engine feature is enabled; the bare build sees them as
// unused.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::TcpListener;

use gui_schema::{Capabilities, Event, HandlerDef, HandlerLang, Limits, EVENT_SCHEMA_VERSION};
use gui_runtime::{run_handler, HandlerError};
use serde_json::{json, Map, Value};

/// A handler source for one language, paired with the lang/entry filename.
struct Fixture {
    lang: HandlerLang,
    entry: &'static str,
    source: &'static str,
}

fn event(action: &str, value: Value) -> Event {
    Event {
        schema_version: EVENT_SCHEMA_VERSION.into(),
        action: action.into(),
        component: None,
        value,
        surface: "demo/main/s".into(),
        version: None,
        received_at: "t".into(),
    }
}

/// Run `source` (in `lang`) with the given caps/limits/kv against a fresh tempdir.
fn run(
    fx: &Fixture,
    caps: Capabilities,
    limits: Limits,
    ev: &Event,
    kv: Map<String, Value>,
) -> Result<gui_runtime::HandlerOutcome, HandlerError> {
    let tmp = tempfile::tempdir().unwrap();
    let vdir = tmp.path().join("v");
    let sdir = tmp.path().join("state");
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::create_dir_all(&sdir).unwrap();
    std::fs::write(vdir.join(fx.entry), fx.source).unwrap();
    let def = HandlerDef {
        lang: fx.lang,
        entry: fx.entry.into(),
        capabilities: caps,
        limits,
    };
    run_handler(&def, &vdir, &sdir, ev, kv)
}

/// A one-shot HTTP/1.1 stub returning `{"ok":true,"echo":<n>}`; returns its `127.0.0.1:port`.
fn http_stub() -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let body = br#"{"ok":true,"n":7}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.write_all(body);
        }
    });
    (format!("{}:{}", addr.ip(), addr.port()), handle)
}

// --- the full-host-API fixture per language --------------------------------

#[cfg(feature = "js")]
const JS_FULL: Fixture = Fixture {
    lang: HandlerLang::Js,
    entry: "main.js",
    source: r#"
function handle(event, host) {
  // kv fold
  host.kv.set("hits", (host.kv.get("hits") || 0) + 1);
  // fs round-trip inside the jail
  host.fs.write("note.txt", "v=" + event.value.n);
  const note = host.fs.read("note.txt");
  // http via allowlist
  const r = host.http.fetch({ url: event.value.url });
  host.log("info", "done");
  return { hits: host.kv.get("hits"), note: note, status: r.status, bodylen: r.body.length };
}
"#,
};

#[cfg(feature = "js")]
const JS_LOOP: Fixture = Fixture {
    lang: HandlerLang::Js,
    entry: "main.js",
    source: "function handle(e,h){ while(true){} }",
};

#[cfg(feature = "js")]
const JS_COUNTER: Fixture = Fixture {
    lang: HandlerLang::Js,
    entry: "main.js",
    source: "function handle(e,h){ h.kv.set('hits',(h.kv.get('hits')||0)+1); return {hits:h.kv.get('hits')}; }",
};

/// The full host API works through the engine: kv fold + jailed fs + allowlisted http + log.
fn assert_full_api(fx: &Fixture) {
    let (addr, stub) = http_stub();
    let host = addr.split(':').next().unwrap().to_string();
    let caps = Capabilities {
        http: vec![host],
        fs: true,
        kv: true,
    };
    let ev = event("go", json!({ "n": 7, "url": format!("http://{addr}/") }));
    let out = run(fx, caps, Limits::default(), &ev, Map::new()).expect("handler ok");
    stub.join().ok();

    assert_eq!(out.response.status, 200);
    assert_eq!(out.response.body["hits"], json!(1));
    assert_eq!(out.response.body["note"], json!("v=7"));
    assert_eq!(out.response.body["status"], json!(200), "http round-trip status");
    assert!(
        out.response.body["bodylen"].as_u64().unwrap() > 0,
        "http response body delivered to guest"
    );
    // kv folded + flagged dirty for persistence.
    assert!(out.kv_changed);
    assert_eq!(out.kv["hits"], json!(1));
}

/// Folding accumulates across invocations (replay property): feeding the prior kv back in.
fn assert_fold_accumulates(fx: &Fixture) {
    let caps = Capabilities { kv: true, ..Default::default() };
    let ev = event("bump", json!(null));
    let mut kv = Map::new();
    for expected in 1..=3 {
        let out = run(fx, caps.clone(), Limits::default(), &ev, kv).unwrap();
        assert_eq!(out.response.body["hits"], json!(expected));
        kv = out.kv;
    }
}

/// An infinite loop is killed at the wall-clock deadline.
fn assert_wall_clock_abort(fx: &Fixture) {
    let limits = Limits { wall_ms: 200, ..Default::default() };
    let ev = event("go", json!(null));
    let err = run(fx, Capabilities::default(), limits, &ev, Map::new()).unwrap_err();
    assert!(
        matches!(err, HandlerError::Timeout(_) | HandlerError::Guest(_)),
        "expected timeout/abort, got {err:?}"
    );
}

#[cfg(feature = "lua")]
const LUA_FULL: Fixture = Fixture {
    lang: HandlerLang::Lua,
    entry: "main.lua",
    source: r#"
return function(event, host)
  host.kv.set("hits", (host.kv.get("hits") or 0) + 1)
  host.fs.write("note.txt", "v=" .. tostring(event.value.n))
  local note = host.fs.read("note.txt")
  local r = host.http.fetch({ url = event.value.url })
  host.log("info", "done")
  return { hits = host.kv.get("hits"), note = note, status = r.status, bodylen = #r.body }
end
"#,
};

#[cfg(feature = "lua")]
const LUA_LOOP: Fixture = Fixture {
    lang: HandlerLang::Lua,
    entry: "main.lua",
    source: "return function(e, h) while true do end end",
};

#[cfg(feature = "lua")]
const LUA_COUNTER: Fixture = Fixture {
    lang: HandlerLang::Lua,
    entry: "main.lua",
    source: "return function(e, h) h.kv.set('hits', (h.kv.get('hits') or 0) + 1); return { hits = h.kv.get('hits') } end",
};

#[cfg(feature = "js")]
mod js {
    use super::*;

    #[test]
    fn full_host_api() {
        assert_full_api(&JS_FULL);
    }

    #[test]
    fn fold_accumulates() {
        assert_fold_accumulates(&JS_COUNTER);
    }

    #[test]
    fn wall_clock_abort() {
        assert_wall_clock_abort(&JS_LOOP);
    }

    #[test]
    fn explicit_response_with_status_code() {
        const FX: Fixture = Fixture {
            lang: HandlerLang::Js,
            entry: "main.js",
            source: "function handle(e,h){ h.response(h.responseCode.NOT_FOUND, {error:'nope'}); return {ignored:true}; }",
        };
        let ev = event("go", json!(null));
        let out = run(&FX, Capabilities::default(), Limits::default(), &ev, Map::new()).unwrap();
        assert_eq!(out.response.status, 404, "host.responseCode.NOT_FOUND from the http crate");
        assert_eq!(out.response.body, json!({ "error": "nope" }), "explicit response overrides return");
    }

    #[test]
    fn undeclared_capability_is_denied() {
        // No kv capability granted → host.kv.set throws → handler errors.
        let ev = event("go", json!(null));
        let err = run(&JS_COUNTER, Capabilities::default(), Limits::default(), &ev, Map::new())
            .unwrap_err();
        assert!(matches!(err, HandlerError::Guest(_)), "got {err:?}");
    }
}

#[cfg(feature = "lua")]
mod lua {
    use super::*;

    #[test]
    fn full_host_api() {
        assert_full_api(&LUA_FULL);
    }

    #[test]
    fn fold_accumulates() {
        assert_fold_accumulates(&LUA_COUNTER);
    }

    #[test]
    fn wall_clock_abort() {
        assert_wall_clock_abort(&LUA_LOOP);
    }

    #[test]
    fn explicit_response_with_status_code() {
        const FX: Fixture = Fixture {
            lang: HandlerLang::Lua,
            entry: "main.lua",
            source: "return function(e, h) h.response(h.responseCode.NOT_FOUND, { error = 'nope' }); return { ignored = true } end",
        };
        let ev = event("go", json!(null));
        let out = run(&FX, Capabilities::default(), Limits::default(), &ev, Map::new()).unwrap();
        assert_eq!(out.response.status, 404, "host.responseCode.NOT_FOUND from the http crate");
        assert_eq!(out.response.body, json!({ "error": "nope" }));
    }

    #[test]
    fn undeclared_capability_is_denied() {
        let ev = event("go", json!(null));
        let err = run(&LUA_COUNTER, Capabilities::default(), Limits::default(), &ev, Map::new())
            .unwrap_err();
        assert!(matches!(err, HandlerError::Guest(_)), "got {err:?}");
    }
}
