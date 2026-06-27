//! Sandboxed backflow handlers: run a surface's `main.{js,lua,wasm}` against a unified,
//! capability-limited [`Host`] when a matching backflow action arrives.
//!
//! The server calls [`run_handler`]; the engine bindings live behind cargo features
//! (`js`, `lua`, `wasm`) and are selected by [`engine_for`]. Handlers never touch the
//! surface — they fold the backflow log into the surface's KV view and define the HTTP
//! response returned to the browser.

mod engine;
mod host;

#[cfg(feature = "js")]
mod js;
#[cfg(feature = "lua")]
mod lua;
#[cfg(feature = "wasm")]
mod wasm;

pub use engine::{Engine, HandlerError, HandlerResponse, Invocation};
pub use host::{Host, HostError, HttpRequest, HttpResponse, SharedHost};

use std::path::Path;

use gui_schema::{Event, HandlerDef, HandlerLang};
use serde_json::{Map, Value};

/// The result of running one handler: the response to return, plus the (possibly mutated)
/// KV view to persist.
#[derive(Debug, Clone)]
pub struct HandlerOutcome {
    pub response: HandlerResponse,
    pub kv: Map<String, Value>,
    pub kv_changed: bool,
}

/// Pick the engine for a language, or report it unavailable if its feature is off.
pub fn engine_for(lang: HandlerLang) -> Result<Box<dyn Engine>, HandlerError> {
    match lang {
        #[cfg(feature = "js")]
        HandlerLang::Js => Ok(Box::new(js::JsEngine::new())),
        #[cfg(feature = "lua")]
        HandlerLang::Lua => Ok(Box::new(lua::LuaEngine::new())),
        #[cfg(feature = "wasm")]
        HandlerLang::Wasm => Ok(Box::new(wasm::WasmEngine::new())),
        other => Err(HandlerError::EngineUnavailable(other)),
    }
}

/// Run `def`'s handler for `event`, starting from KV view `kv`. `version_dir` holds the
/// entry file; `state_dir` is the fs jail and KV location. Returns the response and the
/// resulting KV view (caller persists it when `kv_changed`).
pub fn run_handler(
    def: &HandlerDef,
    version_dir: &Path,
    state_dir: &Path,
    event: &Event,
    kv: Map<String, Value>,
) -> Result<HandlerOutcome, HandlerError> {
    let entry = version_dir.join(&def.entry);
    let engine = engine_for(def.lang)?;
    let host = Host::new(
        state_dir.to_path_buf(),
        def.capabilities.clone(),
        def.limits,
        kv,
    )
    .shared();

    let inv = Invocation {
        entry: &entry,
        event: serde_json::to_value(event).unwrap_or(Value::Null),
        limits: def.limits,
    };

    let returned = engine.run(&inv, host.clone())?;
    let h = host.borrow();
    // An explicit `host.response(code, body)` overrides the handler's return value.
    let response = match h.response() {
        Some((status, body)) => HandlerResponse {
            status: *status,
            body: body.clone(),
        },
        None => returned,
    };
    Ok(HandlerOutcome {
        response,
        kv: h.kv().clone(),
        kv_changed: h.kv_changed(),
    })
}
