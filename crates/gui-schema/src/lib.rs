//! Shared types for the Generative UI Suite — the single source of truth for the
//! on-disk contract.
//!
//! Everything an agent writes and the server reads lives here: the [`Manifest`]
//! (`frame.json`), the [`DeckManifest`] (`deck.json`), the backflow [`Event`], the
//! [`SurfacePath`] identity, the [`Version`] ordering, and the [`Component`] tree that
//! renders to `<ui-*>` HTML. Reserved manifest keys are a closed set
//! (`deny_unknown_fields`); free-form data lives only under `metadata`.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically write `bytes` to `path`: write a uniquely-named temp file in the same
/// directory, then `rename` it over the target. Concurrent readers (agents polling the
/// directory) never observe a partial file. Parent directories are created as needed.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("tmp");
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = parent.join(format!(".{fname}.tmp.{}.{n}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    // rename is atomic within a filesystem; clean up the temp file on failure.
    match std::fs::rename(&tmp, path) {
        Ok(()) => {
            tracing::trace!(path = %path.display(), bytes = bytes.len(), "atomic write");
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub const SURFACE_SCHEMA_VERSION: &str = "generative-ui/surface.v1";
pub const EVENT_SCHEMA_VERSION: &str = "generative-ui/event.v1";
/// The default deck every agent has when none is named.
pub const DEFAULT_DECK: &str = "main";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SchemaError {
    #[error("invalid slug {0:?}: must match [a-z0-9][a-z0-9-]* and be 1..=128 chars")]
    InvalidSlug(String),
    #[error("invalid version id {0:?}: must be decimal digits")]
    InvalidVersion(String),
}

/// True for a string usable as a path/URL segment without escaping or traversal risk.
///
/// Slugs are the trust boundary for filesystem paths — keep this strict.
pub fn is_valid_slug(s: &str) -> bool {
    let bytes = s.as_bytes();
    !s.is_empty()
        && s.len() <= 128
        && bytes[0].is_ascii_lowercase() | bytes[0].is_ascii_digit()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

fn check_slug(s: &str) -> Result<String, SchemaError> {
    if is_valid_slug(s) {
        Ok(s.to_string())
    } else {
        Err(SchemaError::InvalidSlug(s.to_string()))
    }
}

/// A surface version id: unix-milliseconds, newest = live. Sorts numerically.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Version(pub u64);

impl Version {
    pub fn parse(s: &str) -> Result<Self, SchemaError> {
        // Reject leading zeros / signs / whitespace so the dir name round-trips exactly.
        if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SchemaError::InvalidVersion(s.to_string()));
        }
        s.parse::<u64>()
            .map(Version)
            .map_err(|_| SchemaError::InvalidVersion(s.to_string()))
    }

    pub fn dir_name(&self) -> String {
        self.0.to_string()
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The identity of a surface: `agent/deck/surface`. The path *is* the identity, so these
/// slugs are never duplicated inside a manifest.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SurfacePath {
    pub agent: String,
    pub deck: String,
    pub surface: String,
}

impl SurfacePath {
    pub fn new(
        agent: impl Into<String>,
        deck: impl Into<String>,
        surface: impl Into<String>,
    ) -> Result<Self, SchemaError> {
        Ok(Self {
            agent: check_slug(&agent.into())?,
            deck: check_slug(&deck.into())?,
            surface: check_slug(&surface.into())?,
        })
    }

    /// `agent/deck/surface` — the stable string id used in events and the index.
    pub fn id(&self) -> String {
        format!("{}/{}/{}", self.agent, self.deck, self.surface)
    }

    /// Canonical (deck-qualified) URL path, trailing slash.
    pub fn url(&self) -> String {
        format!("/{}/{}/{}/", self.agent, self.deck, self.surface)
    }

    /// `artifacts/{agent}/{deck}/{surface}`
    pub fn dir(&self, root: &Path) -> PathBuf {
        root.join(&self.agent).join(&self.deck).join(&self.surface)
    }

    /// `.../v/{version}`
    pub fn version_dir(&self, root: &Path, v: Version) -> PathBuf {
        self.dir(root).join("v").join(v.dir_name())
    }

    /// `.../events`
    pub fn events_dir(&self, root: &Path) -> PathBuf {
        self.dir(root).join("events")
    }

    /// `.../state` — server-written handler state (sibling of `v/` and `events/`; ignored by
    /// discovery, which only reads `v/`).
    pub fn state_dir(&self, root: &Path) -> PathBuf {
        self.dir(root).join("state")
    }

    /// `.../state/kv.json` — the materialized KV view folded from the backflow log.
    pub fn kv_path(&self, root: &Path) -> PathBuf {
        self.state_dir(root).join("kv.json")
    }
}

// ---------------------------------------------------------------------------
// frame.json
// ---------------------------------------------------------------------------

/// `frame.json` — the surface manifest. Closed top-level shape; new keys go in `metadata`.
#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: String,
    pub title: String,
    pub alt_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backflow: Option<Backflow>,
    /// Sandboxed handlers, keyed by backflow action. The server runs the matching handler
    /// when an interaction with that action arrives (opt-in via `--enable-handlers`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub handlers: BTreeMap<String, HandlerDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nav: Vec<NavEdge>,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, serde_json::Value>,
}

impl Manifest {
    pub fn new(title: impl Into<String>, alt_text: impl Into<String>) -> Self {
        Self {
            schema_version: SURFACE_SCHEMA_VERSION.to_string(),
            title: title.into(),
            alt_text: alt_text.into(),
            created_at: None,
            backflow: None,
            handlers: BTreeMap::new(),
            nav: Vec::new(),
            metadata: serde_json::Map::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers (sandboxed backflow logic)
// ---------------------------------------------------------------------------

/// Which guest engine runs a handler. The `entry` file extension follows by convention
/// (`main.js` / `main.lua` / `main.wasm`) but `lang` is authoritative.
#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HandlerLang {
    Js,
    Lua,
    Wasm,
}

/// One handler attached to a backflow action: the engine, the entry file (relative to the
/// surface version dir), and the capabilities/limits the sandbox grants it.
#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HandlerDef {
    pub lang: HandlerLang,
    pub entry: String,
    #[serde(default)]
    pub capabilities: Capabilities,
    #[serde(default)]
    pub limits: Limits,
}

/// What a handler is allowed to reach. Default-deny: an absent/empty field grants nothing.
#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Debug, PartialEq, Eq, Default)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Allowed outbound HTTP hosts (exact host match). Empty ⇒ no network.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http: Vec<String>,
    /// Read/write within the surface's jailed `state/` dir.
    #[serde(default)]
    pub fs: bool,
    /// Read/write the surface's KV view.
    #[serde(default)]
    pub kv: bool,
}

/// Hard resource limits enforced host-side, identically across all engines.
#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    /// Wall-clock budget for one invocation.
    #[serde(default = "Limits::default_wall_ms")]
    pub wall_ms: u64,
    /// Max guest heap.
    #[serde(default = "Limits::default_mem_mb")]
    pub mem_mb: u64,
    /// Max `http.fetch` calls per invocation.
    #[serde(default = "Limits::default_http_calls")]
    pub http_calls: u32,
    /// Max bytes of a single HTTP response body the host will read.
    #[serde(default = "Limits::default_resp_kb")]
    pub resp_kb: u64,
    /// Max bytes a single `fs.write` may write.
    #[serde(default = "Limits::default_fs_kb")]
    pub fs_kb: u64,
}

impl Limits {
    fn default_wall_ms() -> u64 {
        2000
    }
    fn default_mem_mb() -> u64 {
        32
    }
    fn default_http_calls() -> u32 {
        4
    }
    fn default_resp_kb() -> u64 {
        512
    }
    fn default_fs_kb() -> u64 {
        256
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            wall_ms: Self::default_wall_ms(),
            mem_mb: Self::default_mem_mb(),
            http_calls: Self::default_http_calls(),
            resp_kb: Self::default_resp_kb(),
            fs_kb: Self::default_fs_kb(),
        }
    }
}

#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Backflow {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
}

