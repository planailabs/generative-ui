//! Agent-facing API for the Generative UI Suite.
//!
//! Everything goes through the artifacts directory — there is no submission API. An agent:
//!
//! - **publishes** a surface version with [`Workspace::publish`] (build `<ui-*>` HTML from a
//!   [`Component`] tree, mint a version id, atomically write `v/{version}/`),
//! - **wires a deck** with [`Workspace::define_deck`],
//! - **reads or waits on backflow** with [`Workspace::read_backflow`] /
//!   [`Workspace::wait_for_backflow`].
//!
//! ```no_run
//! use gui_agent::{Workspace, SurfaceBuilder};
//! use gui_schema::{Component, SurfacePath};
//! use serde_json::json;
//!
//! let ws = Workspace::new("artifacts");
//! let path = SurfacePath::new("demo", "main", "sales").unwrap();
//! ws.publish(&path, &SurfaceBuilder::new("Sales", "Revenue overview")
//!     .heading("Sales")
//!     .button("Refresh", "refresh", json!(null))
//!     .nav("settings", Some("Settings"))).unwrap();
//! let event = ws.wait_for_backflow(&path, None, std::time::Duration::from_secs(30)).unwrap();
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use gui_schema::{
    atomic_write, is_valid_slug, render_all, AgentInfo, Component, DeckManifest, Event, Manifest,
    NavEdge, SurfacePath, Version, SURFACE_SCHEMA_VERSION,
};

// Re-export the schema types agents need, so one `use gui_agent::*` is enough.
pub use gui_schema::{
    AgentInfo as AgentInfoSchema, Component as UiComponent, DeckManifest as Deck, NavEdge as Nav,
    SurfacePath as Surface, Version as SurfaceVersion,
};

/// Current time as `(unix_millis, rfc3339)`.
fn now() -> (u64, String) {
    use time::{format_description::well_known::Rfc3339, OffsetDateTime};
    let n = OffsetDateTime::now_utc();
    (
        (n.unix_timestamp_nanos() / 1_000_000) as u64,
        n.format(&Rfc3339).unwrap_or_default(),
    )
}

/// Builder for a surface version: the manifest fields plus the body (a [`Component`] tree
/// or raw HTML).
#[derive(Clone, Debug, Default)]
pub struct SurfaceBuilder {
    title: String,
    alt_text: String,
    components: Vec<Component>,
    raw_html: Option<String>,
    nav: Vec<NavEdge>,
    actions: Vec<String>,
    metadata: serde_json::Map<String, serde_json::Value>,
}

impl SurfaceBuilder {
    pub fn new(title: impl Into<String>, alt_text: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            alt_text: alt_text.into(),
            ..Default::default()
        }
    }

    /// Append a component to the body.
    pub fn component(mut self, c: Component) -> Self {
        self.components.push(c);
        self
    }

    /// Append several components.
    pub fn components(mut self, cs: impl IntoIterator<Item = Component>) -> Self {
        self.components.extend(cs);
        self
    }

    /// Use raw HTML for the body instead of the component tree (the floor escape hatch).
    pub fn html(mut self, raw: impl Into<String>) -> Self {
        self.raw_html = Some(raw.into());
        self
    }

    /// Convenience: append a heading.
    pub fn heading(self, text: impl Into<String>) -> Self {
        self.component(Component::Heading {
            level: None,
            text: text.into(),
        })
    }

    /// Convenience: append a backflow button.
    pub fn button(
        self,
        label: impl Into<String>,
        action: impl Into<String>,
        value: serde_json::Value,
    ) -> Self {
        let action = action.into();
        self.action(action.clone()).component(Component::Button {
            label: label.into(),
            action: Some(action),
            value,
        })
    }

    /// Declare a navigation edge to another surface in the deck.
    pub fn nav(mut self, to: impl Into<String>, label: Option<&str>) -> Self {
        self.nav.push(NavEdge {
            to: to.into(),
            label: label.map(str::to_string),
        });
        self
    }

    /// Declare a backflow action (also added automatically by [`SurfaceBuilder::button`]).
    pub fn action(mut self, a: impl Into<String>) -> Self {
        let a = a.into();
        if !self.actions.contains(&a) {
            self.actions.push(a);
        }
        self
    }

    /// Set a free-form metadata key.
    pub fn meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    fn body_html(&self) -> String {
        match &self.raw_html {
            Some(h) => h.clone(),
            None => render_all(&self.components),
        }
    }

    fn manifest(&self, created_at: String) -> Manifest {
        Manifest {
            schema_version: SURFACE_SCHEMA_VERSION.to_string(),
            title: self.title.clone(),
            alt_text: self.alt_text.clone(),
            created_at: Some(created_at),
            backflow: (!self.actions.is_empty()).then(|| gui_schema::Backflow {
                actions: self.actions.clone(),
            }),
            nav: self.nav.clone(),
            metadata: self.metadata.clone(),
        }
    }
}

/// A backflow event read from a surface's `events/` directory.
#[derive(Clone, Debug)]
pub struct BackflowEvent {
    /// The `{unix-ms}` prefix of the file name (monotone-ish ordering key).
    pub millis: u64,
    pub file: String,
    pub event: Event,
}

