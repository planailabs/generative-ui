//! WebAssembly engine (`wasmtime`, core modules).
//!
//! Sandbox posture: a core module instantiated with **only** the `host` import module — no
//! WASI, no ambient anything. Memory is capped by a `StoreLimits` resource limiter; wall-clock
//! by epoch interruption (a timer thread bumps the engine epoch past the deadline).
//!
//! ABI (core module, all strings are UTF-8 JSON passed as `(ptr, len)` into linear memory):
//!   guest exports: `memory`, `alloc(len:i32)->i32`, `handle(ev_ptr:i32, ev_len:i32)->i64`
//!     where the result packs `(out_ptr<<32 | out_len)` of the response-body JSON.
//!   host imports (module "host"): kv_get/kv_set/kv_all, http_fetch, fs_read/fs_write/fs_list,
//!     log, respond, status_codes — data-returning imports allocate in guest memory (via the
//!     guest's `alloc`) and return the same packed `(ptr<<32|len)`.
//! ponytail: a hand ABI over core wasm — the component model is cleaner but needs a guest
//! toolchain (cargo-component/wasm-tools) we don't have. Revisit if that toolchain lands.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use wasmtime::{Caller, Config, Engine as WtEngine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

use crate::engine::{Engine, HandlerError, HandlerResponse, Invocation};
use crate::host::{HttpRequest, SharedHost};

pub struct WasmEngine;

impl WasmEngine {
    pub fn new() -> Self {
        WasmEngine
    }
}

/// Store data: the shared host plus the memory limiter.
struct Data {
    host: SharedHost,
    limits: StoreLimits,
}

type Result64 = wasmtime::Result<i64>;

