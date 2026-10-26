//! `gui` — debug/ops CLI for the Generative UI Suite.
//!
//! Inspect the artifacts directory, validate manifests, publish surfaces, and tail/emit
//! backflow — all without a browser. Reuses `gui-schema`, `gui-agent`, and `gui-server`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use gui_agent::{SurfaceBuilder, Workspace};
use gui_schema::{atomic_write, Component, DeckManifest, Event, SurfacePath, Version, EVENT_SCHEMA_VERSION};
use gui_server::Index;

mod kv_trigger;

#[derive(Parser)]
#[command(name = "gui", about = "Debug/ops CLI for the Generative UI Suite.")]
struct Cli {
    /// Artifacts directory to operate on.
    #[arg(long, global = true, default_value = "artifacts")]
    artifacts: PathBuf,
    /// Newest surface versions to keep per surface on publish (older are GC'd; 0 = keep all).
    #[arg(long, global = true, default_value_t = gui_agent::DEFAULT_KEEP_VERSIONS)]
    keep_versions: usize,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Print the discovered tree (agents → decks → surfaces → live version + history).
    Index,
    /// Validate every frame.json/deck.json/agent.json; non-zero exit on any error.
    Validate,
    /// Print a deck's navigation graph as JSON.
    Deck {
        agent: String,
        #[arg(default_value = "main")]
        deck: String,
    },
    /// Print a surface's live (or specific) manifest, history, and optionally its HTML.
    Surface {
        agent: String,
        surface: String,
        #[arg(long, default_value = "main")]
        deck: String,
        #[arg(long)]
        version: Option<u64>,
        #[arg(long)]
        html: bool,
    },
    /// Publish a surface version from raw HTML or a component-tree JSON file.
    Publish {
        agent: String,
        surface: String,
        #[arg(long, default_value = "main")]
        deck: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        alt: String,
        #[arg(long, conflicts_with = "components")]
        html: Option<PathBuf>,
        #[arg(long)]
        components: Option<PathBuf>,
    },
    /// Read or emit backflow events.
    Backflow {
        #[command(subcommand)]
        cmd: BackflowCmd,
    },
    /// Watch the artifacts tree and log changes as they happen.
    Watch,
    /// Print a reagent trigger event (one JSON line) per changed key in surfaces' public KV
    /// views. Run as a reagent `watch` trigger, or `poll` with `--once`.
    KvTrigger {
        /// Only this surface (`agent/deck/surface`); repeatable. Default: all.
        #[arg(long = "surface")]
        surfaces: Vec<String>,
        /// Only KV keys with this prefix (`stream.progress`, `saved`); repeatable.
        #[arg(long = "key")]
        keys: Vec<String>,
        /// reagent `to`: `new` (start a task), `running` (message the running one) or a task id.
        #[arg(long, default_value = "new")]
        to: String,
        /// Snapshot file. Default: `$REAGENT_STATE/kv-trigger.json` when set, else in memory.
        #[arg(long)]
        state: Option<PathBuf>,
        /// Diff once and exit (poll mode).
        #[arg(long)]
        once: bool,
        /// Also report `stream.*` keys (off: they're mostly the agent's own progress).
        #[arg(long)]
        with_stream: bool,
    },
}

#[derive(Subcommand)]
enum BackflowCmd {
    /// Block and print each backflow event for a surface as it arrives.
    Tail {
        agent: String,
        surface: String,
        #[arg(long, default_value = "main")]
        deck: String,
    },
    /// Write a synthetic backflow event (exercise the loop without a browser).
    Emit {
        agent: String,
        surface: String,
        #[arg(long, default_value = "main")]
        deck: String,
        #[arg(long)]
        action: String,
        #[arg(long)]
        value: Option<String>,
        #[arg(long)]
        component: Option<String>,
    },
}

fn now() -> (u64, String) {
    use time::{format_description::well_known::Rfc3339, OffsetDateTime};
    let n = OffsetDateTime::now_utc();
    (
        (n.unix_timestamp_nanos() / 1_000_000) as u64,
        n.format(&Rfc3339).unwrap_or_default(),
    )
}

fn main() -> Result<()> {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn,gui_cli=info")))
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let root = cli.artifacts;
    match cli.cmd {
        Cmd::Index => cmd_index(&root),
        Cmd::Validate => cmd_validate(&root),
        Cmd::Deck { agent, deck } => cmd_deck(&root, &agent, &deck),
        Cmd::Surface { agent, surface, deck, version, html } => {
            cmd_surface(&root, &agent, &deck, &surface, version, html)
        }
        Cmd::Publish { agent, surface, deck, title, alt, html, components } => {
            cmd_publish(&root, cli.keep_versions, &agent, &deck, &surface, &title, &alt, html, components)
        }
        Cmd::Backflow { cmd } => match cmd {
            BackflowCmd::Tail { agent, surface, deck } => cmd_tail(&root, &agent, &deck, &surface),
            BackflowCmd::Emit { agent, surface, deck, action, value, component } => {
                cmd_emit(&root, &agent, &deck, &surface, &action, value, component)
            }
        },
        Cmd::Watch => cmd_watch(&root),
        Cmd::KvTrigger { surfaces, keys, to, state, once, with_stream } => {
            let state = state.or_else(|| {
                std::env::var_os("REAGENT_STATE").map(|d| PathBuf::from(d).join("kv-trigger.json"))
            });
            kv_trigger::run(&root, kv_trigger::Opts { surfaces, keys, to, state, once, with_stream })
        }
    }
}

