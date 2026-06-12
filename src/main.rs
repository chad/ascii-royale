mod game;
mod net;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};

use net::host::{self, HostOpts};
use net::protocol::ClientMsg;

#[derive(Parser)]
#[command(name = "ascii-royale", version, about = "Terminal battle royale, peer-to-peer over iroh")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Host a match: prints a ticket friends can join with.
    Host {
        /// Your display name.
        #[arg(long, default_value_t = default_name())]
        name: String,
        /// Bots added when the match starts.
        #[arg(long, default_value_t = 7)]
        bots: u8,
    },
    /// Join a hosted match by ticket.
    Join {
        /// The ticket the host shared.
        ticket: String,
        #[arg(long, default_value_t = default_name())]
        name: String,
    },
    /// Play offline against bots.
    Solo {
        #[arg(long, default_value_t = default_name())]
        name: String,
        #[arg(long, default_value_t = 9)]
        bots: u8,
    },
}

fn default_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| "player".into())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // The TUI owns the main thread; tokio worker threads run sim + network.
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;

    match cli.command {
        Command::Host { name, bots } => {
            eprintln!("binding iroh endpoint (waiting to be reachable)...");
            let hosted =
                rt.block_on(host::start(HostOpts { name, bots, networked: true }))?;
            let ticket = hosted.ticket.clone();
            ui::tui::run(hosted.handle, ticket, true)?;
        }
        Command::Join { ticket, name } => {
            eprintln!("dialing host...");
            let handle = rt.block_on(net::client::connect(&ticket, &name))?;
            ui::tui::run(handle, None, false)?;
        }
        Command::Solo { name, bots } => {
            let hosted =
                rt.block_on(host::start(HostOpts { name, bots, networked: false }))?;
            // No lobby to linger in offline: drop straight in.
            hosted.handle.tx.blocking_send(ClientMsg::Start)?;
            ui::tui::run(hosted.handle, None, true)?;
        }
    }

    rt.shutdown_background();
    Ok(())
}
