//! KV state: the public + private maps with per-scope dirty tracking.

use gui_schema::Scope;
use serde_json::{Map, Value};

use super::HostError;

/// Per-value KV ceiling (serialized bytes). ponytail: fixed cap; lift into `Limits` if a
/// handler ever legitimately needs larger values.
const MAX_KV_VALUE_BYTES: usize = 64 * 1024;

/// The two KV maps a handler can read/write, each flagged dirty when mutated so the caller knows
/// whether to persist it. Public is browser-visible; private is agent-only (the host enforces the
/// capability; this type is pure state).
#[derive(Default)]
pub struct Kv {
    public: Map<String, Value>,
    private: Map<String, Value>,
    public_dirty: bool,
    private_dirty: bool,
}

impl Kv {
    pub fn new(public: Map<String, Value>, private: Map<String, Value>) -> Self {
        Self { public, private, public_dirty: false, private_dirty: false }
    }

    pub fn view(&self, scope: Scope) -> &Map<String, Value> {
        match scope {
            Scope::Public => &self.public,
            Scope::Private => &self.private,
        }
    }

    pub fn changed(&self, scope: Scope) -> bool {
        match scope {
            Scope::Public => self.public_dirty,
            Scope::Private => self.private_dirty,
        }
    }

    pub fn get(&self, scope: Scope, key: &str) -> Value {
        self.view(scope).get(key).cloned().unwrap_or(Value::Null)
    }

    pub fn set(&mut self, scope: Scope, key: &str, value: Value) -> Result<(), HostError> {
        let size = serde_json::to_vec(&value).map(|v| v.len()).unwrap_or(usize::MAX);
        if size > MAX_KV_VALUE_BYTES {
            return Err(HostError::Limit(format!(
                "kv value for {key:?} is {size} bytes (max {MAX_KV_VALUE_BYTES})"
            )));
        }
        let (map, dirty) = match scope {
            Scope::Public => (&mut self.public, &mut self.public_dirty),
            Scope::Private => (&mut self.private, &mut self.private_dirty),
        };
        map.insert(key.to_string(), value);
        *dirty = true;
        Ok(())
    }
}