fn cmd_index(root: &std::path::Path) -> Result<()> {
    let idx = Index::build(root);
    for w in &idx.warnings {
        eprintln!("warning: {w}");
    }
    println!(
        "{} agent(s), {} surface(s) under {}",
        idx.agents.len(),
        idx.surface_count(),
        root.display()
    );
    for agent in idx.agents.values() {
        println!("{}", agent.slug);
        for deck in agent.decks.values() {
            println!("  {}/", deck.slug);
            for surface in deck.ordered_surfaces() {
                println!(
                    "    {}  v{} ({} version(s))",
                    surface.path.surface,
                    surface.live().version,
                    surface.versions.len()
                );
            }
        }
    }
    Ok(())
}

/// Recursively collect every manifest file under `root`.
fn manifest_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(
                path.file_name().and_then(|s| s.to_str()),
                Some("frame.json") | Some("deck.json") | Some("agent.json")
            ) {
                out.push(path);
            }
        }
    }
    out
}

fn cmd_validate(root: &std::path::Path) -> Result<()> {
    let mut errors = 0usize;
    let mut checked = 0usize;
    for path in manifest_files(root) {
        checked += 1;
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                println!("ERROR {}: {e}", path.display());
                errors += 1;
                continue;
            }
        };
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let result = match name {
            "frame.json" => serde_json::from_slice::<gui_schema::Manifest>(&bytes).map(|_| ()),
            "deck.json" => serde_json::from_slice::<DeckManifest>(&bytes).map(|_| ()),
            "agent.json" => serde_json::from_slice::<gui_schema::AgentInfo>(&bytes).map(|_| ()),
            _ => Ok(()),
        };
        if let Err(e) = result {
            println!("ERROR {}: {e}", path.display());
            errors += 1;
        }
    }
    println!("{checked} manifest(s) checked, {errors} error(s)");
    if errors > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn cmd_deck(root: &std::path::Path, agent: &str, deck: &str) -> Result<()> {
    let idx = Index::build(root);
    let d = idx
        .deck(agent, deck)
        .with_context(|| format!("no deck {agent}/{deck}"))?;
    let graph = gui_server::api::deck_graph(d);
    println!("{}", serde_json::to_string_pretty(&graph)?);
    Ok(())
}

fn cmd_surface(
    root: &std::path::Path,
    agent: &str,
    deck: &str,
    surface: &str,
    version: Option<u64>,
    show_html: bool,
) -> Result<()> {
    let ws = Workspace::new(root);
    let path = SurfacePath::new(agent, deck, surface).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let v = match version {
        Some(v) => Version(v),
        None => ws.live_version(&path).context("surface not found")?,
    };
    let (manifest, html) = ws.read_version(&path, v)?;
    println!("surface {}  version {v}", path.id());
    if let Some(live) = ws.live_version(&path) {
        println!("live version: {live}");
    }
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    if show_html {
        println!("--- index.html ---\n{html}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_publish(
    root: &std::path::Path,
    keep_versions: usize,
    agent: &str,
    deck: &str,
    surface: &str,
    title: &str,
    alt: &str,
    html: Option<PathBuf>,
    components: Option<PathBuf>,
) -> Result<()> {
    let ws = Workspace::new(root).with_keep_versions(keep_versions);
    let path = SurfacePath::new(agent, deck, surface).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut b = SurfaceBuilder::new(title, alt);
    match (html, components) {
        (Some(file), _) => {
            b = b.html(std::fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?);
        }
        (None, Some(file)) => {
            let raw = std::fs::read(&file).with_context(|| format!("read {}", file.display()))?;
            let comps: Vec<Component> =
                serde_json::from_slice(&raw).context("parse component-tree JSON")?;
            b = b.components(comps);
        }
        (None, None) => bail!("provide --html <FILE> or --components <FILE>"),
    }
    let version = ws.publish(&path, &b)?;
    println!("published {} version {version}  ->  {}", path.id(), path.url());
    Ok(())
}

fn cmd_emit(
    root: &std::path::Path,
    agent: &str,
    deck: &str,
    surface: &str,
    action: &str,
    value: Option<String>,
    component: Option<String>,
) -> Result<()> {
    let path = SurfacePath::new(agent, deck, surface).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let value = match value {
        Some(v) => serde_json::from_str(&v).context("--value must be JSON")?,
        None => serde_json::Value::Null,
    };
    let (ms, iso) = now();
    let event = Event {
        schema_version: EVENT_SCHEMA_VERSION.to_string(),
        action: action.to_string(),
        component,
        value,
        surface: path.id(),
        version: None,
        scope: gui_schema::Scope::Public,
        source: Some("cli".to_string()),
        received_at: iso,
    };
    let target = path.events_dir(root).join(event.file_name(ms));
    atomic_write(&target, &serde_json::to_vec_pretty(&event)?)?;
    println!("wrote {}", target.display());
    Ok(())
}

fn cmd_tail(root: &std::path::Path, agent: &str, deck: &str, surface: &str) -> Result<()> {
    let ws = Workspace::new(root);
    let path = SurfacePath::new(agent, deck, surface).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    eprintln!("tailing backflow for {} (Ctrl-C to stop)…", path.id());
    let mut since = now().0;
    loop {
        if let Some(be) = ws.wait_for_backflow(&path, Some(since), Duration::from_secs(3600))? {
            println!("{}", serde_json::to_string(&be.event)?);
            since = be.millis;
        }
    }
}

fn cmd_watch(root: &std::path::Path) -> Result<()> {
    use notify::{RecursiveMode, Watcher};
    std::fs::create_dir_all(root).ok();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    eprintln!("watching {} (Ctrl-C to stop)…", root.display());
    for res in rx {
        match res {
            Ok(event) => println!("{:?} {:?}", event.kind, event.paths),
            Err(e) => eprintln!("watch error: {e}"),
        }
    }
    Ok(())
}
