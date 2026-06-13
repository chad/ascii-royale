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
    /// Find and join the public arena — no ticket needed. Fetches the
    /// current ticket over HTTP, then joins peer-to-peer over iroh.
    Play {
        #[arg(long, default_value_t = default_name())]
        name: String,
        /// Arena ticket URL (or a raw ticket). Defaults to the public arena.
        #[arg(long, default_value_t = DEFAULT_ARENA.to_string())]
        arena: String,
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
        /// Serve the landing page + /ticket + /stats on this port.
        #[arg(long)]
        http_port: Option<u16>,
        /// Persist the leaderboard here (survives restarts).
        #[arg(long)]
        stats_file: Option<std::path::PathBuf>,
        /// "Play in your browser" link shown on the landing page.
        #[arg(long)]
        browser_play_url: Option<String>,
    },
}

/// The public arena's ticket endpoint (boxd HTTPS proxy → the arena's HTTP
/// ticket server). `play` GETs this, then joins over iroh.
const DEFAULT_ARENA: &str = "https://royale.boxd.sh/ticket";

fn default_name() -> String {
    std::env::var("USER").unwrap_or_else(|_| "player".into())
}

/// Resolve an arena arg to a ticket: a raw ticket is used directly, anything
/// that looks like a URL is fetched over HTTP(S).
fn resolve_ticket(arena: &str) -> Result<String> {
    let arena = arena.trim();
    if arena.starts_with("http://") || arena.starts_with("https://") {
        let body = ureq::get(arena)
            .call()
            .map_err(|e| anyhow::anyhow!("couldn't reach the arena at {arena}: {e}"))?
            .body_mut()
            .read_to_string()?;
        let ticket = body.trim().to_string();
        if ticket.is_empty() {
            anyhow::bail!("arena returned an empty ticket");
        }
        Ok(ticket)
    } else {
        Ok(arena.to_string())
    }
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
        Command::Play { name, arena } => {
            eprintln!("finding the arena...");
            let ticket = resolve_ticket(&arena)?;
            eprintln!("dropping in...");
            let handle = rt.block_on(client::connect(&ticket, &name))?;
            ui::tui::run(handle, None, false)?;
        }
        Command::Solo { name, bots } => {
            // Solo goes through the lobby too: that's where key config lives.
            let hosted =
                rt.block_on(host::start(HostOpts { name, bots, networked: false }))?;
            ui::tui::run(hosted.handle, None, true)?;
        }
        Command::Serve {
            bots,
            auto_start_secs,
            auto_reset_secs,
            ticket_file,
            http_port,
            stats_file,
            browser_play_url,
        } => {
            return rt.block_on(async {
                let ticket = host::serve(host::ServeOpts {
                    bots,
                    auto_start_secs,
                    auto_reset_secs,
                    ticket_file,
                    http_port,
                    stats_file,
                    browser_play_url,
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