/// A navigation edge from this surface to another surface (by slug) in the deck.
#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct NavEdge {
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

// ---------------------------------------------------------------------------
// agent.json
// ---------------------------------------------------------------------------

/// `agent.json` — optional agent description at `artifacts/{agent}/agent.json`.
#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct AgentInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// deck.json
// ---------------------------------------------------------------------------

/// `deck.json` — optional deck wiring: a display name, the entry surface, and a prev/next
/// `order`. Absent ⇒ a flat deck ordered by first-seen version time.
#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Debug, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct DeckManifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
}

// ---------------------------------------------------------------------------
// backflow event
// ---------------------------------------------------------------------------

/// A backflow event, written by the server into `{surface}/events/{unix-ms}-{action}.json`.
#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub schema_version: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component: Option<String>,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub value: serde_json::Value,
    /// `agent/deck/surface`.
    pub surface: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub received_at: String,
}

impl Event {
    /// Filename for an event received at `received_ms`: `{ms}-{action-slug}.json`.
    /// The action is slugified so an arbitrary action string can't escape the directory.
    pub fn file_name(&self, received_ms: u64) -> String {
        format!("{}-{}.json", received_ms, slugify(&self.action))
    }
}

/// Lossy slugify for filenames: lowercase, non-`[a-z0-9-]` → `-`, collapsed, trimmed.
pub fn slugify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_dash = true; // trim leading dashes
    for c in s.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("event");
    }
    out
}

