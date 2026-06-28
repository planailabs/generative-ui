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
    source: &'static [u8],
}

fn event(action: &str, value: Value) -> Event {
    Event {
        schema_version: EVENT_SCHEMA_VERSION.into(),
        action: action.into(),
        component: None,
        value,
        surface: "demo/main/s".into(),
        version: None,
        scope: gui_schema::Scope::Public,
        source: None,
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
    // Seed the public KV; tests only exercise public scope.
    run_handler(&def, &vdir, &sdir, ev, kv, Map::new())
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
    source: br#"
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
    source: b"function handle(e,h){ while(true){} }",
};

#[cfg(feature = "js")]
const JS_COUNTER: Fixture = Fixture {
    lang: HandlerLang::Js,
    entry: "main.js",
    source: b"function handle(e,h){ h.kv.set('hits',(h.kv.get('hits')||0)+1); return {hits:h.kv.get('hits')}; }",
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
    assert!(out.public.changed);
    assert_eq!(out.public.map["hits"], json!(1));
}

/// Folding accumulates across invocations (replay property): feeding the prior kv back in.
fn assert_fold_accumulates(fx: &Fixture) {
    let caps = Capabilities { kv: true, ..Default::default() };
    let ev = event("bump", json!(null));
    let mut kv = Map::new();
    for expected in 1..=3 {
        let out = run(fx, caps.clone(), Limits::default(), &ev, kv).unwrap();
        assert_eq!(out.response.body["hits"], json!(expected));
        kv = out.public.map;
    }
}

/// A handler can emit an event (for the agent) and write private KV; the public view is
/// untouched and the private view is flagged for persistence.
fn assert_emit_and_private(fx: &Fixture) {
    let caps = Capabilities { kv: true, ..Default::default() };
    let ev = event("emitdemo", json!(null));
    let out = run(fx, caps, Limits::default(), &ev, Map::new()).unwrap();
    assert_eq!(out.emitted.len(), 1, "one event emitted");
    assert_eq!(out.emitted[0].name, "noted");
    assert_eq!(out.emitted[0].scope, gui_schema::Scope::Public);
    assert_eq!(out.private.map["secret"], json!(1), "private kv set");
    assert!(out.private.changed, "private flagged dirty");
    assert!(!out.public.changed, "public untouched");
}

/// The ungated `crypto.*` surface works through the engine (no capability required): known-answer
/// hash + hmac, a self-verified signature, and a well-formed uuid.
fn assert_crypto(fx: &Fixture) {
    let ev = event("go", json!(null));
    let out = run(fx, Capabilities::default(), Limits::default(), &ev, Map::new()).expect("crypto ok");
    assert_eq!(
        out.response.body["sha256"],
        json!("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        "SHA-256(\"abc\")"
    );
    assert_eq!(
        out.response.body["hmac"],
        json!("f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"),
        "HMAC-SHA256 test vector"
    );
    assert_eq!(out.response.body["verified"], json!(true), "verifyHmac round-trip");
    assert_eq!(out.response.body["uuidlen"], json!(36), "uuid length");
    // Public-key + ECDH facades round-trip through the engine.
    assert_eq!(out.response.body["ecdsaOk"], json!(true), "ecdsa keypair+sign+verify");
    assert_eq!(out.response.body["dhAgrees"], json!(true), "x25519 shared secret agrees");
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
    source: br#"
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

#[cfg(feature = "js")]
const JS_CRYPTO: Fixture = Fixture {
    lang: HandlerLang::Js,
    entry: "main.js",
    source: br#"
function handle(e, h) {
  var msg = "The quick brown fox jumps over the lazy dog";
  var mac = h.crypto.hmac("sha256", "key", msg);
  var ec = h.crypto.ecdsa.keypair("p256");
  var sig = h.crypto.ecdsa.sign(ec.privateKey, "m", { curve: "p256" });
  var a = h.crypto.x25519.keypair();
  var b = h.crypto.x25519.keypair();
  return {
    sha256: h.crypto.hash("sha256", "abc"),
    hmac: mac,
    verified: h.crypto.verifyHmac("sha256", "key", msg, mac),
    uuidlen: h.crypto.uuid().length,
    ecdsaOk: h.crypto.ecdsa.verify(ec.publicKey, "m", sig, { curve: "p256" }),
    dhAgrees: h.crypto.x25519.deriveShared(a.privateKey, b.publicKey) === h.crypto.x25519.deriveShared(b.privateKey, a.publicKey),
  };
}
"#,
};

#[cfg(feature = "lua")]
const LUA_CRYPTO: Fixture = Fixture {
    lang: HandlerLang::Lua,
    entry: "main.lua",
    source: br#"
return function(e, h)
  local msg = "The quick brown fox jumps over the lazy dog"
  local mac = h.crypto.hmac("sha256", "key", msg)
  local ec = h.crypto.ecdsa.keypair("p256")
  local sig = h.crypto.ecdsa.sign(ec.privateKey, "m", { curve = "p256" })
  local a = h.crypto.x25519.keypair()
  local b = h.crypto.x25519.keypair()
  return {
    sha256 = h.crypto.hash("sha256", "abc"),
    hmac = mac,
    verified = h.crypto.verifyHmac("sha256", "key", msg, mac),
    uuidlen = #h.crypto.uuid(),
    ecdsaOk = h.crypto.ecdsa.verify(ec.publicKey, "m", sig, { curve = "p256" }),
    dhAgrees = h.crypto.x25519.deriveShared(a.privateKey, b.publicKey) == h.crypto.x25519.deriveShared(b.privateKey, a.publicKey),
  }
end
"#,
};

#[cfg(feature = "lua")]
const LUA_LOOP: Fixture = Fixture {
    lang: HandlerLang::Lua,
    entry: "main.lua",
    source: b"return function(e, h) while true do end end",
};

#[cfg(feature = "lua")]
const LUA_COUNTER: Fixture = Fixture {
    lang: HandlerLang::Lua,
    entry: "main.lua",
    source: b"return function(e, h) h.kv.set('hits', (h.kv.get('hits') or 0) + 1); return { hits = h.kv.get('hits') } end",
};

#[cfg(feature = "js")]
const JS_EMIT: Fixture = Fixture {
    lang: HandlerLang::Js,
    entry: "main.js",
    source: b"function handle(e,h){ h.emit('noted',{n:1}); h.kvPrivate.set('secret',1); return {ok:true}; }",
};

#[cfg(feature = "lua")]
const LUA_EMIT: Fixture = Fixture {
    lang: HandlerLang::Lua,
    entry: "main.lua",
    source: b"return function(e,h) h.emit('noted',{n=1}); h.kvPrivate.set('secret',1); return {ok=true} end",
};

#[cfg(feature = "js")]
mod js {
    use super::*;

    #[test]
    fn emit_and_private() {
        assert_emit_and_private(&JS_EMIT);
    }

    #[test]
    fn full_host_api() {
        assert_full_api(&JS_FULL);
    }

    #[test]
    fn fold_accumulates() {
        assert_fold_accumulates(&JS_COUNTER);
    }

    #[test]
    fn crypto() {
        assert_crypto(&JS_CRYPTO);
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
            source: b"function handle(e,h){ h.response(h.responseCode.NOT_FOUND, {error:'nope'}); return {ignored:true}; }",
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
    fn emit_and_private() {
        assert_emit_and_private(&LUA_EMIT);
    }

    #[test]
    fn fold_accumulates() {
        assert_fold_accumulates(&LUA_COUNTER);
    }

    #[test]
    fn crypto() {
        assert_crypto(&LUA_CRYPTO);
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
            source: b"return function(e, h) h.response(h.responseCode.NOT_FOUND, { error = 'nope' }); return { ignored = true } end",
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

#[cfg(feature = "wasm")]
const WASM_HANDLER: Fixture = Fixture {
    lang: HandlerLang::Wasm,
    entry: "main.wasm",
    // Prebuilt guest (examples/handler-wasm) committed so no wasm toolchain is needed to test.
    source: include_bytes!("fixtures/handler.wasm"),
};

#[cfg(feature = "wasm")]
mod wasm {
    use super::*;

    #[test]
    fn full_host_api() {
        assert_full_api(&WASM_HANDLER);
    }

    #[test]
    fn emit_and_private() {
        assert_emit_and_private(&WASM_HANDLER);
    }

    #[test]
    fn fold_accumulates() {
        assert_fold_accumulates(&WASM_HANDLER);
    }

    #[test]
    fn wall_clock_abort() {
        assert_wall_clock_abort(&WASM_HANDLER);
    }

    #[test]
    fn explicit_response_with_status_code() {
        let ev = event("respond", json!(null));
        let out = run(&WASM_HANDLER, Capabilities::default(), Limits::default(), &ev, Map::new()).unwrap();
        assert_eq!(out.response.status, 404, "host.responseCode.NOT_FOUND via status_codes import");
        assert_eq!(out.response.body, json!({ "error": "nope" }));
    }

    #[test]
    fn undeclared_capability_is_denied() {
        let ev = event("bump", json!(null));
        let err = run(&WASM_HANDLER, Capabilities::default(), Limits::default(), &ev, Map::new())
            .unwrap_err();
        assert!(matches!(err, HandlerError::Guest(_)), "got {err:?}");
    }
}
