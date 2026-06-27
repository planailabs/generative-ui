//! Clean-named MCP proxy tools.
//!
//! swiftide's built-in `McpToolbox` registers tools as `server:tool` (e.g.
//! `gui:publish_surface`). OpenAI's function-name regex forbids the `:`, so strict
//! OpenAI-compatible endpoints reject the request. These proxies expose each MCP tool under
//! its plain name (`publish_surface`) while still invoking it over the rmcp client — so the
//! agent genuinely drives the gui-mcp server.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use rmcp::{
    model::{CallToolRequestParams, ClientInfo, Implementation, InitializeRequestParams, RawContent, RawTextContent},
    service::RunningService,
    RoleClient, ServiceExt as _,
};
use schemars::Schema;
use swiftide::chat_completion::{errors::ToolError, ToolCall, ToolOutput, ToolSpec};
use swiftide::traits::{AgentContext, Tool};
use tokio::sync::RwLock;

type Client = Arc<RwLock<Option<RunningService<RoleClient, InitializeRequestParams>>>>;

/// A swiftide tool that forwards to one MCP tool, using its plain (colon-free) name.
#[derive(Clone)]
struct McpProxyTool {
    client: Client,
    server_tool_name: String,
    spec: ToolSpec,
}

#[async_trait]
impl Tool for McpProxyTool {
    async fn invoke(&self, _ctx: &dyn AgentContext, tool_call: &ToolCall) -> Result<ToolOutput, ToolError> {
        let arguments = match tool_call.args() {
            Some(a) => Some(serde_json::from_str(a).map_err(ToolError::WrongArguments)?),
            None => None,
        };
        let request = CallToolRequestParams {
            meta: None,
            name: self.server_tool_name.clone().into(),
            arguments,
            task: None,
        };
        let guard = self.client.read().await;
        let service = guard
            .as_ref()
            .ok_or_else(|| ToolError::Unknown(anyhow::anyhow!("mcp client not running")))?;
        let response = service
            .call_tool(request)
            .await
            .map_err(|e| ToolError::Unknown(anyhow::anyhow!("mcp call failed: {e}")))?;
        let text = response
            .content
            .into_iter()
            .filter_map(|c| match c.raw {
                RawContent::Text(RawTextContent { text, .. }) => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if response.is_error.unwrap_or(false) {
            return Err(ToolError::Unknown(anyhow::anyhow!("mcp tool error: {text}")));
        }
        Ok(if text.is_empty() { "ok".into() } else { text.into() })
    }

    fn name(&self) -> Cow<'_, str> {
        self.server_tool_name.as_str().into()
    }

    fn tool_spec(&self) -> ToolSpec {
        self.spec.clone()
    }
}

/// Connect an rmcp client over the given stream to the gui-mcp server and build a clean-named
/// proxy tool for each MCP tool.
pub async fn connect<R, W>(read: R, write: W) -> Result<Vec<Box<dyn Tool>>>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
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
    let service = tokio::time::timeout(Duration::from_secs(15), client_info.serve((read, write)))
        .await
        .map_err(|_| anyhow::anyhow!("MCP handshake timed out (client/server rmcp mismatch?)"))??;
    tracing::info!(
        server = ?service.peer_info().map(|i| i.server_info.name.clone()),
        "MCP client connected"
    );

    let listed = service.list_all_tools().await.context("listing MCP tools")?;
    let client: Client = Arc::new(RwLock::new(None));

    let mut tools: Vec<Box<dyn Tool>> = Vec::new();
    for tool in listed {
        let mut builder = ToolSpec::builder();
        builder.name(tool.name.to_string());
        if let Some(description) = &tool.description {
            builder.description(description.to_string());
        }
        let schema_value = tool.schema_as_json_value();
        if !schema_value.is_null() {
            let schema: Schema =
                serde_json::from_value(schema_value).context("parsing MCP tool schema")?;
            builder.parameters_schema(schema);
        }
        let spec = builder.build().context("building tool spec")?;
        tracing::debug!(tool = %tool.name, "registered MCP proxy tool");
        tools.push(Box::new(McpProxyTool {
            client: Arc::clone(&client),
            server_tool_name: tool.name.to_string(),
            spec,
        }));
    }

    // Hand the live service to the proxies.
    *client.write().await = Some(service);
    tracing::info!(count = tools.len(), "connected MCP tools (clean names)");
    Ok(tools)
}