// ---------------------------------------------------------------------------
// Component tree → <ui-*> HTML
// ---------------------------------------------------------------------------

/// A UI component. Rendered to `<ui-*>` HTML by [`Component::render`]; the same elements
/// are upgraded live by the browser runtime. The single definition shared across the
/// stack (agents, MCP, server, browser).
#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Component {
    Heading {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        level: Option<u8>,
        text: String,
    },
    Text {
        text: String,
    },
    Button {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<String>,
        #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
        value: serde_json::Value,
    },
    Input {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_type: Option<String>,
    },
    Select {
        name: String,
        #[serde(default)]
        options: Vec<SelectOption>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    Checkbox {
        name: String,
        label: String,
        #[serde(default)]
        checked: bool,
    },
    Card {
        #[serde(default)]
        children: Vec<Component>,
    },
    Form {
        action: String,
        #[serde(default)]
        children: Vec<Component>,
    },
    Link {
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    DeckNav,
    /// Raw HTML escape hatch — passed through verbatim (author's responsibility).
    Html {
        html: String,
    },
}

#[derive(Serialize, Deserialize, schemars::JsonSchema, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SelectOption {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// Escape text for use in HTML element bodies and double-quoted attribute values.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn attr(name: &str, value: &str) -> String {
    format!(" {}=\"{}\"", name, esc(value))
}

