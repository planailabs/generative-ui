//! The engine abstraction: one trait, three implementations (JS, Lua, WASM).
//!
//! Each engine loads a `main.{js,lua,wasm}`, binds the shared [`Host`](crate::host::Host) as
//! the unified `host` API, builds the triggering [`Event`] as the guest argument, calls the
//! guest's `handle(event, host)`, and returns its value as a [`HandlerResponse`]. Sandbox
//! setup and the wall-clock/memory limits are the engine's responsibility, configured from
//! [`Invocation::limits`].

use std::path::Path;

use gui_schema::{HandlerLang, Limits};
use serde_json::Value;

use crate::host::SharedHost;

/// What an engine needs for one invocation.
pub struct Invocation<'a> {
    /// The entry file (`main.js` / `main.lua` / `main.wasm`).
    pub entry: &'a Path,
    /// The triggering backflow event, serialized to the guest as the first `handle` arg.
    pub event: Value,
    pub limits: Limits,
}

/// The HTTP response a handler defines. ponytail: body only for now (status defaults 200);
/// add a `{status, headers, body}` envelope if a handler ever needs to set them.
#[derive(Debug, Clone)]
pub struct HandlerResponse {
    pub status: u16,
    pub body: Value,
}

impl HandlerResponse {
    pub fn ok(body: Value) -> Self {
        Self { status: 200, body }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    #[error("handler timed out after {0} ms")]
    Timeout(u64),
    #[error("guest error: {0}")]
    Guest(String),
    #[error("host error: {0}")]
    Host(#[from] crate::host::HostError),
    #[error("cannot read handler entry {0}: {1}")]
    Entry(String, String),
    #[error("engine for {0:?} not built (missing cargo feature)")]
    EngineUnavailable(HandlerLang),
}

/// A guest engine that runs one handler invocation against a shared host.
pub trait Engine {
    fn run(&self, inv: &Invocation, host: SharedHost) -> Result<HandlerResponse, HandlerError>;
}
