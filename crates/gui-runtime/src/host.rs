//! The host: the single, capability-gated effect surface handed to every engine.
//!
//! There is exactly one host implementation — the unified API the spec calls for lives here
//! and is mirrored verbatim by each engine binding (WIT for WASM, a `host` global in JS, a
//! `host` table in Lua). ponytail: a concrete struct, not a trait — one implementation, so a
//! trait would be ceremony. Extract a trait the day a second host is real.
//!
//! Every method enforces the handler's [`Capabilities`] (default-deny) and [`Limits`]
//! host-side, so the guarantees are identical regardless of guest language.

use std::cell::RefCell;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use gui_schema::{Capabilities, Limits};
use serde_json::{Map, Value};

/// Per-value KV ceiling (serialized bytes). ponytail: fixed cap; lift into `Limits` if a
/// handler ever legitimately needs larger values.
const MAX_KV_VALUE_BYTES: usize = 64 * 1024;

/// A host error surfaced to the guest as a thrown error / trap.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("capability denied: {0}")]
    Denied(String),
    #[error("limit exceeded: {0}")]
    Limit(String),
    #[error("{0}")]
    Other(String),
}

/// An outbound HTTP request a handler asked for.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

impl HttpRequest {
    /// Build a request from a guest-supplied `{url, method?, headers?, body?}` value.
    pub fn from_json(v: &Value) -> Result<Self, HostError> {
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
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Null) | None => None,
            Some(other) => Some(other.to_string()),
        };
        Ok(Self { url, method, headers, body })
    }
}

impl HttpResponse {
    /// As a JSON object `{status, headers, body}` for handing back to the guest.
    pub fn to_json(&self) -> Value {
        let headers: Map<String, Value> = self
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        serde_json::json!({ "status": self.status, "headers": headers, "body": self.body })
    }
}

/// The response handed back to the guest.
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// The unified effect surface. Shared into each engine as `Rc<RefCell<Host>>` so the guest's
/// host-function callbacks can mutate it; the caller reads the resulting KV back out after.
pub struct Host {
    /// Jail root for `fs.*` and the location of `kv.json`.
    state_dir: PathBuf,
    caps: Capabilities,
    limits: Limits,
    kv: Map<String, Value>,
    kv_changed: bool,
    http_calls: u32,
    /// Explicit response set via `host.response(code, body)`; overrides the handler's return.
    response: Option<(u16, Value)>,
    /// Collected `log` lines (level, message) — also emitted via `tracing`.
    logs: Vec<(String, String)>,
}

/// `NAME -> code` for every status the `http` crate knows, named in SCREAMING_SNAKE to match
/// the crate's own constants (e.g. 404 → `NOT_FOUND`, 200 → `OK`, 418 → `IM_A_TEAPOT`).
/// Drives `host.responseCode.*`; sourced from the `http` crate so the numbers are
/// authoritative. Used by each engine binding (behind its feature); unused in the bare
/// host-only build.
#[allow(dead_code)]
pub fn status_codes() -> Map<String, Value> {
    (100u16..=599)
        .filter_map(|n| {
            let s = http::StatusCode::from_u16(n).ok()?;
            let reason = s.canonical_reason()?;
            Some((screaming_snake(reason), Value::from(n)))
        })
        .collect()
}

fn screaming_snake(reason: &str) -> String {
    reason
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_uppercase())
        .collect::<Vec<_>>()
        .join("_")
}

/// A host shared into an engine's guest callbacks.
pub type SharedHost = Rc<RefCell<Host>>;

impl Host {
    pub fn new(state_dir: PathBuf, caps: Capabilities, limits: Limits, kv: Map<String, Value>) -> Self {
        Self {
            state_dir,
            caps,
            limits,
            kv,
            kv_changed: false,
            http_calls: 0,
            response: None,
            logs: Vec::new(),
        }
    }

    pub fn shared(self) -> SharedHost {
        Rc::new(RefCell::new(self))
    }

    /// The current KV view and whether a handler mutated it (drives persistence).
    pub fn kv(&self) -> &Map<String, Value> {
        &self.kv
    }
    pub fn kv_changed(&self) -> bool {
        self.kv_changed
    }
    pub fn into_kv(self) -> Map<String, Value> {
        self.kv
    }
    pub fn logs(&self) -> &[(String, String)] {
        &self.logs
    }

