//! The host: the single, capability-gated effect surface behind every engine.
//!
//! There is one host implementation and **one entry point** — [`Host::dispatch`] — which resolves
//! the op against the API tree in [`crate::facade`]. The host's *state* is split into the
//! individual parts ([`kv`], [`fs`], [`http`], [`events`]; `crypto` is a stateless module of its
//! own); `Host` composes them and is the one place the capability gate lives, so the three
//! languages stay identical and default-deny is enforced once.

mod events;
mod fs;
mod http;
mod kv;

pub use events::Emitted;
pub use http::{status_codes, HttpRequest, HttpResponse};

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use gui_schema::{Capabilities, Limits, Scope};
use serde_json::{Map, Value};

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

/// A host shared into an engine's guest callbacks.
pub type SharedHost = Rc<RefCell<Host>>;

/// The unified effect surface, shared into each engine as `Rc<RefCell<Host>>`. Composes the per-
/// part state; the caller reads the resulting KV/emitted-events back out after the run.
pub struct Host {
    /// Jail root for `fs.*` and the location of the KV files.
    state_dir: PathBuf,
    caps: Capabilities,
    limits: Limits,
    kv: kv::Kv,
    http: http::Http,
    events: events::Events,
}

impl Host {
    pub fn new(
        state_dir: PathBuf,
        caps: Capabilities,
        limits: Limits,
        kv_public: Map<String, Value>,
        kv_private: Map<String, Value>,
    ) -> Self {
        Self {
            state_dir,
            caps,
            limits,
            kv: kv::Kv::new(kv_public, kv_private),
            http: http::Http::default(),
            events: events::Events::default(),
        }
    }

    pub fn shared(self) -> SharedHost {
        Rc::new(RefCell::new(self))
    }

    // --- read-back accessors (the caller persists what changed) -----------

    /// The current KV view for `scope`.
    pub fn kv(&self, scope: Scope) -> &Map<String, Value> {
        self.kv.view(scope)
    }
    /// Whether a handler mutated `scope`'s KV (drives persistence).
    pub fn kv_changed(&self, scope: Scope) -> bool {
        self.kv.changed(scope)
    }
    pub fn emitted(&self) -> &[Emitted] {
        self.events.emitted()
    }
    pub fn logs(&self) -> &[(String, String)] {
        self.events.logs()
    }
    /// The explicit response, if the handler called `host.response`.
    pub fn response(&self) -> Option<&(u16, Value)> {
        self.events.response()
    }

    /// The single entry point every engine calls. `op` selects the operation; `args` is a JSON
    /// object of named parameters. Routing, the facade and capability checks all derive from the
    /// one host API tree in [`crate::facade`] — this resolves the leaf whose op-id is `op` and
    /// runs it. Crypto leaves forward to the crypto router; it is ungated (pure compute).
    pub fn dispatch(&mut self, op: &str, args: &Value) -> Result<Value, HostError> {
        let root = crate::facade::root();
        match crate::facade::find_op(&root, op) {
            Some(node) => node.call(self, args),
            None => Err(HostError::Other(format!("unknown host op {op:?}"))),
        }
    }

    // --- gated ops (called by the facade tree's leaf nodes) ---------------

    pub fn kv_get(&self, scope: Scope, key: &str) -> Result<Value, HostError> {
        self.require_kv()?;
        Ok(self.kv.get(scope, key))
    }
    pub fn kv_set(&mut self, scope: Scope, key: &str, value: Value) -> Result<(), HostError> {
        self.require_kv()?;
        self.kv.set(scope, key, value)
    }
    pub fn kv_all(&self, scope: Scope) -> Result<Value, HostError> {
        self.require_kv()?;
        Ok(Value::Object(self.kv.view(scope).clone()))
    }

    pub fn http_fetch(&mut self, req: HttpRequest) -> Result<HttpResponse, HostError> {
        self.http.fetch(req, &self.caps, &self.limits)
    }

