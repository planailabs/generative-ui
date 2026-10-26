//! `gui trigger`: what happens on surfaces, as reagent trigger events.
//!
//! Prints one reagent event (a JSON line) per
//! - **backflow** event (`events/`: UI submissions, handler `host.emit`s) not seen before, and
//! - **kv** change: a key of a surface's public KV view (`state/kv.json`, plus the stream's
//!   latest values with `--with-stream`; what `data-kv` binds) that differs from last time.
//!
//! `{"key","to","title","message","vars":{kind,surface,agent,deck,name,url,…}}` with
//! `kind: "backflow"` (+ `action,value,source,scope,file`) or `kind: "kv"` (+ `kv_key,old,new`).
//! Run it as a reagent **watch** trigger (default) or a **poll** trigger (`--once`). Its snapshot
//! lives in `$REAGENT_STATE` when set, so a restart neither repeats nor misses; the first run
//! takes what exists as the baseline.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use gui_agent::Workspace;
use gui_schema::{atomic_write, SurfacePath, STREAM_KV_KEY};
use gui_server::Index;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Default, Serialize, Deserialize)]
struct Snapshot {
    /// `surface id → flattened kv key → value`.
    kv: BTreeMap<String, BTreeMap<String, Value>>,
    /// `surface id → (millis, file)` of the newest backflow event reported.
    backflow: BTreeMap<String, (u64, String)>,
}

pub struct Opts {
    /// Only these surfaces (`agent/deck/surface`); empty = every discovered surface.
    pub surfaces: Vec<String>,
    /// Report KV changes.
    pub kv: bool,
    /// Report backflow events.
    pub backflow: bool,
    /// Only KV keys starting with one of these; empty = all.
    pub keys: Vec<String>,
    /// Only backflow actions starting with one of these; empty = all.
    pub actions: Vec<String>,
    /// Also report `stream.*` keys. Off by default: they are mostly the agent's own progress
    /// (`emit_stream`), which would message the agent about itself.
    pub with_stream: bool,
    /// reagent `to`: `new`, `running`, or a task id.
    pub to: String,
    /// Snapshot file; `None` = in memory only.
    pub state: Option<PathBuf>,
    /// Report once and exit (poll mode) instead of watching.
    pub once: bool,
}

fn prefixed(prefixes: &[String], s: &str) -> bool {
    prefixes.is_empty() || prefixes.iter().any(|p| s.starts_with(p.as_str()))
}

impl Opts {
    fn wants_key(&self, key: &str) -> bool {
        (self.with_stream || !key.starts_with(&format!("{STREAM_KV_KEY}."))) && prefixed(&self.keys, key)
    }
}

/// Top-level KV keys, with `stream` expanded to `stream.{name}` so each stream is its own key.
fn flatten(kv: serde_json::Map<String, Value>) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    for (k, v) in kv {
        match (k == STREAM_KV_KEY, v) {
            (true, Value::Object(streams)) => {
                for (name, v) in streams {
                    out.insert(format!("{STREAM_KV_KEY}.{name}"), v);
                }
            }
            (_, v) => {
                out.insert(k, v);
            }
        }
    }
    out
}

/// The surfaces to look at.
fn surfaces(root: &Path, opts: &Opts) -> Vec<SurfacePath> {
    let idx = Index::build(root);
    idx.agents
        .values()
        .flat_map(|a| a.decks.values())
        .flat_map(|d| d.ordered_surfaces())
        .map(|s| s.path.clone())
        .filter(|p| opts.surfaces.is_empty() || opts.surfaces.contains(&p.id()))
        .collect()
}

/// `vars` every event carries.
fn base_vars(p: &SurfacePath, kind: &str) -> serde_json::Map<String, Value> {
    let Value::Object(m) = json!({
        "kind": kind, "surface": p.id(), "agent": p.agent, "deck": p.deck, "name": p.surface,
        "url": p.url(),
    }) else {
        unreachable!()
    };
    m
}