    /// Set the response explicitly (via `host.response(code, body)`); overrides the handler's
    /// return value. Last call wins.
    pub fn set_response(&mut self, status: u16, body: Value) {
        self.response = Some((status, body));
    }

    /// The explicit response, if the handler called `host.response`.
    pub fn response(&self) -> Option<&(u16, Value)> {
        self.response.as_ref()
    }

    // --- kv ---------------------------------------------------------------

    pub fn kv_get(&self, key: &str) -> Result<Value, HostError> {
        self.require_kv()?;
        Ok(self.kv.get(key).cloned().unwrap_or(Value::Null))
    }

    pub fn kv_set(&mut self, key: &str, value: Value) -> Result<(), HostError> {
        self.require_kv()?;
        let size = serde_json::to_vec(&value).map(|v| v.len()).unwrap_or(usize::MAX);
        if size > MAX_KV_VALUE_BYTES {
            return Err(HostError::Limit(format!(
                "kv value for {key:?} is {size} bytes (max {MAX_KV_VALUE_BYTES})"
            )));
        }
        self.kv.insert(key.to_string(), value);
        self.kv_changed = true;
        Ok(())
    }

    pub fn kv_all(&self) -> Result<Value, HostError> {
        self.require_kv()?;
        Ok(Value::Object(self.kv.clone()))
    }

    // --- http -------------------------------------------------------------

