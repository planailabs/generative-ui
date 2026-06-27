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
pub use host::{Emitted, Host, HostError, HttpRequest, HttpResponse, SharedHost};

use std::path::Path;

use gui_schema::{Event, HandlerDef, HandlerLang, Scope};
use serde_json::{Map, Value};

/// A KV view plus whether the handler mutated it (drives persistence).
#[derive(Debug, Clone)]
pub struct KvView {
    pub map: Map<String, Value>,
    pub changed: bool,
}

/// The result of running one handler: the response to return, the (possibly mutated) public
/// and private KV views to persist, and any events the handler emitted.
#[derive(Debug, Clone)]
pub struct HandlerOutcome {
    pub response: HandlerResponse,
    pub public: KvView,
    pub private: KvView,
    pub emitted: Vec<Emitted>,
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
        // Reachable only in partial-feature builds (a lang whose engine wasn't compiled in).
        #[allow(unreachable_patterns)]
        other => Err(HandlerError::EngineUnavailable(other)),
    }
}

/// Run `def`'s handler for `event`, seeding the public and private KV views. `version_dir`
/// holds the entry file; `state_dir` is the fs jail and KV location. Returns the response, the
/// resulting KV views (caller persists each when `changed`), and any emitted events.
pub fn run_handler(
    def: &HandlerDef,
    version_dir: &Path,
    state_dir: &Path,
    event: &Event,
    kv_public: Map<String, Value>,
    kv_private: Map<String, Value>,
) -> Result<HandlerOutcome, HandlerError> {
    let entry = version_dir.join(&def.entry);
    let engine = engine_for(def.lang)?;
    let host = Host::new(
        state_dir.to_path_buf(),
        def.capabilities.clone(),
        def.limits,
        kv_public,
        kv_private,
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
        public: KvView {
            map: h.kv(Scope::Public).clone(),
            changed: h.kv_changed(Scope::Public),
        },
        private: KvView {
            map: h.kv(Scope::Private).clone(),
            changed: h.kv_changed(Scope::Private),
        },
        emitted: h.emitted().to_vec(),
    })
}
