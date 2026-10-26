//! `gui-mcp` binary: stdio MCP server over an artifacts directory.

use std::path::PathBuf;

use clap::Parser;
use gui_agent::Workspace;

#[derive(Parser)]
#[command(name = "gui-mcp", about = "MCP server for authoring UI surfaces and awaiting backflow.")]
struct Cli {
    /// Artifacts directory to publish surfaces into and read backflow from.
    #[arg(long, default_value = "artifacts")]
    artifacts: PathBuf,
    /// Newest surface versions to keep per surface on publish (older are GC'd; 0 = keep all).
    #[arg(long, default_value_t = gui_agent::DEFAULT_KEEP_VERSIONS)]
    keep_versions: usize,
    /// Directory for model-authored surface templates (`save_template`).
    #[arg(long, default_value = "templates")]
    templates: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // MUST log to stderr: stdout carries the MCP JSON-RPC protocol.
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,gui_mcp=debug")))
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let workspace = Workspace::new(cli.artifacts).with_keep_versions(cli.keep_versions);
    gui_mcp::run_stdio(gui_mcp::GuiServer::new(workspace).with_templates_dir(cli.templates)).await
}
