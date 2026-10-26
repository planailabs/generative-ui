//! `gui kv-trigger`: surface KV changes as reagent trigger events.
//!
//! Diffs each surface's public KV view (`state/kv.json`, plus the stream's latest values with
//! `--with-stream`; i.e. what `data-kv` binds) against a snapshot and prints one reagent event per changed key:
//! `{"key","to","title","message","vars":{surface,agent,deck,name,kv_key,old,new,url}}`.
//! Run it as a reagent **watch** trigger (default) or a **poll** trigger (`--once`). The
//! snapshot lives in `$REAGENT_STATE` when set, so a restart neither repeats nor misses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use gui_schema::{atomic_write, SurfacePath, STREAM_KV_KEY};
use gui_server::Index;
use serde_json::{json, Value};

/// `surface id → flattened kv key → value`.
type Snapshot = BTreeMap<String, BTreeMap<String, Value>>;

pub struct Opts {
    /// Only these surfaces (`agent/deck/surface`); empty = every discovered surface.
    pub surfaces: Vec<String>,
    /// Only KV keys starting with one of these; empty = all.
    pub keys: Vec<String>,
    /// reagent `to`: `new`, `running`, or a task id.
    pub to: String,
    /// Snapshot file; `None` = in memory only (first run is a silent baseline).
    pub state: Option<PathBuf>,
    /// Diff once and exit (poll mode) instead of watching.
    pub once: bool,
    /// Also report `stream.*` keys. Off by default: they are mostly the agent's own progress
    /// (`emit_stream`), which would message the agent about itself.
    pub with_stream: bool,
}

impl Opts {
    fn wants(&self, key: &str) -> bool {
        (self.with_stream || !key.starts_with(&format!("{STREAM_KV_KEY}.")))
            && (self.keys.is_empty() || self.keys.iter().any(|p| key.starts_with(p.as_str())))
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

fn current(root: &Path, opts: &Opts) -> Snapshot {
    let idx = Index::build(root);
    let mut snap = Snapshot::new();
    for agent in idx.agents.values() {
        for deck in agent.decks.values() {
            for s in deck.ordered_surfaces() {
                let id = s.path.id();
                if !opts.surfaces.is_empty() && !opts.surfaces.contains(&id) {
                    continue;
                }
                let kv: BTreeMap<_, _> =
                    flatten(s.path.public_kv(root)).into_iter().filter(|(k, _)| opts.wants(k)).collect();
                snap.insert(id, kv);
            }
        }
    }
    snap
}

/// One reagent event per key whose value differs between `old` and `new` (removed → `null`).
fn diff(old: &Snapshot, new: &Snapshot, to: &str, ms: u64) -> Vec<Value> {
    let empty = BTreeMap::new();
    let mut out = Vec::new();
    for (id, kv) in new {
        let before = old.get(id).unwrap_or(&empty);
        let keys = kv.keys().chain(before.keys().filter(|k| !kv.contains_key(*k)));
        for k in keys {
            let (o, n) = (before.get(k).unwrap_or(&Value::Null), kv.get(k).unwrap_or(&Value::Null));
            if o == n {
                continue;
            }
            let (agent, deck, name) = match id.splitn(3, '/').collect::<Vec<_>>()[..] {
                [a, d, s] => (a, d, s),
                _ => continue,
            };
            let url = SurfacePath::new(agent, deck, name).map(|p| p.url()).unwrap_or_default();
            out.push(json!({
                "key": format!("{id}#{k}@{ms}"),
                "to": to,
                "title": format!("{id}: {k} changed"),
                "message": format!("{id} {k}: {o} → {n}"),
                "vars": {
                    "surface": id, "agent": agent, "deck": deck, "name": name,
                    "kv_key": k, "old": o, "new": n, "url": url,
                },
            }));
        }
    }
    out
}

fn load(state: &Option<PathBuf>) -> Result<Option<Snapshot>> {
    let Some(file) = state else { return Ok(None) };
    match std::fs::read(file) {
        Ok(b) => Ok(Some(serde_json::from_slice(&b).with_context(|| format!("parse {}", file.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("read {}", file.display())),
    }
}

fn save(state: &Option<PathBuf>, snap: &Snapshot) -> Result<()> {
    if let Some(file) = state {
        atomic_write(file, &serde_json::to_vec(snap)?).with_context(|| format!("write {}", file.display()))?;
    }
    Ok(())
}

/// Diff against `last`, print the events, persist, and return the new snapshot.
fn step(root: &Path, opts: &Opts, last: &Snapshot) -> Result<Snapshot> {
    let now = current(root, opts);
    let ms = crate::now().0;
    // Same filter on the old side, so a snapshot taken with other --key flags isn't "removed".
    let last: Snapshot = last
        .iter()
        .map(|(id, kv)| (id.clone(), kv.iter().filter(|(k, _)| opts.wants(k)).map(|(k, v)| (k.clone(), v.clone())).collect()))
        .collect();
    for ev in diff(&last, &now, &opts.to, ms) {
        println!("{ev}");
    }
    save(&opts.state, &now)?;
    Ok(now)
}

pub fn run(root: &Path, opts: Opts) -> Result<()> {
    use notify::{RecursiveMode, Watcher};
    // No snapshot yet: take the current view as the baseline instead of reporting all of it.
    let mut last = match load(&opts.state)? {
        Some(s) => s,
        None => {
            let s = current(root, &opts);
            save(&opts.state, &s)?;
            s
        }
    };
    last = step(root, &opts, &last)?;
    if opts.once {
        return Ok(());
    }

    std::fs::create_dir_all(root).ok();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    eprintln!("kv-trigger: watching {} (Ctrl-C to stop)…", root.display());
    while let Ok(res) = rx.recv() {
        if let Err(e) = res {
            eprintln!("watch error: {e}");
            continue;
        }
        // A publish/handler run fires a burst of fs events: settle, then diff once.
        while rx.recv_timeout(Duration::from_millis(200)).is_ok() {}
        last = step(root, &opts, &last)?;
    }
    Ok(())
}
