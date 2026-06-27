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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    gui_mcp::run_stdio(Workspace::new(cli.artifacts)).await
}