impl Component {
    /// Render this component to `<ui-*>` HTML.
    pub fn render(&self) -> String {
        match self {
            Component::Heading { level, text } => {
                format!(
                    "<ui-heading{}>{}</ui-heading>",
                    attr("level", &level.unwrap_or(2).to_string()),
                    esc(text)
                )
            }
            Component::Text { text } => format!("<ui-text>{}</ui-text>", esc(text)),
            Component::Button {
                label,
                action,
                value,
            } => {
                let mut a = String::new();
                if let Some(action) = action {
                    a.push_str(&attr("data-action", action));
                }
                if !value.is_null() {
                    a.push_str(&attr("data-value", &value.to_string()));
                }
                format!("<ui-button{}>{}</ui-button>", a, esc(label))
            }
            Component::Input {
                name,
                label,
                placeholder,
                input_type,
            } => {
                let mut a = attr("name", name);
                if let Some(l) = label {
                    a.push_str(&attr("label", l));
                }
                if let Some(p) = placeholder {
                    a.push_str(&attr("placeholder", p));
                }
                if let Some(t) = input_type {
                    a.push_str(&attr("type", t));
                }
                format!("<ui-input{a}></ui-input>")
            }
            Component::Select {
                name,
                options,
                label,
            } => {
                let mut a = attr("name", name);
                if let Some(l) = label {
                    a.push_str(&attr("label", l));
                }
                let opts: String = options
                    .iter()
                    .map(|o| {
                        format!(
                            "<option{}>{}</option>",
                            attr("value", &o.value),
                            esc(o.label.as_deref().unwrap_or(&o.value))
                        )
                    })
                    .collect();
                format!("<ui-select{a}>{opts}</ui-select>")
            }
            Component::Checkbox {
                name,
                label,
                checked,
            } => {
                let c = if *checked { " checked" } else { "" };
                format!(
                    "<ui-checkbox{}{}>{}</ui-checkbox>",
                    attr("name", name),
                    c,
                    esc(label)
                )
            }
            Component::Card { children } => {
                format!("<ui-card>{}</ui-card>", render_all(children))
            }
            Component::Form { action, children } => {
                format!(
                    "<ui-form{}>{}</ui-form>",
                    attr("data-action", action),
                    render_all(children)
                )
            }
            Component::Link { to, label } => {
                format!(
                    "<ui-link{}>{}</ui-link>",
                    attr("to", to),
                    esc(label.as_deref().unwrap_or(to))
                )
            }
            Component::DeckNav => "<ui-deck-nav></ui-deck-nav>".to_string(),
            Component::Html { html } => html.clone(),
        }
    }
}

