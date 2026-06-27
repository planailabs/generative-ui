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
    /// Execute surface handlers (main.js/lua/wasm) on matching backflow actions. Off by
    /// default — handlers run sandboxed code, so opt in explicitly.
    #[arg(long)]
    enable_handlers: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();
    let state = AppState::new(cli.artifacts.clone()).with_handlers(cli.enable_handlers);
    if cli.enable_handlers {
        tracing::warn!("handler execution enabled — surfaces' main.js/lua/wasm will run on backflow");
    }

    let idx = state.index.load_full();
    for w in &idx.warnings {
        tracing::warn!("{w}");
    }
    tracing::info!(
        agents = idx.agents.len(),
        surfaces = idx.surface_count(),
        artifacts = %cli.artifacts.display(),
        "discovered artifacts"
    );

    gui_server::serve(&cli.addr, state).await
}

/// Logs to stderr; level via `RUST_LOG` (default `info`, with `gui_server` at debug).
fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,gui_server=debug,tower_http=debug")),
        )
        .with_writer(std::io::stderr)
        .init();
}
