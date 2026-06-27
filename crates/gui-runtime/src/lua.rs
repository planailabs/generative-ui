//! Lua engine (Luau via `mlua`).
//!
//! Sandbox: Luau is safe-by-default; `Lua::sandbox(true)` freezes the stdlib/globals. The only
//! capability is the single bound primitive `__host_call(op, argsJson) -> resultJson`; the
//! ergonomic `host` table ([`PRELUDE`]) wraps it. Wall-clock via a Luau interrupt past a
//! deadline; memory via `set_memory_limit` where supported (best-effort on Luau).
//!
//! Handler contract: `main.lua` evaluates to `function(event, host) ... end`.

use std::time::{Duration, Instant};

use mlua::{Lua, LuaSerdeExt, Value as LuaValue, VmState};

use crate::engine::{Engine, HandlerError, HandlerResponse, Invocation};
use crate::host::SharedHost;

pub struct LuaEngine;

impl LuaEngine {
    pub fn new() -> Self {
        LuaEngine
    }
}

/// The ergonomic `host` table, built over the single `__host_call` primitive. Returns the
/// table; mirrors the JS facade exactly. `__host_call` returns JSON; we decode via the engine.
const PRELUDE: &str = r#"
local call = function(op, args) return __decode(__host_call(op, __encode(args or {}))) end
local function kv(scope)
  return {
    get = function(k) return call("kv.get", { scope = scope, key = tostring(k) }) end,
    set = function(k, v) return call("kv.set", { scope = scope, key = tostring(k), value = v }) end,
    all = function() return call("kv.all", { scope = scope }) end,
  }
end
return {
  kv = kv("public"),
  kvPrivate = kv("private"),
  http = { fetch = function(req) return call("http.fetch", { request = req }) end },
  fs = {
    read = function(p) return call("fs.read", { path = tostring(p) }) end,
    write = function(p, d) return call("fs.write", { path = tostring(p), data = tostring(d) }) end,
    list = function(p) return call("fs.list", { prefix = tostring(p or "") }) end,
  },
  log = function(level, msg) return call("log", { level = tostring(level), message = tostring(msg) }) end,
  emit = function(name, data) return call("emit", { scope = "public", name = tostring(name), data = data }) end,
  emitPrivate = function(name, data) return call("emit", { scope = "private", name = tostring(name), data = data }) end,
  response = function(code, body)
    if body == nil then body = code; code = 200 end
    return call("respond", { status = code, body = body })
  end,
  responseCode = call("status_codes", {}),
}
"#;

impl Engine for LuaEngine {
    fn run(&self, inv: &Invocation, host: SharedHost) -> Result<HandlerResponse, HandlerError> {
        let source = std::fs::read_to_string(inv.entry)
            .map_err(|e| HandlerError::Entry(inv.entry.display().to_string(), e.to_string()))?;

        let lua = Lua::new();
        // Best-effort memory cap (Luau manages its own allocator; the interrupt is the hard stop).
        let _ = lua.set_memory_limit((inv.limits.mem_mb as usize).saturating_mul(1024 * 1024));
        let deadline = Instant::now() + Duration::from_millis(inv.limits.wall_ms);
        lua.set_interrupt(move |_| {
            if Instant::now() >= deadline {
                Err(mlua::Error::runtime("handler timed out"))
            } else {
                Ok(VmState::Continue)
            }
        });

        let started = Instant::now();
        let result = run_inner(&lua, &host, &source, &inv.event);
        match result {
            Ok(body) => Ok(HandlerResponse::ok(body)),
            Err(_) if started.elapsed() >= Duration::from_millis(inv.limits.wall_ms) => {
                Err(HandlerError::Timeout(inv.limits.wall_ms))
            }
            Err(e) => Err(HandlerError::Guest(e.to_string())),
        }
    }
}

fn run_inner(
    lua: &Lua,
    host: &SharedHost,
    source: &str,
    event: &serde_json::Value,
) -> mlua::Result<serde_json::Value> {
    bind_host_call(lua, host)?;
    let host_tbl: mlua::Table = lua.load(PRELUDE).set_name("host_prelude").eval()?;
    lua.sandbox(true)?;

    let handle: mlua::Function = lua.load(source).set_name("main.lua").eval()?;
    let event_val = to_lua(lua, event)?;
    let ret: LuaValue = handle.call((event_val, host_tbl))?;
    Ok(lua.from_value(ret).unwrap_or(serde_json::Value::Null))
}

/// Bind `__host_call` plus JSON `__encode`/`__decode` helpers used by the prelude (Luau has no
/// built-in JSON), all over the single host dispatch.
fn bind_host_call(lua: &Lua, host: &SharedHost) -> mlua::Result<()> {
    let g = lua.globals();
    let h = host.clone();
    g.set(
        "__host_call",
        lua.create_function(move |_, (op, args_json): (String, String)| {
            let args = serde_json::from_str(&args_json).unwrap_or(serde_json::Value::Null);
            h.borrow_mut()
                .dispatch(&op, &args)
                .map(|v| v.to_string())
                .map_err(|e| mlua::Error::runtime(e.to_string()))
        })?,
    )?;
    g.set(
        "__encode",
        lua.create_function(|lua, v: LuaValue| {
            let json: serde_json::Value = lua.from_value(v)?;
            Ok(json.to_string())
        })?,
    )?;
    g.set(
        "__decode",
        lua.create_function(|lua, s: String| {
            let json: serde_json::Value = serde_json::from_str(&s).unwrap_or(serde_json::Value::Null);
            to_lua(lua, &json)
        })?,
    )?;
    Ok(())
}

/// Convert a JSON value to Lua, mapping `null` to Lua `nil` (not mlua's null *sentinel*) so
/// idioms like `host.kv.get(k) or 0` work.
fn to_lua(lua: &Lua, v: &serde_json::Value) -> mlua::Result<LuaValue> {
    let opts = mlua::SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false);
    lua.to_value_with(v, opts)
}