impl Engine for WasmEngine {
    fn run(&self, inv: &Invocation, host: SharedHost) -> Result<HandlerResponse, HandlerError> {
        let bytes = std::fs::read(inv.entry)
            .map_err(|e| HandlerError::Entry(inv.entry.display().to_string(), e.to_string()))?;

        let mut config = Config::new();
        config.epoch_interruption(true);
        let engine = WtEngine::new(&config).map_err(traperr)?;
        let module = Module::new(&engine, &bytes).map_err(|e| HandlerError::Guest(e.to_string()))?;

        let limits = StoreLimitsBuilder::new()
            .memory_size((inv.limits.mem_mb as usize).saturating_mul(1024 * 1024))
            .build();
        let mut store = Store::new(&engine, Data { host, limits });
        store.limiter(|d| &mut d.limits);
        store.set_epoch_deadline(1);

        // Wall-clock guard: a timer thread bumps the epoch once the deadline passes, unless we
        // signal completion first (via the condvar) — so a fast handler doesn't wait out the
        // budget.
        let done = Arc::new((Mutex::new(false), Condvar::new()));
        let timer = {
            let engine = engine.clone();
            let done = done.clone();
            let wall = Duration::from_millis(inv.limits.wall_ms);
            std::thread::spawn(move || {
                let (lock, cv) = &*done;
                let guard = lock.lock().unwrap();
                let (_g, res) = cv.wait_timeout_while(guard, wall, |finished| !*finished).unwrap();
                if res.timed_out() {
                    engine.increment_epoch();
                }
            })
        };

        let started = Instant::now();
        let result = (|| {
            let mut linker = Linker::new(&engine);
            link_host(&mut linker)?;
            let instance = linker.instantiate(&mut store, &module)?;
            run_handler_call(&mut store, &instance, &inv.event)
        })();
        // Signal completion so the timer wakes immediately, then reap it.
        {
            let (lock, cv) = &*done;
            *lock.lock().unwrap() = true;
            cv.notify_all();
        }
        let _ = timer.join();

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

fn traperr(e: wasmtime::Error) -> HandlerError {
    HandlerError::Guest(e.to_string())
}

/// Allocate `bytes.len()` in guest memory (via the guest's `alloc`), copy in, return packed
/// `(ptr<<32 | len)`.
fn write_guest(caller: &mut Caller<'_, Data>, bytes: &[u8]) -> Result64 {
    let alloc = caller
        .get_export("alloc")
        .and_then(|e| e.into_func())
        .ok_or_else(|| wasmtime::Error::msg("guest missing alloc export"))?
        .typed::<i32, i32>(&caller)?;
    let ptr = alloc.call(&mut *caller, bytes.len() as i32)?;
    let mem = memory(caller)?;
    mem.write(&mut *caller, ptr as usize, bytes)?;
    Ok(((ptr as i64) << 32) | (bytes.len() as i64))
}

fn read_guest(caller: &mut Caller<'_, Data>, ptr: i32, len: i32) -> wasmtime::Result<String> {
    let mem = memory(caller)?;
    let data = mem.data(&caller);
    let (start, end) = (ptr as usize, ptr as usize + len as usize);
    let slice = data
        .get(start..end)
        .ok_or_else(|| wasmtime::Error::msg("guest pointer out of bounds"))?;
    Ok(String::from_utf8_lossy(slice).into_owned())
}

fn memory(caller: &mut Caller<'_, Data>) -> wasmtime::Result<wasmtime::Memory> {
    caller
        .get_export("memory")
        .and_then(|e| e.into_memory())
        .ok_or_else(|| wasmtime::Error::msg("guest missing memory export"))
}

/// Bind the `host.*` imports. A `HostError` becomes a trap (aborting the guest).
fn link_host(linker: &mut Linker<Data>) -> wasmtime::Result<()> {
    // data-returning: read input str, run host op → JSON string, allocate in guest.
    linker.func_wrap("host", "kv_get", |mut caller: Caller<'_, Data>, p: i32, l: i32| -> Result64 {
        let key = read_guest(&mut caller, p, l)?;
        let v = caller.data().host.borrow().kv_get(&key)?;
        write_guest(&mut caller, v.to_string().as_bytes())
    })?;
    linker.func_wrap(
        "host",
        "kv_set",
        |mut caller: Caller<'_, Data>, kp: i32, kl: i32, vp: i32, vl: i32| -> wasmtime::Result<()> {
            let key = read_guest(&mut caller, kp, kl)?;
            let val = read_guest(&mut caller, vp, vl)?;
            let v: Value = serde_json::from_str(&val).unwrap_or(Value::Null);
            caller.data().host.borrow_mut().kv_set(&key, v)?;
            Ok(())
        },
    )?;
    linker.func_wrap("host", "kv_all", |mut caller: Caller<'_, Data>| -> Result64 {
        let v = caller.data().host.borrow().kv_all()?;
        write_guest(&mut caller, v.to_string().as_bytes())
    })?;
    linker.func_wrap("host", "http_fetch", |mut caller: Caller<'_, Data>, p: i32, l: i32| -> Result64 {
        let req_json = read_guest(&mut caller, p, l)?;
        let v: Value = serde_json::from_str(&req_json)?;
        let req = HttpRequest::from_json(&v)?;
        let resp = caller.data().host.borrow_mut().http_fetch(req)?;
        write_guest(&mut caller, resp.to_json().to_string().as_bytes())
    })?;
    linker.func_wrap("host", "fs_read", |mut caller: Caller<'_, Data>, p: i32, l: i32| -> Result64 {
        let path = read_guest(&mut caller, p, l)?;
        let v = caller.data().host.borrow().fs_read(&path)?;
        write_guest(&mut caller, v.to_string().as_bytes())
    })?;
    linker.func_wrap(
        "host",
        "fs_write",
        |mut caller: Caller<'_, Data>, pp: i32, pl: i32, dp: i32, dl: i32| -> wasmtime::Result<()> {
            let path = read_guest(&mut caller, pp, pl)?;
            let data = read_guest(&mut caller, dp, dl)?;
            caller.data().host.borrow().fs_write(&path, &data)?;
            Ok(())
        },
    )?;
    linker.func_wrap("host", "fs_list", |mut caller: Caller<'_, Data>, p: i32, l: i32| -> Result64 {
        let prefix = read_guest(&mut caller, p, l)?;
        let v = caller.data().host.borrow().fs_list(&prefix)?;
        write_guest(&mut caller, v.to_string().as_bytes())
    })?;
    linker.func_wrap(
        "host",
        "log",
        |mut caller: Caller<'_, Data>, lp: i32, ll: i32, mp: i32, ml: i32| -> wasmtime::Result<()> {
            let level = read_guest(&mut caller, lp, ll)?;
            let msg = read_guest(&mut caller, mp, ml)?;
            caller.data().host.borrow_mut().log(&level, &msg);
            Ok(())
        },
    )?;
    linker.func_wrap(
        "host",
        "respond",
        |mut caller: Caller<'_, Data>, status: i32, bp: i32, bl: i32| -> wasmtime::Result<()> {
            let body_json = read_guest(&mut caller, bp, bl)?;
            let body: Value = serde_json::from_str(&body_json).unwrap_or(Value::Null);
            caller
                .data()
                .host
                .borrow_mut()
                .set_response((status.clamp(100, 599)) as u16, body);
            Ok(())
        },
    )?;
    linker.func_wrap("host", "status_codes", |mut caller: Caller<'_, Data>| -> Result64 {
        let codes = Value::Object(crate::host::status_codes());
        write_guest(&mut caller, codes.to_string().as_bytes())
    })?;
    Ok(())
}

/// Marshal the event into guest memory, call `handle`, read back the response-body string.
fn run_handler_call(
    store: &mut Store<Data>,
    instance: &wasmtime::Instance,
    event: &Value,
) -> wasmtime::Result<Value> {
    let event_str = event.to_string();
    let alloc = instance.get_typed_func::<i32, i32>(&mut *store, "alloc")?;
    let memory = instance
        .get_memory(&mut *store, "memory")
        .ok_or_else(|| wasmtime::Error::msg("guest missing memory export"))?;
    let ev_ptr = alloc.call(&mut *store, event_str.len() as i32)?;
    memory.write(&mut *store, ev_ptr as usize, event_str.as_bytes())?;

    let handle = instance.get_typed_func::<(i32, i32), i64>(&mut *store, "handle")?;
    let packed = handle.call(&mut *store, (ev_ptr, event_str.len() as i32))?;
    let out_ptr = (packed >> 32) as u32 as usize;
    let out_len = (packed & 0xffff_ffff) as usize;
    let mut buf = vec![0u8; out_len];
    memory.read(&mut *store, out_ptr, &mut buf)?;
    Ok(serde_json::from_slice(&buf).unwrap_or(Value::Null))
}