    pub fn http_fetch(&mut self, req: HttpRequest) -> Result<HttpResponse, HostError> {
        let host = url::Url::parse(&req.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_string))
            .ok_or_else(|| HostError::Other(format!("invalid url {:?}", req.url)))?;
        if !self.caps.http.iter().any(|h| h == &host) {
            return Err(HostError::Denied(format!("http host {host:?} not in allowlist")));
        }
        if self.http_calls >= self.limits.http_calls {
            return Err(HostError::Limit(format!(
                "http_calls exceeded ({})",
                self.limits.http_calls
            )));
        }
        self.http_calls += 1;

        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_millis(self.limits.wall_ms.max(1)))
            .build();
        let mut request = agent.request(&req.method, &req.url);
        for (k, v) in &req.headers {
            request = request.set(k, v);
        }
        let send = match &req.body {
            Some(b) => request.send_string(b),
            None => request.call(),
        };
        // ureq treats non-2xx as Err(Status); we want the response either way.
        let resp = match send {
            Ok(r) => r,
            Err(ureq::Error::Status(_, r)) => r,
            Err(e) => return Err(HostError::Other(format!("http error: {e}"))),
        };
        let status = resp.status();
        let headers = resp
            .headers_names()
            .into_iter()
            .filter_map(|n| resp.header(&n).map(|v| (n.clone(), v.to_string())))
            .collect();
        let cap = (self.limits.resp_kb as usize).saturating_mul(1024);
        let mut body = String::new();
        resp.into_reader()
            .take(cap as u64 + 1)
            .read_to_string(&mut body)
            .map_err(|e| HostError::Other(format!("read body: {e}")))?;
        if body.len() > cap {
            return Err(HostError::Limit(format!(
                "response body exceeds {} KB",
                self.limits.resp_kb
            )));
        }
        Ok(HttpResponse { status, headers, body })
    }

    // --- fs (jailed to state_dir) ----------------------------------------

    pub fn fs_read(&self, rel: &str) -> Result<Value, HostError> {
        self.require_fs()?;
        let path = self.jail(rel)?;
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Value::String(s)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
            Err(e) => Err(HostError::Other(format!("fs read: {e}"))),
        }
    }

    pub fn fs_write(&self, rel: &str, data: &str) -> Result<(), HostError> {
        self.require_fs()?;
        let cap = (self.limits.fs_kb as usize).saturating_mul(1024);
        if data.len() > cap {
            return Err(HostError::Limit(format!("fs write exceeds {} KB", self.limits.fs_kb)));
        }
        let path = self.jail(rel)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| HostError::Other(format!("fs mkdir: {e}")))?;
        }
        gui_schema::atomic_write(&path, data.as_bytes())
            .map_err(|e| HostError::Other(format!("fs write: {e}")))
    }

    pub fn fs_list(&self, prefix: &str) -> Result<Value, HostError> {
        self.require_fs()?;
        let dir = self.jail(prefix)?;
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.filter_map(|e| e.ok()) {
                if let Some(n) = e.file_name().to_str() {
                    out.push(Value::String(n.to_string()));
                }
            }
        }
        Ok(Value::Array(out))
    }

    // --- log --------------------------------------------------------------

    pub fn log(&mut self, level: &str, message: &str) {
        tracing::info!(target: "gui_runtime::handler", %level, %message, "handler log");
        self.logs.push((level.to_string(), message.to_string()));
    }

    // --- guards -----------------------------------------------------------

    fn require_kv(&self) -> Result<(), HostError> {
        self.caps
            .kv
            .then_some(())
            .ok_or_else(|| HostError::Denied("kv capability not granted".into()))
    }
    fn require_fs(&self) -> Result<(), HostError> {
        self.caps
            .fs
            .then_some(())
            .ok_or_else(|| HostError::Denied("fs capability not granted".into()))
    }

    /// Resolve a guest-supplied relative path inside the jail, rejecting absolute paths and
    /// any `..` traversal.
    fn jail(&self, rel: &str) -> Result<PathBuf, HostError> {
        let p = Path::new(rel);
        if p.is_absolute()
            || p.components()
                .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir))
        {
            return Err(HostError::Denied(format!("path {rel:?} escapes state dir")));
        }
        Ok(self.state_dir.join(p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn host(caps: Capabilities) -> Host {
        let dir = std::env::temp_dir().join(format!("gui-rt-{}-{:p}", std::process::id(), &caps));
        std::fs::create_dir_all(&dir).unwrap();
        Host::new(dir, caps, Limits::default(), Map::new())
    }

    #[test]
    fn kv_roundtrip_and_dirty() {
        let mut h = host(Capabilities { kv: true, ..Default::default() });
        assert_eq!(h.kv_get("hits").unwrap(), json!(null));
        assert!(!h.kv_changed());
        h.kv_set("hits", json!(1)).unwrap();
        assert!(h.kv_changed());
        assert_eq!(h.kv_get("hits").unwrap(), json!(1));
        assert_eq!(h.kv_all().unwrap(), json!({ "hits": 1 }));
    }

    #[test]
    fn capabilities_default_deny() {
        let mut h = host(Capabilities::default());
        assert!(matches!(h.kv_get("x"), Err(HostError::Denied(_))));
        assert!(matches!(h.kv_set("x", json!(1)), Err(HostError::Denied(_))));
        assert!(matches!(h.fs_read("a"), Err(HostError::Denied(_))));
        assert!(matches!(
            h.http_fetch(HttpRequest {
                url: "https://blocked.example/x".into(),
                method: "GET".into(),
                headers: vec![],
                body: None,
            }),
            Err(HostError::Denied(_))
        ));
    }

    #[test]
    fn fs_jail_blocks_traversal_and_caps_size() {
        let h = host(Capabilities { fs: true, ..Default::default() });
        h.fs_write("note.txt", "hello").unwrap();
        assert_eq!(h.fs_read("note.txt").unwrap(), json!("hello"));
        assert_eq!(h.fs_read("missing.txt").unwrap(), json!(null));
        assert!(matches!(h.fs_write("../escape", "x"), Err(HostError::Denied(_))));
        assert!(matches!(h.fs_read("/etc/passwd"), Err(HostError::Denied(_))));
        let big = "x".repeat((Limits::default().fs_kb as usize) * 1024 + 1);
        assert!(matches!(h.fs_write("big.txt", &big), Err(HostError::Limit(_))));
    }

    #[test]
    fn http_allowlist_denies_unlisted_host() {
        let mut h = host(Capabilities {
            http: vec!["allowed.example".into()],
            ..Default::default()
        });
        assert!(matches!(
            h.http_fetch(HttpRequest {
                url: "https://other.example/data".into(),
                method: "GET".into(),
                headers: vec![],
                body: None,
            }),
            Err(HostError::Denied(_))
        ));
    }
}
