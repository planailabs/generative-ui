//! MCP server exposing the Generative UI agent API as tools.
//!
//! Regular agents speak MCP to publish surfaces, wire decks, and wait for backflow without
//! knowing the directory layout. The tool logic lives on [`GuiServer`] and delegates to
//! [`gui_agent::Workspace`]; [`run_stdio`] runs it over stdio. The tool methods are public
//! so they can be unit-tested directly without standing up the transport.

use std::time::Duration;

use base64::Engine as _;
use gui_agent::{BackflowFilter, Capabilities, HandlerLang, Limits, SurfaceBuilder, Workspace};
use gui_schema::{DeckManifest, SurfacePath, DEFAULT_DECK};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData, ServerHandler, ServiceExt,
};
use serde::Deserialize;

/// Help text describing the `<ui-*>` components an agent can compose.
pub const COMPONENT_HELP: &str = include_str!("component_help.txt");

fn invalid(msg: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(msg.into(), None)
}
fn internal(msg: impl Into<String>) -> ErrorData {
    ErrorData::internal_error(msg.into(), None)
}
fn ok_json(v: serde_json::Value) -> Result<CallToolResult, ErrorData> {
    Ok(CallToolResult::success(vec![Content::text(v.to_string())]))
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NavParam {
    /// Target surface slug in the same deck.
    pub to: String,
    pub label: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PublishParams {
    /// Agent slug (globally unique).
    pub agent: String,
    /// Deck slug; defaults to `main`.
    pub deck: Option<String>,
    /// Surface slug (the regenerable, latest-wins key).
    pub surface: String,
    pub title: String,
    /// Accessibility text describing the surface.
    pub alt_text: String,
    /// The surface body as HTML. Use the `<ui-*>` components (call `component_help`); plain
    /// HTML elements (`button`, `h1`-`h3`, `p`, `form`, `select`, `a[data-to]`) are
    /// live-rewritten into the equivalent components in the browser.
    pub html: String,
    /// Navigation edges to sibling surfaces.
    pub nav: Option<Vec<NavParam>>,
    /// Sandboxed handlers that run on matching backflow actions (server must run with
    /// `--enable-handlers`). Handlers never edit the surface; they fold the surface's KV view
    /// (read by the UI) and define the HTTP response. See `component_help` for the host API.
    pub handlers: Option<Vec<HandlerParam>>,
    /// Free-form metadata as a JSON **string** (a JSON object).
    pub metadata_json: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct HandlerParam {
    /// Backflow action this handler runs on.
    pub action: String,
    /// Engine: `js`, `lua`, or `wasm`.
    pub lang: String,
    /// Entry filename in the version dir (default `main.js`/`main.lua`/`main.wasm`).
    pub entry: Option<String>,
    /// Handler source (text — for `js`/`lua`).
    pub source: Option<String>,
    /// Handler bytes as base64 (for `wasm`, or any binary entry).
    pub source_base64: Option<String>,
    /// Allowed outbound HTTP hosts (exact match). Empty/absent ⇒ no network.
    pub http_allow: Option<Vec<String>>,
    /// Grant jailed filesystem access under the surface's `state/` dir.
    pub fs: Option<bool>,
    /// Grant access to the surface's KV view.
    pub kv: Option<bool>,
    /// Wall-clock budget in ms (default 2000).
    pub wall_ms: Option<u64>,
    /// Memory cap in MB (default 32).
    pub mem_mb: Option<u64>,
}

fn handler_lang(s: &str) -> Result<HandlerLang, ErrorData> {
    match s {
        "js" => Ok(HandlerLang::Js),
        "lua" => Ok(HandlerLang::Lua),
        "wasm" => Ok(HandlerLang::Wasm),
        other => Err(invalid(format!("unknown handler lang {other:?} (use js|lua|wasm)"))),
    }
}

fn default_entry(lang: HandlerLang) -> &'static str {
    match lang {
        HandlerLang::Js => "main.js",
        HandlerLang::Lua => "main.lua",
        HandlerLang::Wasm => "main.wasm",
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DefineDeckParams {
    pub agent: String,
    pub deck: String,
    /// Entry surface slug.
    pub entry: Option<String>,
    /// Surface slugs in navigation order.
    pub order: Option<Vec<String>>,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WaitParams {
    pub agent: String,
    pub deck: Option<String>,
    pub surface: String,
    /// Only return events with `millis` greater than this (default: wait for new ones).
    pub since: Option<u64>,
    /// How long to block before giving up (default 30000).
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ListBackflowParams {
    pub agent: String,
    pub deck: Option<String>,
    pub surface: String,
    pub since: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DrainBackflowParams {
    pub agent: String,
    pub deck: Option<String>,
    pub surface: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DeleteBackflowParams {
    pub agent: String,
    pub deck: Option<String>,
    pub surface: String,
    /// Exact event file name (the `file` id from `list_backflow`/`wait_for_backflow`).
    pub file: Option<String>,
    /// Delete events with this exact action.
    pub action: Option<String>,
    /// Only delete events with `millis > since`.
    pub since: Option<u64>,
    /// Only delete events with `millis <= until`.
    pub until: Option<u64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadDeckParams {
    pub agent: String,
    pub deck: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ReadSurfaceParams {
    pub agent: String,
    pub deck: Option<String>,
    pub surface: String,
}

/// The MCP server. Holds a [`Workspace`] over the artifacts directory.
#[derive(Clone)]
pub struct GuiServer {
    workspace: Workspace,
    // Read by the #[tool_handler] macro; rustc's dead-code pass doesn't see that.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

fn surface_path(agent: &str, deck: &Option<String>, surface: &str) -> Result<SurfacePath, ErrorData> {
    let deck = deck.as_deref().unwrap_or(DEFAULT_DECK);
    SurfacePath::new(agent, deck, surface).map_err(|e| invalid(e.to_string()))
}

#[tool_router]
impl GuiServer {
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        description = "Publish a new version of a UI surface (latest-wins). Provide `html`: the surface body written with the <ui-*> components (call `component_help` first). You may also write plain HTML (button, h1-h3, p, form, select, a[data-to]); it is live-rewritten into the equivalent components in the browser. Optionally attach `handlers` (js/lua/wasm) that run on backflow actions to fold the surface KV view and define a response — bind values in the HTML with data-kv=\"key\"."
    )]
    pub async fn publish_surface(
        &self,
        Parameters(p): Parameters<PublishParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = surface_path(&p.agent, &p.deck, &p.surface)?;
        tracing::info!(surface = %path.id(), "tool: publish_surface");
        let mut b = SurfaceBuilder::new(p.title, p.alt_text).html(p.html);
        for edge in p.nav.unwrap_or_default() {
            b = b.nav(edge.to, edge.label.as_deref());
        }
        for h in p.handlers.unwrap_or_default() {
            let lang = handler_lang(&h.lang)?;
            let entry = h.entry.unwrap_or_else(|| default_entry(lang).to_string());
            let source: Vec<u8> = match (h.source, h.source_base64) {
                (Some(s), _) => s.into_bytes(),
                (None, Some(b64)) => base64::engine::general_purpose::STANDARD
                    .decode(b64.trim())
                    .map_err(|e| invalid(format!("handler source_base64: {e}")))?,
                (None, None) => return Err(invalid("handler needs `source` or `source_base64`")),
            };
            let capabilities = Capabilities {
                http: h.http_allow.unwrap_or_default(),
                fs: h.fs.unwrap_or(false),
                kv: h.kv.unwrap_or(false),
            };
            let mut limits = Limits::default();
            if let Some(w) = h.wall_ms {
                limits.wall_ms = w;
            }
            if let Some(m) = h.mem_mb {
                limits.mem_mb = m;
            }
            b = b.handler(h.action, lang, entry, source, capabilities, limits);
        }
        if let Some(metadata) = p.metadata_json {
            let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&metadata)
                .map_err(|e| invalid(format!("invalid metadata_json: {e}")))?;
            for (k, v) in obj {
                b = b.meta(k, v);
            }
        }
        let version = self
            .workspace
            .publish(&path, &b)
            .map_err(|e| internal(e.to_string()))?;
        ok_json(serde_json::json!({
            "ok": true,
            "surface": path.id(),
            "version": version.to_string(),
            "url": path.url(),
        }))
    }

    #[tool(description = "Create or update a deck's wiring (entry surface + navigation order).")]
    pub async fn define_deck(
        &self,
        Parameters(p): Parameters<DefineDeckParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let manifest = DeckManifest {
            name: p.name,
            entry: p.entry,
            order: p.order.unwrap_or_default(),
        };
        self.workspace
            .define_deck(&p.agent, &p.deck, &manifest)
            .map_err(|e| invalid(e.to_string()))?;
        ok_json(serde_json::json!({ "ok": true, "deck": format!("{}/{}", p.agent, p.deck) }))
    }

    #[tool(
        description = "Block until a user interaction (backflow) arrives for a surface, then return it. Use this after publish_surface to react to clicks/submissions."
    )]
    pub async fn wait_for_backflow(
        &self,
        Parameters(p): Parameters<WaitParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = surface_path(&p.agent, &p.deck, &p.surface)?;
        tracing::info!(surface = %path.id(), "tool: wait_for_backflow");
        let ws = self.workspace.clone();
        let since = p.since;
        let timeout_ms = p.timeout_ms.unwrap_or(30_000);
        let timeout = Duration::from_millis(timeout_ms);
        // The wait is blocking (filesystem watch); keep it off the async executor.
        let result = tokio::task::spawn_blocking(move || ws.wait_for_backflow(&path, since, timeout))
            .await
            .map_err(|e| internal(e.to_string()))?
            .map_err(|e| internal(e.to_string()))?;
        match result {
            Some(be) => ok_json(serde_json::json!({
                "received": true, "file": be.file, "event": be.event
            })),
            None => ok_json(serde_json::json!({ "received": false, "timeout_ms": timeout_ms })),
        }
    }

    #[tool(description = "List recent backflow events for a surface (optionally only those after `since`).")]
    pub async fn list_backflow(
        &self,
        Parameters(p): Parameters<ListBackflowParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = surface_path(&p.agent, &p.deck, &p.surface)?;
        let events = self
            .workspace
            .read_backflow(&path, p.since)
            .map_err(|e| internal(e.to_string()))?;
        let events: Vec<_> = events
            .into_iter()
            .map(|be| serde_json::json!({ "file": be.file, "millis": be.millis, "event": be.event }))
            .collect();
        ok_json(serde_json::json!({ "surface": path.id(), "events": events }))
    }

    #[tool(
        description = "Drain a surface's backflow: delete ALL recorded events. Returns the deleted file names."
    )]
    pub async fn drain_backflow(
        &self,
        Parameters(p): Parameters<DrainBackflowParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = surface_path(&p.agent, &p.deck, &p.surface)?;
        tracing::info!(surface = %path.id(), "tool: drain_backflow");
        let deleted = self
            .workspace
            .delete_backflow(&path, &BackflowFilter::default())
            .map_err(|e| internal(e.to_string()))?;
        ok_json(serde_json::json!({ "surface": path.id(), "deleted": deleted }))
    }

    #[tool(
        description = "Delete specific backflow events for a surface, selected by `file` (the id from list_backflow/wait_for_backflow), exact `action`, and/or a `since`/`until` millis range. All given filters must match; at least one is required (use drain_backflow to delete everything). Returns the deleted file names."
    )]
    pub async fn delete_backflow(
        &self,
        Parameters(p): Parameters<DeleteBackflowParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = surface_path(&p.agent, &p.deck, &p.surface)?;
        let filter = BackflowFilter {
            file: p.file,
            action: p.action,
            since: p.since,
            until: p.until,
        };
        if filter.is_empty() {
            return Err(invalid(
                "delete_backflow needs at least one filter (file, action, since, until); use drain_backflow to delete all events",
            ));
        }
        tracing::info!(surface = %path.id(), ?filter, "tool: delete_backflow");
        let deleted = self
            .workspace
            .delete_backflow(&path, &filter)
            .map_err(|e| internal(e.to_string()))?;
        ok_json(serde_json::json!({ "surface": path.id(), "deleted": deleted }))
    }

    #[tool(description = "List all discovered agents, decks, and surfaces (live versions).")]
    pub async fn list_surfaces(&self) -> Result<CallToolResult, ErrorData> {
        let idx = gui_server::Index::build(self.workspace.root());
        let resp = gui_server::api::index_response(&idx);
        let v = serde_json::to_value(&resp).map_err(|e| internal(e.to_string()))?;
        ok_json(v)
    }

    #[tool(description = "Read a deck's navigation graph (entry, order, surfaces, nav edges).")]
    pub async fn read_deck(
        &self,
        Parameters(p): Parameters<ReadDeckParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let idx = gui_server::Index::build(self.workspace.root());
        match idx.deck(&p.agent, &p.deck) {
            Some(d) => {
                let v = serde_json::to_value(gui_server::api::deck_graph(d))
                    .map_err(|e| internal(e.to_string()))?;
                ok_json(v)
            }
            None => Err(invalid(format!("no deck {}/{}", p.agent, p.deck))),
        }
    }

    #[tool(description = "Read a surface's live manifest and rendered HTML.")]
    pub async fn read_surface(
        &self,
        Parameters(p): Parameters<ReadSurfaceParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let path = surface_path(&p.agent, &p.deck, &p.surface)?;
        let Some(version) = self.workspace.live_version(&path) else {
            return Err(invalid(format!("no surface {}", path.id())));
        };
        let (manifest, html) = self
            .workspace
            .read_version(&path, version)
            .map_err(|e| internal(e.to_string()))?;
        ok_json(serde_json::json!({
            "surface": path.id(),
            "version": version.to_string(),
            "manifest": manifest,
            "html": html,
        }))
    }

    #[tool(description = "Reference for the <ui-*> components available when composing a surface.")]
    pub async fn component_help(&self) -> Result<CallToolResult, ErrorData> {
        Ok(CallToolResult::success(vec![Content::text(COMPONENT_HELP)]))
    }
}

#[tool_handler]
impl ServerHandler for GuiServer {
    fn get_info(&self) -> ServerInfo {
        // ServerInfo is #[non_exhaustive]; build from default and set fields.
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Author interactive UI surfaces for users and react to their input. \
             publish_surface creates/regenerates a surface (latest-wins); define_deck \
             wires navigation; wait_for_backflow blocks until the user interacts. \
             Call component_help for the component reference."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        // Stable server name so clients see predictable tool names (e.g. `gui:publish_surface`).
        let mut imp = rmcp::model::Implementation::default();
        imp.name = "gui".into();
        imp.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = imp;
        info
    }
}

/// Run the MCP server over stdio until the client disconnects.
pub async fn run_stdio(workspace: Workspace) -> anyhow::Result<()> {
    let service = GuiServer::new(workspace).serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

/// Serve the MCP server over an arbitrary byte stream (e.g. an in-process duplex), so a host
/// can embed the server without spawning a subprocess.
pub async fn serve_io<R, W>(workspace: Workspace, read: R, write: W) -> anyhow::Result<()>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    tracing::debug!("gui-mcp serving over in-process stream");
    let service = GuiServer::new(workspace).serve((read, write)).await?;
    service.waiting().await?;
    Ok(())
}
