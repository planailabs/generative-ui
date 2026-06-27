//! Direct-access demo: the HTTP server, the MCP server, and a swiftide LLM agent all run in
//! one process as separate libraries.
//!
//! "Direct access" = everything (server + MCP + agent) lives in one binary, wired in-process
//! — no subprocesses. The swiftide agent connects to our `gui-mcp` server over an in-memory
//! pipe and **authors the whole app itself** through the MCP tools (`gui:publish_surface`,
//! `gui:wait_for_backflow`, …), guided by the `authoring-ui` skill loaded as its system
//! prompt. The HTTP server renders whatever the agent publishes.

use std::path::PathBuf;

use anyhow::Result;
use async_trait::async_trait;
use rmcp::{
    model::{ClientInfo, Implementation},
    ServiceExt as _,
};
use swiftide::agents::{self, tools::mcp::McpToolbox};
use swiftide::chat_completion::{
    errors::LanguageModelError, ChatCompletion, ChatCompletionRequest, ChatCompletionResponse,
    ChatMessage, ToolCall,
};

/// The `authoring-ui` skill — loaded into the agent so it knows how to use the MCP tools.
const SKILL: &str = include_str!("../../../.agents/skills/authoring-ui/SKILL.md");

/// Connect the agent to an in-process `gui-mcp` server and let it author the app via MCP.
///
/// Serves `gui-mcp` (rmcp 1.8) on one end of an in-memory duplex and connects swiftide's
/// rmcp-0.17 client on the other — JSON-RPC is wire-compatible, so no subprocess is needed.
pub async fn run_agent<L>(llm: L, artifacts: PathBuf, goal: &str) -> Result<()>
where
    L: ChatCompletion + Clone + 'static,
{
    tracing::info!(artifacts = %artifacts.display(), goal, "starting agent session");
    let (server_end, client_end) = tokio::io::duplex(1 << 16);

    // gui-mcp server, in-process.
    let (sr, sw) = tokio::io::split(server_end);
    let ws = gui_agent::Workspace::new(&artifacts);
    tracing::debug!("spawning in-process gui-mcp server");
    tokio::spawn(async move {
        if let Err(e) = gui_mcp::serve_io(ws, sr, sw).await {
            tracing::error!(error = %e, "gui-mcp server stopped");
        }
    });

    // swiftide's rmcp client on the other end → an MCP toolbox.
    let (cr, cw) = tokio::io::split(client_end);
    let client_info = ClientInfo {
        client_info: Implementation {
            name: "direct-access".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            title: None,
            description: None,
            icons: None,
            website_url: None,
        },
        ..Default::default()
    };
    tracing::info!("connecting MCP client to gui-mcp over in-process duplex");
    let service = tokio::time::timeout(std::time::Duration::from_secs(15), client_info.serve((cr, cw)))
        .await
        .map_err(|_| anyhow::anyhow!("MCP client handshake timed out (server/client rmcp protocol mismatch?)"))??;
    tracing::info!(server = ?service.peer_info().map(|i| i.server_info.name.clone()), "MCP client connected");
    let toolbox = McpToolbox::from_running_service(service);

    let system = format!(
        "{SKILL}\n\n# Your task right now\n{goal}\n\nYou author the app yourself using the \
         gui:* tools. If unsure about components, call gui:component_help first. Build each \
         screen with gui:publish_surface (compose a component tree); wire navigation with \
         gui:define_deck. When the app is complete, stop and briefly summarize what you built."
    );

    let mut agent = agents::Agent::builder()
        .llm(&llm)
        .add_toolbox(toolbox)
        .system_prompt(system)
        .on_new_message(|_, msg| {
            let msg = msg.to_string();
            Box::pin(async move {
                tracing::info!(target: "agent", "{msg}");
                Ok(())
            })
        })
        .limit(20)
        .build()?;

    tracing::info!("agent built; querying");
    agent.query(goal.to_string()).await?;
    tracing::info!("agent run complete");
    Ok(())
}

/// A deterministic LLM stand-in: it drives one `gui:publish_surface` tool call (building a
/// small home screen) and then stops. Lets the full agent→MCP→app flow run offline and in
/// tests without a network or API key.
#[derive(Clone, Default)]
pub struct MockLlm;

#[async_trait]
impl ChatCompletion for MockLlm {
    async fn complete(
        &self,
        request: &ChatCompletionRequest<'_>,
    ) -> std::result::Result<ChatCompletionResponse, LanguageModelError> {
        // If a tool has already run, finish.
        let acted = request
            .messages()
            .iter()
            .any(|m| matches!(m, ChatMessage::ToolOutput(_, _)));
        if acted {
            return Ok(ChatCompletionResponse::builder()
                .message("Done — published the home screen.".to_string())
                .build()
                .expect("response builds"));
        }
        // Otherwise, author a small app by calling the MCP publish_surface tool.
        let args = serde_json::json!({
            "agent": "app",
            "surface": "home",
            "title": "Welcome",
            "alt_text": "Generated app home screen",
            "components": [
                { "type": "heading", "text": "Welcome" },
                { "type": "text", "text": "This screen was generated by the agent through the MCP." },
                { "type": "button", "label": "Get started", "action": "start" }
            ]
        })
        .to_string();
        let call = ToolCall::builder()
            .id("call-1")
            .name("gui:publish_surface")
            .args(args)
            .build()
            .expect("tool call builds");
        Ok(ChatCompletionResponse::builder()
            .tool_calls(vec![call])
            .build()
            .expect("response builds"))
    }
}
