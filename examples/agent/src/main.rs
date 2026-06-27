//! Example agent CLI: publish the demo deck, then wait for a user interaction.
//!
//! Closes the loop with no API:
//!   example-agent --artifacts ./artifacts publish        # author the deck
//!   example-agent --artifacts ./artifacts wait sales     # block until a click/submit
//! Meanwhile open the surface in a browser (via gui-server) and interact.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use gui_agent::Workspace;
use gui_schema::SurfacePath;

#[derive(Parser)]
#[command(name = "example-agent", about = "Publish a demo deck and react to backflow.")]
struct Cli {
    #[arg(long, default_value = "artifacts")]
    artifacts: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Publish the three-surface demo deck.
    Publish,
    /// Block until a backflow event arrives for a surface, then print it.
    Wait {
        #[arg(default_value = "sales")]
        surface: String,
        #[arg(long, default_value_t = 60_000)]
        timeout_ms: u64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let ws = Workspace::new(cli.artifacts);
    match cli.cmd {
        Cmd::Publish => {
            example_agent::publish_demo(&ws)?;
            println!("published demo deck (sales → region-detail → settings)");
        }
        Cmd::Wait { surface, timeout_ms } => {
            let path = SurfacePath::new(example_agent::AGENT, example_agent::DECK, &surface)?;
            println!("waiting for backflow on {} …", path.id());
            match ws.wait_for_backflow(&path, None, Duration::from_millis(timeout_ms))? {
                Some(be) => println!("received: {}", serde_json::to_string_pretty(&be.event)?),
                None => println!("timed out with no backflow"),
            }
        }
    }
    Ok(())
}
