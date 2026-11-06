//! KV state: the public + private maps with per-scope dirty tracking.

use gui_schema::Scope;
use serde_json::{Map, Value};

use super::HostError;

/// Per-value KV ceiling (serialized bytes). ponytail: fixed cap; lift into `Limits` if a
/// handler ever legitimately needs larger values.
const MAX_KV_VALUE_BYTES: usize = 64 * 1024;
/// Keys per scope, and bytes of all values of a scope together: the host's own memory, which
/// the guest's heap limit doesn't cover.
const MAX_KV_KEYS: usize = 1024;
const MAX_KV_TOTAL_BYTES: usize = 1024 * 1024;

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
        if !map.contains_key(key) && map.len() >= MAX_KV_KEYS {
            return Err(HostError::Limit(format!("kv holds at most {MAX_KV_KEYS} keys")));
        }
        let others: usize = map.iter().filter(|(k, _)| k.as_str() != key).map(|(_, v)| serde_json::to_vec(v).map(|b| b.len()).unwrap_or(0)).sum();
        if others + size > MAX_KV_TOTAL_BYTES {
            return Err(HostError::Limit(format!("kv holds at most {MAX_KV_TOTAL_BYTES} bytes")));
        }
        map.insert(key.to_string(), value);
        *dirty = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_and_total_bytes_are_capped() {
        let mut kv = Kv::default();
        for i in 0..MAX_KV_KEYS {
            kv.set(Scope::Public, &format!("k{i}"), json!(i)).unwrap();
        }
        assert!(kv.set(Scope::Public, "one-more", json!(1)).is_err(), "key cap");
        assert!(kv.set(Scope::Public, "k1", json!("replacing is fine")).is_ok());
        let mut kv = Kv::default();
        let big = json!("x".repeat(60 * 1024));
        let mut stored = 0;
        while kv.set(Scope::Private, &format!("b{stored}"), big.clone()).is_ok() {
            stored += 1;
        }
        assert!(stored * 60 * 1024 <= MAX_KV_TOTAL_BYTES && stored > 10, "{stored}");
    }
}
