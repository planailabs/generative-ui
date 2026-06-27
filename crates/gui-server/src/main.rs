//! `gui-server` binary: scan an artifacts directory and serve its surfaces over HTTP.

use std::path::PathBuf;

use clap::Parser;
use gui_server::AppState;

#[derive(Parser)]
#[command(name = "gui-server", about = "Serve agent-published UI surfaces from a directory.")]
struct Cli {
    /// Directory of artifacts to discover and serve.
    #[arg(long, default_value = "artifacts")]
    artifacts: PathBuf,
    /// Address to bind.
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let state = AppState::new(cli.artifacts.clone());

    let idx = state.index.load_full();
    for w in &idx.warnings {
        eprintln!("warning: {w}");
    }
    eprintln!(
        "discovered {} agent(s), {} surface(s) under {}",
        idx.agents.len(),
        idx.surface_count(),
        cli.artifacts.display()
    );

    gui_server::serve(&cli.addr, state).await
}