/// A handle to an artifacts directory.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Publish a new version of a surface (latest-wins). Returns the minted [`Version`].
    pub fn publish(&self, path: &SurfacePath, builder: &SurfaceBuilder) -> Result<Version> {
        let (ms, iso) = now();
        // Avoid colliding with an existing version dir on rapid publishes.
        let mut version = Version(ms);
        while path.version_dir(&self.root, version).exists() {
            version = Version(version.0 + 1);
        }
        let dir = path.version_dir(&self.root, version);

        let manifest = builder.manifest(iso);
        let manifest_json =
            serde_json::to_vec_pretty(&manifest).context("serialize frame.json")?;
        atomic_write(&dir.join("frame.json"), &manifest_json).context("write frame.json")?;
        atomic_write(&dir.join("index.html"), builder.body_html().as_bytes())
            .context("write index.html")?;
        Ok(version)
    }

    /// Write/replace an agent's `agent.json`.
    pub fn define_agent(&self, agent: &str, info: &AgentInfo) -> Result<()> {
        if !is_valid_slug(agent) {
            bail!("invalid agent slug {agent:?}");
        }
        let bytes = serde_json::to_vec_pretty(info)?;
        atomic_write(&self.root.join(agent).join("agent.json"), &bytes)?;
        Ok(())
    }

    /// Write/replace a deck's `deck.json` to wire the navigable deck.
    pub fn define_deck(&self, agent: &str, deck: &str, manifest: &DeckManifest) -> Result<()> {
        if !is_valid_slug(agent) || !is_valid_slug(deck) {
            bail!("invalid agent/deck slug");
        }
        let bytes = serde_json::to_vec_pretty(manifest)?;
        atomic_write(&self.root.join(agent).join(deck).join("deck.json"), &bytes)?;
        Ok(())
    }

    /// The live (newest) version id of a surface, if any.
    pub fn live_version(&self, path: &SurfacePath) -> Option<Version> {
        let vdir = path.dir(&self.root).join("v");
        std::fs::read_dir(vdir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| Version::parse(e.file_name().to_str()?).ok())
            .filter(|v| path.version_dir(&self.root, *v).join("frame.json").is_file())
            .max()
    }

    /// Read a specific version's `(manifest, index.html)`.
    pub fn read_version(&self, path: &SurfacePath, version: Version) -> Result<(Manifest, String)> {
        let dir = path.version_dir(&self.root, version);
        let manifest: Manifest = serde_json::from_slice(&std::fs::read(dir.join("frame.json"))?)
            .context("parse frame.json")?;
        let html = std::fs::read_to_string(dir.join("index.html"))?;
        Ok((manifest, html))
    }

    /// All backflow events for a surface with `millis > since` (or all when `since` is
    /// `None`), sorted oldest-first.
    pub fn read_backflow(&self, path: &SurfacePath, since: Option<u64>) -> Result<Vec<BackflowEvent>> {
        let dir = path.events_dir(&self.root);
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e).context("read events dir"),
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !name.ends_with(".json") {
                continue;
            }
            let millis = match name.split('-').next().and_then(|s| s.parse::<u64>().ok()) {
                Some(m) => m,
                None => continue,
            };
            if let Some(s) = since {
                if millis <= s {
                    continue;
                }
            }
            let event: Event = match std::fs::read(entry.path())
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
            {
                Some(ev) => ev,
                None => continue, // skip partial/corrupt
            };
            out.push(BackflowEvent {
                millis,
                file: name,
                event,
            });
        }
        out.sort_by(|a, b| a.millis.cmp(&b.millis).then_with(|| a.file.cmp(&b.file)));
        Ok(out)
    }

    /// Block until a backflow event newer than `since` arrives (or `timeout` elapses).
    ///
    /// `since = None` waits for events strictly after *now*. It is event-driven on a
    /// filesystem watcher (near-zero idle CPU); a slow fallback re-scan bounds latency if a
    /// watcher event is ever missed.
    pub fn wait_for_backflow(
        &self,
        path: &SurfacePath,
        since: Option<u64>,
        timeout: Duration,
    ) -> Result<Option<BackflowEvent>> {
        use notify::{RecursiveMode, Watcher};

        // Fixed cursor: every poll re-reads all events with millis > cursor and returns the
        // oldest. Simple and correct; O(events) per poll (fine for modest event counts).
        let cursor = since.unwrap_or_else(|| now().0);

        // Already-present newer event? Return immediately.
        if let Some(ev) = self.read_backflow(path, Some(cursor))?.into_iter().next() {
            return Ok(Some(ev));
        }

        // Ensure the dir exists so the watcher has something to watch.
        let dir = path.events_dir(&self.root);
        std::fs::create_dir_all(&dir).context("create events dir")?;

        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        })
        .and_then(|mut w| {
            w.watch(&dir, RecursiveMode::NonRecursive)?;
            Ok(w)
        });
        // Hold the watcher (if any) for the duration; ignore setup failure (poll still works).
        let _watcher = watcher.ok();

        // Event-driven: block on the watcher channel and only re-scan when it signals. A long
        // fallback (not a tight poll) bounds latency if a filesystem event is ever missed, so
        // idle CPU is ~zero instead of scanning 4×/second.
        const FALLBACK: Duration = Duration::from_secs(10);
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            match rx.recv_timeout(remaining.min(FALLBACK)) {
                // A watcher event arrived: drain any others so a burst is one scan, then scan.
                Ok(_) => while rx.try_recv().is_ok() {},
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {} // fallback re-scan
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    // Watcher gone; fall back to a slow poll until the deadline.
                    std::thread::sleep(remaining.min(FALLBACK));
                }
            }
            if let Some(ev) = self.read_backflow(path, Some(cursor))?.into_iter().next() {
                return Ok(Some(ev));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gui_schema::EVENT_SCHEMA_VERSION;
    use serde_json::json;

    fn ws() -> (tempfile::TempDir, Workspace) {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::new(tmp.path());
        (tmp, ws)
    }

    #[test]
    fn publish_latest_wins_and_keeps_history() {
        let (_t, ws) = ws();
        let p = SurfacePath::new("demo", "main", "sales").unwrap();

        let v1 = ws
            .publish(&p, &SurfaceBuilder::new("Sales v1", "alt").heading("One"))
            .unwrap();
        let v2 = ws
            .publish(
                &p,
                &SurfaceBuilder::new("Sales v2", "alt")
                    .heading("Two")
                    .button("Go", "go", json!({"x": 1}))
                    .nav("settings", Some("Settings")),
            )
            .unwrap();
        assert!(v2 > v1, "newer version id");
        assert_eq!(ws.live_version(&p), Some(v2));

        let (manifest, html) = ws.read_version(&p, v2).unwrap();
        assert_eq!(manifest.title, "Sales v2");
        assert_eq!(manifest.nav[0].to, "settings");
        assert_eq!(manifest.backflow.unwrap().actions, ["go"]);
        assert!(html.contains(r#"<ui-button data-action="go""#));
        assert!(html.contains("<ui-heading"));

        // History retained: v1 still readable.
        assert_eq!(ws.read_version(&p, v1).unwrap().0.title, "Sales v1");
    }

    #[test]
    fn define_deck_writes_manifest() {
        let (_t, ws) = ws();
        ws.define_deck(
            "demo",
            "main",
            &DeckManifest {
                name: Some("Demo".into()),
                entry: Some("sales".into()),
                order: vec!["sales".into(), "settings".into()],
            },
        )
        .unwrap();
        let raw = std::fs::read(ws.root().join("demo/main/deck.json")).unwrap();
        let back: DeckManifest = serde_json::from_slice(&raw).unwrap();
        assert_eq!(back.entry.as_deref(), Some("sales"));
        assert!(ws.define_deck("Bad", "main", &DeckManifest::default()).is_err());
    }

    #[test]
    fn read_backflow_filters_by_since() {
        let (_t, ws) = ws();
        let p = SurfacePath::new("demo", "main", "sales").unwrap();
        let dir = p.events_dir(ws.root());
        for ms in [100u64, 200, 300] {
            let ev = Event {
                schema_version: EVENT_SCHEMA_VERSION.into(),
                action: "ping".into(),
                component: None,
                value: json!(null),
                surface: p.id(),
                version: None,
                received_at: "t".into(),
            };
            atomic_write(
                &dir.join(format!("{ms}-ping.json")),
                &serde_json::to_vec(&ev).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(ws.read_backflow(&p, None).unwrap().len(), 3);
        let after = ws.read_backflow(&p, Some(150)).unwrap();
        assert_eq!(after.len(), 2);
        assert_eq!(after[0].millis, 200);
    }

    #[test]
    fn wait_for_backflow_unblocks_on_new_event() {
        let (_t, ws) = ws();
        let p = SurfacePath::new("demo", "main", "sales").unwrap();
        let dir = p.events_dir(ws.root());
        std::fs::create_dir_all(&dir).unwrap();

        let writer_dir = dir.clone();
        let pid = p.id();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let ms = now().0 + 5_000; // safely after the wait cursor
            let ev = Event {
                schema_version: EVENT_SCHEMA_VERSION.into(),
                action: "submit".into(),
                component: Some("ui-form".into()),
                value: json!({"email": "a@b.c"}),
                surface: pid,
                version: None,
                received_at: "t".into(),
            };
            atomic_write(
                &writer_dir.join(format!("{ms}-submit.json")),
                &serde_json::to_vec(&ev).unwrap(),
            )
            .unwrap();
        });

        let got = ws
            .wait_for_backflow(&p, Some(0), Duration::from_secs(5))
            .unwrap();
        handle.join().unwrap();
        let got = got.expect("event arrived before timeout");
        assert_eq!(got.event.action, "submit");
    }

    #[test]
    fn wait_for_backflow_times_out() {
        let (_t, ws) = ws();
        let p = SurfacePath::new("demo", "main", "empty").unwrap();
        let got = ws
            .wait_for_backflow(&p, Some(0), Duration::from_millis(300))
            .unwrap();
        assert!(got.is_none());
    }
}
