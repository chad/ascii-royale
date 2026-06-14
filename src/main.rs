use anyhow::Result;
use clap::{Parser, Subcommand};

use ascii_royale::net;
use ascii_royale::net::client;
use ascii_royale::net::host::{self, HostOpts};
use ascii_royale::net::protocol::parse_hex_color;
use ascii_royale::ui;
use ascii_royale::ui::profile::Profile;

#[derive(Parser)]
#[command(name = "ascii-royale", version, about = "Terminal battle royale, peer-to-peer over iroh")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Name + skin flags shared by the player-facing subcommands. Either flag
/// overrides the persisted profile (~/.config/ascii-royale/profile.conf) and,
/// when set, is saved back so it sticks.
#[derive(clap::Args)]
struct Who {
    /// Display name (defaults to your saved profile).
    #[arg(long)]
    name: Option<String>,
    /// Skin color as a hex code, e.g. ff8800 (defaults to your saved profile).
    #[arg(long)]
    color: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Host a match: prints a ticket friends can join with.
    Host {
        #[command(flatten)]
        who: Who,
        /// Bots added when the match starts.
        #[arg(long, default_value_t = 7)]
        bots: u8,
        /// Advertise this game on the public lobby so `browse` can find it.
        #[arg(long)]
        announce: bool,
    },
    /// Join a hosted match by ticket.
    Join {
        /// The ticket the host shared.
        ticket: String,
        #[command(flatten)]
        who: Who,
    },
    /// Find and join the public arena — no ticket needed. Fetches the
    /// current ticket over HTTP, then joins peer-to-peer over iroh.
    Play {
        #[command(flatten)]
        who: Who,
        /// Arena ticket URL (or a raw ticket). Defaults to the public arena.
        #[arg(long, default_value_t = DEFAULT_ARENA.to_string())]
        arena: String,
    },
    /// Browse open games on the gossip lobby and join one — fully decentralized
    /// discovery, no ticket sharing.
    Browse {
        #[command(flatten)]
        who: Who,
        /// Bootstrap id URL (or a raw gossip id). Defaults to the public arena.
        #[arg(long, default_value_t = DEFAULT_LOBBY.to_string())]
        bootstrap: String,
    },
    /// Play offline against bots.
    Solo {
        #[command(flatten)]
        who: Who,
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
/// The public arena's gossip bootstrap id endpoint.
const DEFAULT_LOBBY: &str = "https://royale.boxd.sh/lobby";

/// Load the saved profile, apply any `--name`/`--color` overrides, and persist
/// the result so the choice sticks for next time.
fn resolve_profile(who: &Who) -> Profile {
    let mut p = Profile::load();
    let mut changed = false;
    if let Some(name) = &who.name {
        p.name = ascii_royale::ui::profile::sanitize_name(name);
        changed = true;
    }
    if let Some(color) = &who.color {
        if let Some(c) = parse_hex_color(color) {
            p.color = c;
            changed = true;
        } else {
            eprintln!("ignoring bad --color '{color}' (want a hex code like ff8800)");
        }
    }
    if changed {
        let _ = p.save();
    }
    p
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
        Command::Host { who, bots, announce } => {
            let profile = resolve_profile(&who);
            eprintln!("binding iroh endpoint (waiting to be reachable)...");
            let announce = if announce {
                eprintln!("fetching lobby bootstrap...");
                Some(resolve_ticket(DEFAULT_LOBBY)?)
            } else {
                None
            };
            let hosted = rt.block_on(host::start(HostOpts {
                name: profile.name.clone(),
                color: profile.color,
                bots,
                networked: true,
                announce,
            }))?;
            let ticket = hosted.ticket.clone();
            if let Some(t) = &ticket {
                eprintln!("ticket: {t}");
            }
            ui::tui::run(hosted.handle, ticket, true, profile)?;
        }
        Command::Join { ticket, who } => {
            let profile = resolve_profile(&who);
            eprintln!("dialing host...");
            let handle = rt.block_on(client::connect(&ticket, &profile.name, profile.color))?;
            ui::tui::run(handle, None, false, profile)?;
        }
        Command::Play { who, arena } => {
            let profile = resolve_profile(&who);
            eprintln!("finding the arena...");
            let ticket = resolve_ticket(&arena)?;
            eprintln!("dropping in...");
            let handle = rt.block_on(client::connect(&ticket, &profile.name, profile.color))?;
            ui::tui::run(handle, None, false, profile)?;
        }
        Command::Browse { who, bootstrap } => {
            let profile = resolve_profile(&who);
            eprintln!("finding the lobby...");
            let boot = resolve_ticket(&bootstrap)?;
            let boot_id = boot.trim().parse().ok();
            let listings = rt.block_on(net::lobby::discover(boot_id))?;
            // Browse + pick happens in the TUI; it returns a chosen ticket.
            if let Some(ticket) = ui::tui::browse(listings)? {
                eprintln!("dropping in...");
                let handle = rt.block_on(client::connect(&ticket, &profile.name, profile.color))?;
                ui::tui::run(handle, None, false, profile)?;
            }
        }
        Command::Solo { who, bots } => {
            let profile = resolve_profile(&who);
            // Solo goes through the lobby too: that's where config lives.
            let hosted = rt.block_on(host::start(HostOpts {
                name: profile.name.clone(),
                color: profile.color,
                bots,
                networked: false,
                announce: None,
            }))?;
            ui::tui::run(hosted.handle, None, true, profile)?;
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
                    announce: true,
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
