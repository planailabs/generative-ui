//! Events state: the side-effects a handler accumulates for the caller to read back — emitted
//! events, log lines, and an optional explicit HTTP response override.

use gui_schema::Scope;
use serde_json::Value;

/// An event a handler asked to record via `host.emit` — the server writes it into the surface's
/// `events/` dir so the agent (and, when public, the events API) sees it.
#[derive(Debug, Clone)]
pub struct Emitted {
    pub name: String,
    pub data: Value,
    pub scope: Scope,
}

/// Accumulated handler side-effects, drained by the caller after the run.
#[derive(Default)]
pub struct Events {
    emitted: Vec<Emitted>,
    logs: Vec<(String, String)>,
    /// Explicit response set via `host.response(code, body)`; overrides the handler's return.
    response: Option<(u16, Value)>,
}

impl Events {
    pub fn emit(&mut self, scope: Scope, name: &str, data: Value) {
        self.emitted.push(Emitted { name: name.to_string(), data, scope });
    }

    pub fn log(&mut self, level: &str, message: &str) {
        tracing::info!(target: "gui_runtime::handler", %level, %message, "handler log");
        self.logs.push((level.to_string(), message.to_string()));
    }

    pub fn set_response(&mut self, status: u16, body: Value) {
        self.response = Some((status, body));
    }

    pub fn emitted(&self) -> &[Emitted] {
        &self.emitted
    }
    pub fn logs(&self) -> &[(String, String)] {
        &self.logs
    }
    pub fn response(&self) -> Option<&(u16, Value)> {
        self.response.as_ref()
    }
}
