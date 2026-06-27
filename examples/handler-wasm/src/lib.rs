//! Example WASM handler guest (core module) for the Generative UI handler ABI.
//!
//! Compiled to `wasm32-unknown-unknown` and committed as `main.wasm` so the host needs no
//! wasm toolchain. Reaches the whole host through ONE import — `host_call(op, args) -> json`
//! — matching `gui-runtime`'s single dispatch; helpers below are the thin guest facade.
//! Routes on the backflow `event.action`: `bump` (KV counter), `go`+url (full API),
//! `go` w/o url (infinite loop → wall-clock), `respond` (explicit 404).

use serde_json::{json, Value};

#[link(wasm_import_module = "host")]
extern "C" {
    fn host_call(op_ptr: i32, op_len: i32, args_ptr: i32, args_len: i32) -> i64;
}

/// Allocate `len` bytes in our linear memory and hand the host the pointer.
#[no_mangle]
pub extern "C" fn alloc(len: i32) -> i32 {
    let mut buf = Vec::<u8>::with_capacity(len.max(0) as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr as i32
}

unsafe fn unpack(packed: i64) -> String {
    let ptr = (packed >> 32) as u32 as *const u8;
    let len = (packed & 0xffff_ffff) as usize;
    String::from_utf8_lossy(std::slice::from_raw_parts(ptr, len)).into_owned()
}

/// The one host primitive: send `(op, args)` as JSON, get JSON back.
fn call(op: &str, args: &Value) -> Value {
    let a = args.to_string();
    let packed = unsafe {
        host_call(op.as_ptr() as i32, op.len() as i32, a.as_ptr() as i32, a.len() as i32)
    };
    serde_json::from_str(&unsafe { unpack(packed) }).unwrap_or(Value::Null)
}

fn kv_get(key: &str) -> Value {
    call("kv.get", &json!({ "key": key }))
}
fn kv_set(key: &str, value: Value) {
    call("kv.set", &json!({ "key": key, "value": value }));
}

fn dispatch(event: &Value) -> Value {
    let action = event.get("action").and_then(|a| a.as_str()).unwrap_or("");
    let value = event.get("value").cloned().unwrap_or(Value::Null);
    match action {
        "bump" => {
            let n = kv_get("hits").as_i64().unwrap_or(0) + 1;
            kv_set("hits", json!(n));
            json!({ "hits": n })
        }
        "respond" => {
            let code = call("status_codes", &json!({}))
                .get("NOT_FOUND")
                .and_then(|x| x.as_i64())
                .unwrap_or(404);
            call("respond", &json!({ "status": code, "body": { "error": "nope" } }));
            json!({ "ignored": true })
        }
        "go" => {
            if let Some(url) = value.get("url").and_then(|u| u.as_str()) {
                let n = kv_get("hits").as_i64().unwrap_or(0) + 1;
                kv_set("hits", json!(n));
                let nn = value.get("n").and_then(|x| x.as_i64()).unwrap_or(0);
                call("fs.write", &json!({ "path": "note.txt", "data": format!("v={nn}") }));
                let note = call("fs.read", &json!({ "path": "note.txt" }));
                let r = call("http.fetch", &json!({ "request": { "url": url } }));
                call("log", &json!({ "level": "info", "message": "done" }));
                let status = r.get("status").and_then(|x| x.as_i64()).unwrap_or(0);
                let bodylen = r.get("body").and_then(|x| x.as_str()).map(|s| s.len()).unwrap_or(0);
                json!({ "hits": n, "note": note, "status": status, "bodylen": bodylen })
            } else {
                #[allow(clippy::empty_loop)]
                loop {}
            }
        }
        _ => Value::Null,
    }
}

/// Entry point: read the event, run the handler, return the response body as packed (ptr,len).
#[no_mangle]
pub extern "C" fn handle(ev_ptr: i32, ev_len: i32) -> i64 {
    let event_str = unsafe { unpack(((ev_ptr as i64) << 32) | (ev_len as i64 & 0xffff_ffff)) };
    let event: Value = serde_json::from_str(&event_str).unwrap_or(Value::Null);
    let out = dispatch(&event).to_string().into_bytes();
    let ptr = alloc(out.len() as i32) as *mut u8;
    unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), ptr, out.len()) };
    ((ptr as i64) << 32) | (out.len() as i64 & 0xffff_ffff)
}
