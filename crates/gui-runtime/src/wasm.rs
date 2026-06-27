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
use crate::host::SharedHost;

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

/// Bind the single host import `host_call(op_ptr,op_len,args_ptr,args_len) -> i64` over
/// [`Host::dispatch`](crate::host::Host::dispatch). A `HostError` becomes a trap.
fn link_host(linker: &mut Linker<Data>) -> wasmtime::Result<()> {
    linker.func_wrap(
        "host",
        "host_call",
        |mut caller: Caller<'_, Data>, op_ptr: i32, op_len: i32, args_ptr: i32, args_len: i32| -> Result64 {
            let op = read_guest(&mut caller, op_ptr, op_len)?;
            let args_json = read_guest(&mut caller, args_ptr, args_len)?;
            let args = serde_json::from_str(&args_json).unwrap_or(Value::Null);
            let result = caller.data().host.borrow_mut().dispatch(&op, &args)?;
            write_guest(&mut caller, result.to_string().as_bytes())
        },
    )?;
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
