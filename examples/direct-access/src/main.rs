//! Direct-access binary: serve the UI and run a swiftide agent that authors the whole app
//! through the in-process MCP server — all in one process.
//!
//!   direct-access --artifacts ./run --mock              # offline, deterministic
//!   OPENAI_API_KEY=… direct-access --artifacts ./run \
//!       --model gpt-5.4-mini --base-url https://your-endpoint/v1 \
//!       --goal "Build a signup form, then a thank-you screen"
//!
//! Open the printed inspector URL to watch the surfaces the agent publishes.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use gui_server::AppState;

#[derive(Parser)]
#[command(name = "direct-access", about = "Server + in-process MCP + a swiftide agent that builds the app.")]
struct Cli {
    #[arg(long, default_value = "run-artifacts")]
    artifacts: PathBuf,
    #[arg(long, default_value = "127.0.0.1:8080")]
    addr: String,
    /// Use the deterministic mock LLM (no API key) — publishes a canned home screen via MCP.
    #[arg(long)]
    mock: bool,
    /// OpenAI prompt model (when not using --mock).
    #[arg(long, default_value = "gpt-5.4-mini")]
    model: String,
    /// Custom OpenAI-compatible base URL (key from OPENAI_API_KEY).
    #[arg(long)]
    base_url: Option<String>,
    /// What the agent should build. If omitted, the agent first asks the user via a chat
    /// surface (open it in the browser) and builds whatever they request.
    #[arg(long)]
    goal: Option<String>,
}

/// Instruction used when no `--goal` is given: ask the user through the UI, then build.
const INTERACTIVE_GOAL: &str = "No goal was given yet. First call publish_surface for agent \
\"assistant\", deck \"main\", surface \"chat\": a heading asking \"What app should I build?\" \
and a form (action \"submit\") containing a text input named \"goal\". Then call \
wait_for_backflow on assistant/main/chat to receive the user's answer (the \"goal\" field of \
the event value). Then build exactly what they asked for as one or more surfaces, and stop.";

#[tokio::main]
async fn main() -> Result<()> {
    // Verbose by default; override with RUST_LOG. Logs go to stderr.
    use tracing_subscriber::EnvFilter;
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            EnvFilter::new("info,direct_access=debug,gui_server=debug,gui_agent=debug,gui_mcp=debug,swiftide=debug,rmcp=info")
        }))
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.artifacts).ok();

    // Serve the UI so the agent's published surfaces are viewable live.
    let state = AppState::new(cli.artifacts.clone());
    let addr = cli.addr.clone();
    tokio::spawn(async move {
        if let Err(e) = gui_server::serve(&addr, state).await {
            eprintln!("server error: {e}");
        }
    });
    let goal = cli.goal.clone().unwrap_or_else(|| INTERACTIVE_GOAL.to_string());
    if cli.goal.is_none() {
        eprintln!("no --goal given: the agent will ask via http://{}/assistant/chat/", cli.addr);
    }
    eprintln!("inspector: http://{}/  (the agent's screens appear here as it builds them)", cli.addr);

    if cli.mock {
        return direct_access::run_agent(direct_access::MockLlm, cli.artifacts, &goal).await;
    }

    #[cfg(feature = "openai")]
    {
        let llm = build_openai(&cli.model, cli.base_url.as_deref())?;
        direct_access::run_agent(llm, cli.artifacts, &goal).await
    }
    #[cfg(not(feature = "openai"))]
    {
        let _ = (&cli.model, &cli.base_url);
        anyhow::bail!("no real LLM backend compiled in — run with --mock, or rebuild with `--features openai`")
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
