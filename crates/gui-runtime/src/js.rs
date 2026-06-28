//! JavaScript engine (QuickJS via `rquickjs`).
//!
//! Sandbox: a bare QuickJS runtime has no ambient I/O — only what we bind exists. Memory is
//! capped via `set_memory_limit`; wall-clock via an interrupt past a deadline. The whole host
//! surface is reached through ONE bound primitive, `__host_call(op, argsJson) -> resultJson`;
//! the ergonomic `host` object ([`PRELUDE`]) is a thin wrapper over it.
//!
//! Handler contract: `main.js` is a script defining a global `function handle(event, host)`
//! that returns the response body.

use std::time::{Duration, Instant};

use rquickjs::{Context, Ctx, Function, Runtime};

use crate::engine::{Engine, HandlerError, HandlerResponse, Invocation};
use crate::host::SharedHost;

pub struct JsEngine;

impl JsEngine {
    pub fn new() -> Self {
        JsEngine
    }
}

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
            bind_host_call(&ctx, &host)?;
            ctx.globals()
                .set("__event_json", event_json)
                .map_err(|e| guest(&ctx, e))?;
            ctx.eval::<(), _>(crate::facade::js_prelude()).map_err(|e| guest(&ctx, e))?;
            ctx.eval::<(), _>(source.as_bytes()).map_err(|e| guest(&ctx, e))?;
            ctx.eval(
                "JSON.stringify((function(){\
                   if (typeof handle !== 'function') throw new Error('main.js must define a global handle(event, host)');\
                   var r = handle(JSON.parse(__event_json), host);\
                   return r === undefined ? null : r;\
                 })())"
                    .as_bytes(),
            )
            .map_err(|e| guest(&ctx, e))
        });

        let body_json = match result {
            Ok(s) => s,
            Err(HandlerError::Guest(_)) if started.elapsed() >= Duration::from_millis(inv.limits.wall_ms) => {
                return Err(HandlerError::Timeout(inv.limits.wall_ms));
            }
            Err(e) => return Err(e),
        };
        Ok(HandlerResponse::ok(
            serde_json::from_str(&body_json).unwrap_or(serde_json::Value::Null),
        ))
    }
}

/// Bind the single `__host_call(op, argsJson) -> resultJson`. A [`HostError`] becomes a thrown
/// JS exception.
fn bind_host_call(ctx: &Ctx, host: &SharedHost) -> Result<(), HandlerError> {
    let h = host.clone();
    let f = Function::new(ctx.clone(), move |ctx: Ctx, op: String, args_json: String| {
        let args = serde_json::from_str(&args_json).unwrap_or(serde_json::Value::Null);
        match h.borrow_mut().dispatch(&op, &args) {
            Ok(v) => Ok(v.to_string()),
            Err(e) => Err(ctx.throw(
                rquickjs::String::from_str(ctx.clone(), &e.to_string())
                    .map(rquickjs::Value::from_string)
                    .unwrap_or_else(|_| rquickjs::Value::new_null(ctx.clone())),
            )),
        }
    })
    .map_err(js_err)?;
    ctx.globals().set("__host_call", f).map_err(js_err)?;
    Ok(())
}

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
