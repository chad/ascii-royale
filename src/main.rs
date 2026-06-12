use anyhow::Result;
use clap::{Parser, Subcommand};

use ascii_royale::net::client;
use ascii_royale::net::host::{self, HostOpts};
use ascii_royale::ui;

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
    /// Run a headless arena: no local player, matches start automatically
    /// when humans join and the lobby reopens after every match.
    Serve {
        #[arg(long, default_value_t = 7)]
        bots: u8,
        /// Seconds the lobby waits (with at least one human) before starting.
        #[arg(long, default_value_t = 20)]
        auto_start_secs: u32,
        /// Seconds the results screen lingers before a fresh lobby.
        #[arg(long, default_value_t = 12)]
        auto_reset_secs: u32,
        /// Write the join ticket here for launcher scripts to read.
        #[arg(long)]
        ticket_file: Option<std::path::PathBuf>,
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
            if let Some(t) = &ticket {
                // Also goes to scrollback so it survives the TUI session.
                eprintln!("ticket: {t}");
            }
            ui::tui::run(hosted.handle, ticket, true)?;
        }
        Command::Join { ticket, name } => {
            eprintln!("dialing host...");
            let handle = rt.block_on(client::connect(&ticket, &name))?;
            ui::tui::run(handle, None, false)?;
        }
        Command::Solo { name, bots } => {
            // Solo goes through the lobby too: that's where key config lives.
            let hosted =
                rt.block_on(host::start(HostOpts { name, bots, networked: false }))?;
            ui::tui::run(hosted.handle, None, true)?;
        }
        Command::Serve { bots, auto_start_secs, auto_reset_secs, ticket_file } => {
            return rt.block_on(async {
                let ticket = host::serve(host::ServeOpts {
                    bots,
                    auto_start_secs,
                    auto_reset_secs,
                    ticket_file,
                })
                .await?;
                println!("[arena] ticket: {ticket}");
                println!("[arena] join with: ascii-royale join {ticket}");
                tokio::signal::ctrl_c().await?;
                Ok(())
            });
        }
    }

    rt.shutdown_background();
    Ok(())
}
