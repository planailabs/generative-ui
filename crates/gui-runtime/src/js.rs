//! JavaScript engine (QuickJS via `rquickjs`).
//!
//! Sandbox posture: a bare QuickJS runtime has no ambient I/O — `fetch`, `fs`, `require`,
//! timers, etc. simply don't exist. The only capabilities a handler has are the ones we bind
//! under `host`, each gated by the shared [`Host`]. Memory is capped via
//! `Runtime::set_memory_limit`; wall-clock via an interrupt handler that trips past a deadline.
//!
//! Handler contract: `main.js` is a script (not a module) that defines a global
//! `function handle(event, host)` and returns the response body. We pass `host` as a JS object
//! that marshals every call to/from JSON across thin Rust-bound primitives.

use std::time::{Duration, Instant};

use rquickjs::{Context, Ctx, Function, Runtime};

use crate::engine::{Engine, HandlerError, HandlerResponse, Invocation};
use crate::host::{HttpRequest, SharedHost};

pub struct JsEngine;

impl JsEngine {
    pub fn new() -> Self {
        JsEngine
    }
}

/// The `host` object, built in JS over the low-level `__host_*` primitives we bind. Mirrors
/// the WIT interface / Lua table exactly.
const PRELUDE: &str = r#"
globalThis.host = {
  kv: {
    get: (k) => JSON.parse(__host_kv_get(String(k))),
    set: (k, v) => __host_kv_set(String(k), JSON.stringify(v === undefined ? null : v)),
    all: () => JSON.parse(__host_kv_all()),
  },
  http: { fetch: (req) => JSON.parse(__host_http(JSON.stringify(req))) },
  fs: {
    read: (p) => JSON.parse(__host_fs_read(String(p))),
    write: (p, d) => __host_fs_write(String(p), String(d)),
    list: (p) => JSON.parse(__host_fs_list(String(p || ""))),
  },
  log: (level, msg) => __host_log(String(level), String(msg)),
  // response(body) or response(code, body): set the HTTP response explicitly (overrides
  // the handler's return value).
  response: (code, body) => {
    if (body === undefined) { body = code; code = 200; }
    __host_respond(code | 0, JSON.stringify(body === undefined ? null : body));
  },
  responseCode: JSON.parse(__host_status_codes()),
};
"#;

impl Engine for JsEngine {
    fn run(&self, inv: &Invocation, host: SharedHost) -> Result<HandlerResponse, HandlerError> {
        let source = std::fs::read_to_string(inv.entry)
            .map_err(|e| HandlerError::Entry(inv.entry.display().to_string(), e.to_string()))?;
        let event_json = serde_json::to_string(&inv.event).unwrap_or_else(|_| "null".into());

        let rt = Runtime::new().map_err(js_err)?;
        rt.set_memory_limit((inv.limits.mem_mb as usize).saturating_mul(1024 * 1024));
        let deadline = Instant::now() + Duration::from_millis(inv.limits.wall_ms);
        rt.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));

        let ctx = Context::full(&rt).map_err(js_err)?;
        let started = Instant::now();

        let result: Result<String, HandlerError> = ctx.with(|ctx| {
            bind_host(&ctx, &host)?;
            ctx.globals()
                .set("__event_json", event_json)
                .map_err(|e| guest(&ctx, e))?;
            ctx.eval::<(), _>(PRELUDE).map_err(|e| guest(&ctx, e))?;
            ctx.eval::<(), _>(source.as_bytes()).map_err(|e| guest(&ctx, e))?;
            // Call the handler and JSON-encode its result so we can read it back into Rust.
            let out: String = ctx
                .eval(
                    "JSON.stringify((function(){\
                       if (typeof handle !== 'function') throw new Error('main.js must define a global handle(event, host)');\
                       var r = handle(JSON.parse(__event_json), host);\
                       return r === undefined ? null : r;\
                     })())"
                        .as_bytes(),
                )
                .map_err(|e| guest(&ctx, e))?;
            Ok(out)
        });

        let body_json = match result {
            Ok(s) => s,
            Err(HandlerError::Guest(_)) if started.elapsed() >= Duration::from_millis(inv.limits.wall_ms) => {
                return Err(HandlerError::Timeout(inv.limits.wall_ms));
            }
            Err(e) => return Err(e),
        };
        let body = serde_json::from_str(&body_json).unwrap_or(serde_json::Value::Null);
        Ok(HandlerResponse::ok(body))
    }
}

/// Map a top-level rquickjs error to a `HandlerError`, pulling the pending exception's message.
fn guest(ctx: &Ctx, e: rquickjs::Error) -> HandlerError {
    if e.is_exception() {
        let exc = ctx.catch();
        let msg = exc
            .as_exception()
            .and_then(|x| x.message())
            .or_else(|| exc.as_string().and_then(|s| s.to_string().ok()))
            .unwrap_or_else(|| "uncaught exception".into());
        HandlerError::Guest(msg)
    } else {
        HandlerError::Guest(e.to_string())
    }
}

fn js_err(e: rquickjs::Error) -> HandlerError {
    HandlerError::Guest(e.to_string())
}

