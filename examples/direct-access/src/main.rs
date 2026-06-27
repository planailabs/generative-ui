//! Direct-access binary: serve the UI and run a swiftide agent that talks to the user
//! through it — all in one process.
//!
//!   direct-access --artifacts ./run --mock         # offline, deterministic
//!   OPENAI_API_KEY=… direct-access --artifacts ./run --model gpt-4o-mini
//!
//! Then open the printed URL and chat. Each prompt you submit flows back to the agent; its
//! answer re-renders the surface live.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use gui_agent::Workspace;
use gui_server::AppState;

#[derive(Parser)]
#[command(name = "direct-access", about = "Server + swiftide agent in one binary.")]
struct Cli {
    #[arg(long, default_value = "run-artifacts")]
    artifacts: PathBuf,
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,
    /// Use the deterministic mock LLM instead of a real backend (no API key needed).
    #[arg(long)]
    mock: bool,
    /// OpenAI prompt model (when not using --mock).
    #[arg(long, default_value = "gpt-4o-mini")]
    model: String,
    /// Custom OpenAI-compatible base URL (e.g. a local server, Azure, or OpenRouter).
    /// Defaults to the official API. The key is read from OPENAI_API_KEY.
    #[arg(long)]
    base_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.artifacts).ok();

    // Serve the UI in the background.
    let state = AppState::new(cli.artifacts.clone());
    let addr = cli.addr.clone();
    tokio::spawn(async move {
        if let Err(e) = gui_server::serve(&addr, state).await {
            eprintln!("server error: {e}");
        }
    });
    eprintln!(
        "open http://{}/{}/{}/  (chat with the agent)",
        cli.addr,
        direct_access::AGENT,
        direct_access::SURFACE
    );

    // Run the agent session in the foreground.
    let ws = Workspace::new(&cli.artifacts);
    if cli.mock {
        return direct_access::run_session(ws, direct_access::MockLlm).await;
    }

    #[cfg(feature = "openai")]
    {
        let llm = build_openai(&cli.model, cli.base_url.as_deref())?;
        direct_access::run_session(ws, llm).await
    }
    #[cfg(not(feature = "openai"))]
    {
        let _ = (&cli.model, &cli.base_url);
        anyhow::bail!(
            "no real LLM backend compiled in — run with --mock, or rebuild with `--features openai`"
        )
    }
}

/// Build an OpenAI (or OpenAI-compatible) LLM. A custom `base_url` points the client at any
/// compatible endpoint; the API key comes from `OPENAI_API_KEY`.
#[cfg(feature = "openai")]
fn build_openai(
    model: &str,
    base_url: Option<&str>,
) -> Result<swiftide::integrations::openai::OpenAI> {
    use swiftide::integrations::openai::OpenAI;
    let mut builder = OpenAI::builder();
    builder.default_prompt_model(model);
    if let Some(url) = base_url {
        let config = async_openai::config::OpenAIConfig::new().with_api_base(url);
        builder.client(async_openai::Client::with_config(config));
    }
    Ok(builder.build()?)
}