    pub fn fs_read(&self, rel: &str) -> Result<Value, HostError> {
        self.require_fs()?;
        fs::read(&self.state_dir, rel)
    }
    pub fn fs_write(&self, rel: &str, data: &str) -> Result<(), HostError> {
        self.require_fs()?;
        fs::write(&self.state_dir, &self.limits, rel, data)
    }
    pub fn fs_list(&self, prefix: &str) -> Result<Value, HostError> {
        self.require_fs()?;
        fs::list(&self.state_dir, prefix)
    }

    pub fn emit(&mut self, scope: Scope, name: &str, data: Value) {
        self.events.emit(scope, name, data);
    }
    pub fn log(&mut self, level: &str, message: &str) {
        self.events.log(level, message);
    }
    pub fn set_response(&mut self, status: u16, body: Value) {
        self.events.set_response(status, body);
    }

    // --- capability guards ------------------------------------------------

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
}

/// `args.scope == "private"` ⇒ `Private`, else `Public`.
pub(crate) fn scope_arg(args: &Value) -> Scope {
    match args.get("scope").and_then(|v| v.as_str()) {
        Some("private") => Scope::Private,
        _ => Scope::Public,
    }
}

pub(crate) fn arg(args: &Value, key: &str) -> Value {
    args.get(key).cloned().unwrap_or(Value::Null)
}

pub(crate) fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, HostError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| HostError::Other(format!("missing string arg {key:?}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn host(caps: Capabilities) -> Host {
        let dir = std::env::temp_dir().join(format!("gui-rt-{}-{:p}", std::process::id(), &caps));
        std::fs::create_dir_all(&dir).unwrap();
        Host::new(dir, caps, Limits::default(), Map::new(), Map::new())
    }

    #[test]
    fn dispatch_kv_roundtrip_and_scope_isolation() {
        let mut h = host(Capabilities { kv: true, ..Default::default() });
        // public + private are independent maps.
        h.dispatch("kv.set", &json!({ "key": "hits", "value": 1 })).unwrap();
        h.dispatch("kv.set", &json!({ "scope": "private", "key": "secret", "value": "s" })).unwrap();
        assert_eq!(h.dispatch("kv.get", &json!({ "key": "hits" })).unwrap(), json!(1));
        assert_eq!(h.dispatch("kv.get", &json!({ "key": "secret" })).unwrap(), json!(null), "public can't see private");
        assert_eq!(
            h.dispatch("kv.get", &json!({ "scope": "private", "key": "secret" })).unwrap(),
            json!("s")
        );
        assert!(h.kv_changed(Scope::Public) && h.kv_changed(Scope::Private));
        assert_eq!(h.kv(Scope::Public), json!({ "hits": 1 }).as_object().unwrap());
    }

    #[test]
    fn dispatch_emit_records_events() {
        let mut h = host(Capabilities::default());
        h.dispatch("emit", &json!({ "name": "saved", "data": { "id": 1 } })).unwrap();
        h.dispatch("emit", &json!({ "scope": "private", "name": "audit", "data": "x" })).unwrap();
        assert_eq!(h.emitted().len(), 2);
        assert_eq!(h.emitted()[0].name, "saved");
        assert_eq!(h.emitted()[0].scope, Scope::Public);
        assert_eq!(h.emitted()[1].scope, Scope::Private);
    }

    #[test]
    fn dispatch_capabilities_default_deny() {
        let mut h = host(Capabilities::default());
        assert!(matches!(h.dispatch("kv.get", &json!({ "key": "x" })), Err(HostError::Denied(_))));
        assert!(matches!(h.dispatch("fs.read", &json!({ "path": "a" })), Err(HostError::Denied(_))));
        assert!(matches!(
            h.dispatch("http.fetch", &json!({ "request": { "url": "https://blocked.example/x" } })),
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
    fn unknown_op_errors() {
        let mut h = host(Capabilities::default());
        assert!(matches!(h.dispatch("nope", &json!({})), Err(HostError::Other(_))));
    }
}