/// Bind the low-level `__host_*` primitives. Each takes/returns JSON strings; a [`HostError`]
/// becomes a thrown JS exception.
fn bind_host(ctx: &Ctx, host: &SharedHost) -> Result<(), HandlerError> {
    let g = ctx.globals();

    macro_rules! throw {
        ($ctx:expr, $e:expr) => {
            $ctx.throw(
                rquickjs::String::from_str($ctx.clone(), &$e.to_string())
                    .map(rquickjs::Value::from_string)
                    .unwrap_or_else(|_| rquickjs::Value::new_null($ctx.clone())),
            )
        };
    }

    let h = host.clone();
    g.set(
        "__host_kv_get",
        Function::new(ctx.clone(), move |ctx: Ctx, key: String| {
            h.borrow()
                .kv_get(&key)
                .map(|v| v.to_string())
                .map_err(|e| throw!(ctx, e))
        })
        .map_err(js_err)?,
    )
    .map_err(js_err)?;

    let h = host.clone();
    g.set(
        "__host_kv_set",
        Function::new(ctx.clone(), move |ctx: Ctx, key: String, value_json: String| {
            let value = serde_json::from_str(&value_json).unwrap_or(serde_json::Value::Null);
            h.borrow_mut().kv_set(&key, value).map_err(|e| throw!(ctx, e))
        })
        .map_err(js_err)?,
    )
    .map_err(js_err)?;

    let h = host.clone();
    g.set(
        "__host_kv_all",
        Function::new(ctx.clone(), move |ctx: Ctx| {
            h.borrow().kv_all().map(|v| v.to_string()).map_err(|e| throw!(ctx, e))
        })
        .map_err(js_err)?,
    )
    .map_err(js_err)?;

    let h = host.clone();
    g.set(
        "__host_http",
        Function::new(ctx.clone(), move |ctx: Ctx, req_json: String| {
            let req = parse_http_request(&req_json).map_err(|e| throw!(ctx, e))?;
            let resp = h.borrow_mut().http_fetch(req).map_err(|e| throw!(ctx, e))?;
            Ok::<String, rquickjs::Error>(
                serde_json::json!({
                    "status": resp.status,
                    "headers": resp
                        .headers
                        .into_iter()
                        .map(|(k, v)| (k, serde_json::Value::String(v)))
                        .collect::<serde_json::Map<_, _>>(),
                    "body": resp.body,
                })
                .to_string(),
            )
        })
        .map_err(js_err)?,
    )
    .map_err(js_err)?;

    let h = host.clone();
    g.set(
        "__host_fs_read",
        Function::new(ctx.clone(), move |ctx: Ctx, path: String| {
            h.borrow().fs_read(&path).map(|v| v.to_string()).map_err(|e| throw!(ctx, e))
        })
        .map_err(js_err)?,
    )
    .map_err(js_err)?;

    let h = host.clone();
    g.set(
        "__host_fs_write",
        Function::new(ctx.clone(), move |ctx: Ctx, path: String, data: String| {
            h.borrow().fs_write(&path, &data).map_err(|e| throw!(ctx, e))
        })
        .map_err(js_err)?,
    )
    .map_err(js_err)?;

    let h = host.clone();
    g.set(
        "__host_fs_list",
        Function::new(ctx.clone(), move |ctx: Ctx, prefix: String| {
            h.borrow().fs_list(&prefix).map(|v| v.to_string()).map_err(|e| throw!(ctx, e))
        })
        .map_err(js_err)?,
    )
    .map_err(js_err)?;

    let h = host.clone();
    g.set(
        "__host_log",
        Function::new(ctx.clone(), move |level: String, msg: String| {
            h.borrow_mut().log(&level, &msg);
        })
        .map_err(js_err)?,
    )
    .map_err(js_err)?;

    let h = host.clone();
    g.set(
        "__host_respond",
        Function::new(ctx.clone(), move |status: i32, body_json: String| {
            let body = serde_json::from_str(&body_json).unwrap_or(serde_json::Value::Null);
            h.borrow_mut().set_response(status.clamp(100, 599) as u16, body);
        })
        .map_err(js_err)?,
    )
    .map_err(js_err)?;

    g.set(
        "__host_status_codes",
        Function::new(ctx.clone(), move || {
            serde_json::Value::Object(crate::host::status_codes()).to_string()
        })
        .map_err(js_err)?,
    )
    .map_err(js_err)?;

    Ok(())
}

fn parse_http_request(json: &str) -> Result<HttpRequest, crate::host::HostError> {
    use crate::host::HostError;
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| HostError::Other(format!("bad http request: {e}")))?;
    let url = v
        .get("url")
        .and_then(|u| u.as_str())
        .ok_or_else(|| HostError::Other("http request missing url".into()))?
        .to_string();
    let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("GET").to_string();
    let headers = v
        .get("headers")
        .and_then(|h| h.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let body = match v.get("body") {
        Some(serde_json::Value::String(s)) => Some(s.clone()),
        Some(serde_json::Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    };
    Ok(HttpRequest { url, method, headers, body })
}