/// Read the current state, print events for what's new since `last`, and return the state.
/// With `report: false` only the state is taken (the first run's baseline).
fn step(root: &Path, opts: &Opts, last: &Snapshot, report: bool) -> Result<Snapshot> {
    let ws = Workspace::new(root);
    let ms = crate::now().0;
    // Carry over what this run doesn't look at (other surfaces, a kind turned off), so a later
    // run with other flags doesn't replay it.
    let mut now = Snapshot { kv: last.kv.clone(), backflow: last.backflow.clone() };
    let mut events = Vec::new();
    for p in surfaces(root, opts) {
        let id = p.id();
        if opts.backflow {
            let seen = last.backflow.get(&id);
            let mut mark = seen.cloned();
            let mut evs = ws.read_backflow(&p, None).with_context(|| format!("read backflow of {id}"))?;
            evs.sort_by(|a, b| (a.millis, &a.file).cmp(&(b.millis, &b.file)));
            for be in evs {
                let at = (be.millis, be.file.clone());
                if seen.is_some_and(|s| at <= *s) {
                    continue;
                }
                mark = Some(at);
                let e = &be.event;
                if !report || !prefixed(&opts.actions, &e.action) {
                    continue;
                }
                let mut vars = base_vars(&p, "backflow");
                vars.insert("action".into(), e.action.clone().into());
                vars.insert("value".into(), e.value.clone());
                vars.insert("source".into(), e.source.clone().unwrap_or_else(|| "ui".into()).into());
                vars.insert("scope".into(), json!(e.scope));
                vars.insert("file".into(), be.file.clone().into());
                events.push(json!({
                    "key": format!("{id}#{}", be.file),
                    "to": opts.to,
                    "title": format!("{id}: {}", e.action),
                    "message": format!("{id} {}: {}", e.action, e.value),
                    "vars": vars,
                }));
            }
            if let Some(m) = mark {
                now.backflow.insert(id.clone(), m);
            }
        }
        if opts.kv {
            let kv: BTreeMap<_, _> = flatten(p.public_kv(root)).into_iter().filter(|(k, _)| opts.wants_key(k)).collect();
            let empty = BTreeMap::new();
            let before = last.kv.get(&id).unwrap_or(&empty);
            // Old keys outside the filter aren't "removed": a snapshot may predate other flags.
            let gone = before.keys().filter(|k| opts.wants_key(k) && !kv.contains_key(*k));
            for k in kv.keys().chain(gone) {
                let (o, n) = (before.get(k).unwrap_or(&Value::Null), kv.get(k).unwrap_or(&Value::Null));
                if !report || o == n {
                    continue;
                }
                let mut vars = base_vars(&p, "kv");
                vars.insert("kv_key".into(), k.clone().into());
                vars.insert("old".into(), o.clone());
                vars.insert("new".into(), n.clone());
                events.push(json!({
                    "key": format!("{id}#kv:{k}@{ms}"),
                    "to": opts.to,
                    "title": format!("{id}: {k} changed"),
                    "message": format!("{id} {k}: {o} → {n}"),
                    "vars": vars,
                }));
            }
            now.kv.insert(id, kv);
        }
    }
    for ev in events {
        println!("{ev}");
    }
    if let Some(file) = &opts.state {
        atomic_write(file, &serde_json::to_vec(&now)?).with_context(|| format!("write {}", file.display()))?;
    }
    Ok(now)
}

fn load(state: &Option<PathBuf>) -> Result<Option<Snapshot>> {
    let Some(file) = state else { return Ok(None) };
    match std::fs::read(file) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).with_context(|| format!("parse {}", file.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", file.display())),
    }
}

pub fn run(root: &Path, opts: Opts) -> Result<()> {
    use notify::{RecursiveMode, Watcher};
    let mut last = match load(&opts.state)? {
        Some(s) => step(root, &opts, &s, true)?,
        // No snapshot yet: what exists now is the baseline, not news.
        None => step(root, &opts, &Snapshot::default(), false)?,
    };
    if opts.once {
        return Ok(());
    }

    std::fs::create_dir_all(root).ok();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    eprintln!("trigger: watching {} (Ctrl-C to stop)…", root.display());
    while let Ok(res) = rx.recv() {
        if let Err(e) = res {
            eprintln!("watch error: {e}");
            continue;
        }
        // A publish/handler run fires a burst of fs events: settle, then diff once.
        while rx.recv_timeout(Duration::from_millis(200)).is_ok() {}
        last = step(root, &opts, &last, true)?;
    }
    Ok(())
}
