//! Events state: the side-effects a handler accumulates for the caller to read back — emitted
//! events, log lines, and an optional explicit HTTP response override.

use gui_schema::Scope;
use serde_json::Value;

use super::HostError;

/// Per invocation: emitted events and log lines, and each one's size. The host's own memory
/// (the guest heap limit doesn't cover it).
const MAX_EMITS: usize = 64;
const MAX_LOGS: usize = 256;
const MAX_ITEM_BYTES: usize = 16 * 1024;

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
    pub fn emit(&mut self, scope: Scope, name: &str, data: Value) -> Result<(), HostError> {
        if self.emitted.len() >= MAX_EMITS {
            return Err(HostError::Limit(format!("at most {MAX_EMITS} events per run")));
        }
        let size = name.len() + serde_json::to_vec(&data).map(|v| v.len()).unwrap_or(usize::MAX);
        if size > MAX_ITEM_BYTES {
            return Err(HostError::Limit(format!("an event is at most {MAX_ITEM_BYTES} bytes")));
        }
        self.emitted.push(Emitted { name: name.to_string(), data, scope });
        Ok(())
    }

    pub fn log(&mut self, level: &str, message: &str) -> Result<(), HostError> {
        if self.logs.len() >= MAX_LOGS {
            return Err(HostError::Limit(format!("at most {MAX_LOGS} log lines per run")));
        }
        let message: String = message.chars().take(MAX_ITEM_BYTES / 4).collect();
        let level: String = level.chars().take(16).collect();
        tracing::info!(target: "gui_runtime::handler", %level, %message, "handler log");
        self.logs.push((level, message));
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn emits_and_logs_are_capped() {
        let mut e = Events::default();
        for _ in 0..MAX_EMITS {
            e.emit(Scope::Public, "n", json!(1)).unwrap();
        }
        assert!(e.emit(Scope::Public, "n", json!(1)).is_err());
        assert!(Events::default().emit(Scope::Public, "n", json!("x".repeat(MAX_ITEM_BYTES))).is_err());
        let mut e = Events::default();
        for _ in 0..MAX_LOGS {
            e.log("info", "m").unwrap();
        }
        assert!(e.log("info", "m").is_err());
    }
}