/// Render a list of components in order.
pub fn render_all(components: &[Component]) -> String {
    components.iter().map(Component::render).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn slug_validation() {
        assert!(is_valid_slug("demo"));
        assert!(is_valid_slug("sales-2"));
        assert!(is_valid_slug("9"));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug("-leading"));
        assert!(!is_valid_slug("Caps"));
        assert!(!is_valid_slug("has space"));
        assert!(!is_valid_slug("../escape"));
        assert!(!is_valid_slug("under_score"));
    }

    #[test]
    fn version_parse_and_order() {
        assert_eq!(Version::parse("1719500000000").unwrap(), Version(1719500000000));
        assert!(Version::parse("00").is_err());
        assert!(Version::parse("0x1").is_err());
        assert!(Version::parse("-1").is_err());
        assert!(Version(2) > Version(1));
        assert_eq!(Version(5).dir_name(), "5");
    }

    #[test]
    fn surface_path_identity_and_fs() {
        let p = SurfacePath::new("demo", "main", "sales").unwrap();
        assert_eq!(p.id(), "demo/main/sales");
        assert_eq!(p.url(), "/demo/main/sales/");
        let root = Path::new("/artifacts");
        assert_eq!(p.dir(root), Path::new("/artifacts/demo/main/sales"));
        assert_eq!(
            p.version_dir(root, Version(7)),
            Path::new("/artifacts/demo/main/sales/v/7")
        );
        assert_eq!(p.state_dir(root), Path::new("/artifacts/demo/main/sales/state"));
        assert_eq!(p.kv_path(root), Path::new("/artifacts/demo/main/sales/state/kv.json"));
        assert!(SurfacePath::new("demo", "main", "../x").is_err());
    }

    #[test]
    fn handlers_roundtrip_and_defaults() {
        // Full handler def round-trips.
        let mut handlers = BTreeMap::new();
        handlers.insert(
            "bump".to_string(),
            HandlerDef {
                lang: HandlerLang::Js,
                entry: "main.js".into(),
                capabilities: Capabilities {
                    http: vec!["api.example.com".into()],
                    fs: true,
                    kv: true,
                },
                limits: Limits {
                    wall_ms: 1000,
                    ..Limits::default()
                },
            },
        );
        let m = Manifest {
            handlers,
            ..Manifest::new("t", "a")
        };
        let back: Manifest = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(m, back);
        assert_eq!(back.handlers["bump"].lang, HandlerLang::Js);

        // Partial handler: limits/capabilities default (default-deny + default limits).
        let def: HandlerDef =
            serde_json::from_str(r#"{"lang":"wasm","entry":"main.wasm"}"#).unwrap();
        assert_eq!(def.lang, HandlerLang::Wasm);
        assert_eq!(def.capabilities, Capabilities::default());
        assert!(def.capabilities.http.is_empty() && !def.capabilities.fs && !def.capabilities.kv);
        assert_eq!(def.limits, Limits::default());
        assert_eq!(def.limits.wall_ms, 2000);

        // lang serializes lowercase.
        assert_eq!(serde_json::to_value(HandlerLang::Lua).unwrap(), serde_json::json!("lua"));
    }

    #[test]
    fn manifest_roundtrip_and_closed_shape() {
        let m = Manifest {
            schema_version: SURFACE_SCHEMA_VERSION.into(),
            title: "Sales".into(),
            alt_text: "alt".into(),
            created_at: Some("2026-06-27T10:00:00Z".into()),
            backflow: Some(Backflow {
                actions: vec!["refresh".into()],
            }),
            handlers: BTreeMap::new(),
            nav: vec![NavEdge {
                to: "settings".into(),
                label: Some("Settings".into()),
            }],
            metadata: json!({"k": 1}).as_object().unwrap().clone(),
        };
        let s = serde_json::to_string(&m).unwrap();
        let back: Manifest = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);

        // Unknown top-level key is rejected (closed shape).
        let bad = r#"{"schema_version":"generative-ui/surface.v1","title":"t","alt_text":"a","surprise":1}"#;
        assert!(serde_json::from_str::<Manifest>(bad).is_err());

        // Minimal manifest: only required fields.
        let min: Manifest =
            serde_json::from_str(r#"{"schema_version":"generative-ui/surface.v1","title":"t","alt_text":"a"}"#)
                .unwrap();
        assert!(min.nav.is_empty() && min.metadata.is_empty());
    }

    #[test]
    fn deck_and_event_roundtrip() {
        let d = DeckManifest {
            name: Some("Demo".into()),
            entry: Some("sales".into()),
            order: vec!["sales".into(), "settings".into()],
        };
        assert_eq!(d, serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap());
        assert!(serde_json::from_str::<DeckManifest>(r#"{"bogus":1}"#).is_err());

        let e = Event {
            schema_version: EVENT_SCHEMA_VERSION.into(),
            action: "drill down".into(),
            component: Some("ui-button".into()),
            value: json!({"region": "emea"}),
            surface: "demo/main/sales".into(),
            version: Some("1719500000000".into()),
            received_at: "2026-06-27T10:05:00Z".into(),
        };
        assert_eq!(e, serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap());
        assert_eq!(e.file_name(42), "42-drill-down.json");
    }

    #[test]
    fn esc_escapes_all() {
        assert_eq!(esc(r#"<a href="x">&'"#), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
    }

    #[test]
    fn component_render() {
        assert_eq!(
            Component::Button {
                label: "Buy <now>".into(),
                action: Some("buy".into()),
                value: json!({"id": 1}),
            }
            .render(),
            r#"<ui-button data-action="buy" data-value="{&quot;id&quot;:1}">Buy &lt;now&gt;</ui-button>"#
        );
        assert_eq!(
            Component::Heading { level: None, text: "Hi".into() }.render(),
            r#"<ui-heading level="2">Hi</ui-heading>"#
        );
        assert_eq!(
            Component::Link { to: "settings".into(), label: None }.render(),
            r#"<ui-link to="settings">settings</ui-link>"#
        );
        assert_eq!(Component::DeckNav.render(), "<ui-deck-nav></ui-deck-nav>");

        let form = Component::Form {
            action: "signup".into(),
            children: vec![Component::Input {
                name: "email".into(),
                label: Some("Email".into()),
                placeholder: None,
                input_type: Some("email".into()),
            }],
        };
        assert_eq!(
            form.render(),
            r#"<ui-form data-action="signup"><ui-input name="email" label="Email" type="email"></ui-input></ui-form>"#
        );
    }

    #[test]
    fn atomic_write_creates_dirs_and_no_temp_left() {
        let tmp = std::env::temp_dir().join(format!("gui-aw-{}", std::process::id()));
        let target = tmp.join("nested/events/1-x.json");
        atomic_write(&target, b"{}").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"{}");
        // No leftover temp files in the directory.
        let leftovers: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind");
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn component_tree_tagged_roundtrip() {
        let c = Component::Card {
            children: vec![
                Component::Heading { level: Some(1), text: "T".into() },
                Component::DeckNav,
            ],
        };
        let v = serde_json::to_value(&c).unwrap();
        assert_eq!(v["type"], "card");
        assert_eq!(v["children"][1]["type"], "deck-nav");
        let back: Component = serde_json::from_value(v).unwrap();
        assert_eq!(c, back);
    }
}

// ---------------------------------------------------------------------------
// JSON Schema generation — the Rust types above are the single source of truth.
// The files in `schemas/` are generated from them (drift-gated by the test below).
// ---------------------------------------------------------------------------

/// Base for the published `$id`s.
const SCHEMA_ID_BASE: &str = "https://plan.ai/generative-ui/schemas";

fn schema_json(id: &str, root: schemars::schema::RootSchema) -> String {
    let mut v = serde_json::to_value(&root).expect("schema serializes");
    if let Some(obj) = v.as_object_mut() {
        // Inject a stable, dereferenceable id (schemars doesn't emit one).
        obj.insert("$id".to_string(), serde_json::Value::String(format!("{SCHEMA_ID_BASE}/{id}")));
    }
    let mut out = serde_json::to_string_pretty(&v).expect("pretty json");
    out.push('\n');
    out
}

/// The published JSON Schema files, generated from the Rust types — `(filename, contents)`.
/// The on-disk `schemas/*.json` must equal these (enforced by `committed_schemas_match_types`).
pub fn json_schemas() -> Vec<(&'static str, String)> {
    vec![
        (
            "surface.v1.schema.json",
            schema_json("surface.v1.schema.json", schemars::schema_for!(Manifest)),
        ),
        (
            "deck.v1.schema.json",
            schema_json("deck.v1.schema.json", schemars::schema_for!(DeckManifest)),
        ),
        (
            "event.v1.schema.json",
            schema_json("event.v1.schema.json", schemars::schema_for!(Event)),
        ),
    ]
}

#[cfg(test)]
mod schema_files {
    use super::*;

    /// The committed `schemas/*.json` are generated from the types. Regenerate after changing
    /// a type with: `UPDATE_SCHEMAS=1 cargo test -p gui-schema committed_schemas_match_types`.
    #[test]
    fn committed_schemas_match_types() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas");
        let update = std::env::var_os("UPDATE_SCHEMAS").is_some();
        for (name, generated) in json_schemas() {
            let path = dir.join(name);
            if update {
                atomic_write(&path, generated.as_bytes()).unwrap();
            } else {
                let committed = std::fs::read_to_string(&path).unwrap_or_default();
                assert_eq!(
                    committed, generated,
                    "{name} is stale — regenerate with `UPDATE_SCHEMAS=1 cargo test -p gui-schema`"
                );
            }
        }
    }
}
