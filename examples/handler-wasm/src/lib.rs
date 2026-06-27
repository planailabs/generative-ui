//! Example WASM handler guest (core module) for the Generative UI handler ABI.
//!
//! Compiled to `wasm32-unknown-unknown` and committed as `main.wasm` so the host needs no
//! wasm toolchain. Mirrors the JS/Lua example handlers and exercises the full `host` API.
//! Routes on the backflow `event.action`:
//!   - `bump`     → fold a `hits` counter into KV, return `{hits}`
//!   - `go` + url → full API: kv + fs round-trip + http fetch + log, return a summary
//!   - `go`, none → infinite loop (proves the wall-clock limit)
//!   - `respond`  → set an explicit 404 response via the host
//!
//! ABI: see `gui-runtime`'s `wasm` module. Strings are JSON passed as `(ptr, len)`; the host
//! allocates return buffers via our exported `alloc`.

use serde_json::{json, Value};

// Host imports (module "host"). Data-returning ones give back a packed (ptr<<32 | len).
#[link(wasm_import_module = "host")]
extern "C" {
    fn kv_get(kp: i32, kl: i32) -> i64;
    fn kv_set(kp: i32, kl: i32, vp: i32, vl: i32);
    fn http_fetch(p: i32, l: i32) -> i64;
    fn fs_write(pp: i32, pl: i32, dp: i32, dl: i32);
    fn fs_read(p: i32, l: i32) -> i64;
    fn log(lp: i32, ll: i32, mp: i32, ml: i32);
    fn respond(status: i32, bp: i32, bl: i32);
    fn status_codes() -> i64;
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

fn call_io(f: unsafe extern "C" fn(i32, i32) -> i64, a: &str) -> Value {
    let b = a.as_bytes();
    let s = unsafe { unpack(f(b.as_ptr() as i32, b.len() as i32)) };
    serde_json::from_str(&s).unwrap_or(Value::Null)
}

fn call_2(f: unsafe extern "C" fn(i32, i32, i32, i32), a: &str, b: &str) {
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    unsafe { f(ab.as_ptr() as i32, ab.len() as i32, bb.as_ptr() as i32, bb.len() as i32) }
}

fn kv_get_v(key: &str) -> Value {
    call_io(kv_get, key)
}
fn kv_set_v(key: &str, v: &Value) {
    call_2(kv_set, key, &v.to_string());
}
fn status_code(name: &str) -> i32 {
    let codes = unsafe { unpack(status_codes()) };
    serde_json::from_str::<Value>(&codes)
        .ok()
        .and_then(|c| c.get(name).and_then(|x| x.as_i64()))
        .unwrap_or(200) as i32
}

fn dispatch(event: &Value) -> Value {
    let action = event.get("action").and_then(|a| a.as_str()).unwrap_or("");
    let value = event.get("value").cloned().unwrap_or(Value::Null);
    match action {
        "bump" => {
            let n = kv_get_v("hits").as_i64().unwrap_or(0) + 1;
            kv_set_v("hits", &json!(n));
            json!({ "hits": n })
        }
        "respond" => {
            let body = json!({ "error": "nope" });
            let b = body.to_string();
            unsafe { respond(status_code("NOT_FOUND"), b.as_ptr() as i32, b.len() as i32) };
            json!({ "ignored": true })
        }
        "go" => {
            if let Some(url) = value.get("url").and_then(|u| u.as_str()) {
                let n = kv_get_v("hits").as_i64().unwrap_or(0) + 1;
                kv_set_v("hits", &json!(n));
                let nn = value.get("n").and_then(|x| x.as_i64()).unwrap_or(0);
                call_2(fs_write, "note.txt", &format!("v={nn}"));
                let note = call_io(fs_read, "note.txt");
                let r = call_io(http_fetch, &json!({ "url": url }).to_string());
                call_2(log, "info", "done");
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
