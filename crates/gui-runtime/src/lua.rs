//! Lua engine (Luau via `mlua`).
//!
//! Sandbox posture: Luau is safe-by-default (no `io`, no `os.execute`, no `require` of
//! arbitrary files); `Lua::sandbox(true)` additionally freezes the stdlib/globals. The only
//! capabilities are the ones on the `host` table, each gated by the shared [`Host`].
//! Wall-clock is enforced by a Luau interrupt that errors past a deadline; memory via
//! `set_memory_limit` where the allocator supports it (best-effort on Luau).
//!
//! Handler contract: `main.lua` evaluates to a function — `return function(event, host) ...
//! end` — which we call with the event and the `host` table. (Returning the function, rather
//! than defining a global, keeps it compatible with the frozen sandbox globals.)

use std::time::{Duration, Instant};

use mlua::{Lua, LuaSerdeExt, MultiValue, SerializeOptions, Value as LuaValue, VmState};

use crate::engine::{Engine, HandlerError, HandlerResponse, Invocation};
use crate::host::{HttpRequest, SharedHost};

/// Convert a JSON value to Lua, mapping `null` to Lua `nil` (not mlua's default null
/// *sentinel* userdata) so idioms like `host.kv.get(k) or 0` work.
fn to_lua(lua: &Lua, v: &serde_json::Value) -> mlua::Result<LuaValue> {
    let opts = SerializeOptions::new()
        .serialize_none_to_null(false)
        .serialize_unit_to_null(false);
    lua.to_value_with(v, opts)
}

pub struct LuaEngine;

impl LuaEngine {
    pub fn new() -> Self {
        LuaEngine
    }
}

impl Engine for LuaEngine {
    fn run(&self, inv: &Invocation, host: SharedHost) -> Result<HandlerResponse, HandlerError> {
        let source = std::fs::read_to_string(inv.entry)
            .map_err(|e| HandlerError::Entry(inv.entry.display().to_string(), e.to_string()))?;

        let lua = Lua::new();
        // Best-effort memory cap: Luau manages its own allocator, so this may be unsupported.
        // ponytail: wall-clock interrupt is the hard stop; revisit if a Luau mem cap lands.
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
            Err(e) if started.elapsed() >= Duration::from_millis(inv.limits.wall_ms) => {
                let _ = e;
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
    let host_tbl = build_host(lua, host)?;
    lua.sandbox(true)?;

    let handle: mlua::Function = lua.load(source).set_name("main.lua").eval()?;
    let event_val = to_lua(lua, event)?;
    let ret: LuaValue = handle.call((event_val, host_tbl))?;
    Ok(lua.from_value(ret).unwrap_or(serde_json::Value::Null))
}

fn lua_err(e: crate::host::HostError) -> mlua::Error {
    mlua::Error::runtime(e.to_string())
}

/// Build the `host` table — the unified API, mirroring the JS `host` object exactly.
fn build_host(lua: &Lua, host: &SharedHost) -> mlua::Result<mlua::Table> {
    let host_tbl = lua.create_table()?;

    // kv
    let kv = lua.create_table()?;
    let h = host.clone();
    kv.set(
        "get",
        lua.create_function(move |lua, key: String| {
            let v = h.borrow().kv_get(&key).map_err(lua_err)?;
            to_lua(lua, &v)
        })?,
    )?;
    let h = host.clone();
    kv.set(
        "set",
        lua.create_function(move |lua, (key, val): (String, LuaValue)| {
            let v: serde_json::Value = lua.from_value(val)?;
            h.borrow_mut().kv_set(&key, v).map_err(lua_err)
        })?,
    )?;
    let h = host.clone();
    kv.set(
        "all",
        lua.create_function(move |lua, ()| {
            let v = h.borrow().kv_all().map_err(lua_err)?;
            to_lua(lua, &v)
        })?,
    )?;
    host_tbl.set("kv", kv)?;

    // http
    let http = lua.create_table()?;
    let h = host.clone();
    http.set(
        "fetch",
        lua.create_function(move |lua, req: LuaValue| {
            let v: serde_json::Value = lua.from_value(req)?;
            let request = HttpRequest::from_json(&v).map_err(lua_err)?;
            let resp = h.borrow_mut().http_fetch(request).map_err(lua_err)?;
            to_lua(lua, &resp.to_json())
        })?,
    )?;
    host_tbl.set("http", http)?;

    // fs
    let fs = lua.create_table()?;
    let h = host.clone();
    fs.set(
        "read",
        lua.create_function(move |lua, path: String| {
            let v = h.borrow().fs_read(&path).map_err(lua_err)?;
            to_lua(lua, &v)
        })?,
    )?;
    let h = host.clone();
    fs.set(
        "write",
        lua.create_function(move |_, (path, data): (String, String)| {
            h.borrow().fs_write(&path, &data).map_err(lua_err)
        })?,
    )?;
    let h = host.clone();
    fs.set(
        "list",
        lua.create_function(move |lua, prefix: Option<String>| {
            let v = h.borrow().fs_list(&prefix.unwrap_or_default()).map_err(lua_err)?;
            to_lua(lua, &v)
        })?,
    )?;
    host_tbl.set("fs", fs)?;

    // log
    let h = host.clone();
    host_tbl.set(
        "log",
        lua.create_function(move |_, (level, msg): (String, String)| {
            h.borrow_mut().log(&level, &msg);
            Ok(())
        })?,
    )?;

    // response(body) or response(code, body)
    let h = host.clone();
    host_tbl.set(
        "response",
        lua.create_function(move |lua, args: MultiValue| {
            let mut it = args.into_iter();
            let first = it.next().unwrap_or(LuaValue::Nil);
            let (status, body_val) = match it.next() {
                Some(body) => (lua_to_status(&first).unwrap_or(200), body),
                None => (200u16, first),
            };
            let body: serde_json::Value = lua.from_value(body_val)?;
            h.borrow_mut().set_response(status.clamp(100, 599), body);
            Ok(())
        })?,
    )?;

    // responseCode.* (http-crate-sourced status map)
    let codes = lua.to_value(&serde_json::Value::Object(crate::host::status_codes()))?;
    host_tbl.set("responseCode", codes)?;

    Ok(host_tbl)
}

fn lua_to_status(v: &LuaValue) -> Option<u16> {
    match v {
        LuaValue::Integer(i) => Some(*i as u16),
        LuaValue::Number(n) => Some(*n as u16),
        _ => None,
    }
}
