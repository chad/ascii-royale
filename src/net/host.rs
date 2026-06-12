use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::presets;
use iroh::Endpoint;
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;

use crate::game::state::{MatchPhase, World};
use crate::game::{GameConfig, TPS};

use super::protocol::{recv_frame, send_frame, ClientMsg, ServerHandle, ServerMsg, Standing, ALPN};

const BOT_NAMES: &[&str] = &[
    "Rusty", "Clanker", "Gritbox", "Vex", "Mortar", "Slag", "Pixel", "Doomba", "Crank", "Widget",
    "Bolt", "Socket", "Gauge", "Piston", "Sprocket",
];

/// Everything the game loop hears from the outside world. Connections are
/// identified by a stable `conn` slot, decoupled from player ids so the
/// same connection can play many matches in a row.
enum Inbound {
    Join { name: String, reply: oneshot::Sender<JoinReply> },
    Msg { conn: usize, msg: ClientMsg },
    Disconnected { conn: usize },
}

enum JoinReply {
    /// `welcome` is None when the joiner is queued for the next match.
    Accepted { conn: usize, welcome: Option<ServerMsg>, rx: mpsc::Receiver<ServerMsg> },
    Rejected { reason: String },
}

/// A connected human (the departed leave a None slot behind).
struct Client {
    name: String,
    tx: mpsc::Sender<ServerMsg>,
    /// Player id in the current world; None while queued for the next match.
    player: Option<u8>,
}

/// How the match lifecycle is driven.
struct LoopOpts {
    bots: u8,
    /// Conn 0 is a local boss whose Start messages control the lifecycle
    /// (interactive host / solo). False for the headless arena.
    local_boss: bool,
    /// Arena mode: start automatically this long after a human is in the lobby.
    auto_start_secs: Option<u32>,
    /// Arena mode: return to the lobby this long after a match ends.
    auto_reset_secs: Option<u32>,
    /// Log lifecycle events to stdout (only safe without a TUI).
    log: bool,
}

pub struct HostOpts {
    pub name: String,
    pub bots: u8,
    /// When false (solo mode) no iroh endpoint is created at all.
    pub networked: bool,
}

pub struct Hosted {
    pub handle: ServerHandle,
    /// Present when networked: the string friends pass to `join`.
    pub ticket: Option<String>,
}

/// Boot an interactive match: spawn the authoritative game loop, optionally
/// listen on iroh, and join the host's own player through the same path
/// remote players use.
pub async fn start(opts: HostOpts) -> Result<Hosted> {
    let (inbound_tx, inbound_rx) = mpsc::channel::<Inbound>(256);

    let ticket = if opts.networked {
        let endpoint = bind_endpoint().await?;
        let ticket = endpoint.id().to_string();
        tokio::spawn(accept_loop(endpoint, inbound_tx.clone()));
        Some(ticket)
    } else {
        None
    };

    tokio::spawn(game_loop(
        LoopOpts {
            bots: opts.bots,
            local_boss: true,
            auto_start_secs: None,
            auto_reset_secs: None,
            log: false,
        },
        inbound_rx,
    ));

    // The host's player joins like anyone else, minus the network.
    let (reply_tx, reply_rx) = oneshot::channel();
    inbound_tx
        .send(Inbound::Join { name: opts.name, reply: reply_tx })
        .await
        .ok()
        .context("game loop gone")?;
    let JoinReply::Accepted { conn, welcome, rx } = reply_rx.await? else {
        anyhow::bail!("host player rejected by own lobby");
    };

    // Feed the Welcome through the same channel the loop will use later.
    let (ui_tx, ui_rx) = mpsc::channel::<ServerMsg>(64);
    if let Some(welcome) = welcome {
        ui_tx.send(welcome).await.ok();
    }
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<ClientMsg>(64);
    let pump_in = inbound_tx.clone();
    tokio::spawn(async move {
        let mut rx = rx;
        loop {
            tokio::select! {
                m = rx.recv() => match m {
                    Some(m) => { if ui_tx.send(m).await.is_err() { break } }
                    None => break,
                },
                c = cmd_rx.recv() => match c {
                    Some(c) => { let _ = pump_in.send(Inbound::Msg { conn, msg: c }).await; }
                    None => break,
                },
            }
        }
    });

    Ok(Hosted { handle: ServerHandle { rx: ui_rx, tx: cmd_tx }, ticket })
}

pub struct ServeOpts {
    pub bots: u8,
    pub auto_start_secs: u32,
    pub auto_reset_secs: u32,
    pub ticket_file: Option<PathBuf>,
}

/// Headless arena: no local player, matches start when humans show up and
/// the lobby reopens after every match. Returns the ticket once listening;
/// the arena then runs until the process exits.
pub async fn serve(opts: ServeOpts) -> Result<String> {
    let (inbound_tx, inbound_rx) = mpsc::channel::<Inbound>(256);
    let endpoint = bind_endpoint().await?;
    let ticket = endpoint.id().to_string();
    if let Some(path) = &opts.ticket_file {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        std::fs::write(path, &ticket).context("writing ticket file")?;
    }
    tokio::spawn(accept_loop(endpoint, inbound_tx));
    tokio::spawn(game_loop(
        LoopOpts {
            bots: opts.bots,
            local_boss: false,
            auto_start_secs: Some(opts.auto_start_secs),
            auto_reset_secs: Some(opts.auto_reset_secs),
            log: true,
        },
        inbound_rx,
    ));
    Ok(ticket)
}

async fn bind_endpoint() -> Result<Endpoint> {
    let endpoint = Endpoint::builder(presets::N0)
        .alpns(vec![ALPN.to_vec()])
        .bind()
        .await
        .context("binding iroh endpoint")?;
    // Wait until we're reachable (relay + discovery published).
    endpoint.online().await;
    Ok(endpoint)
}

async fn accept_loop(endpoint: Endpoint, inbound: mpsc::Sender<Inbound>) {
    while let Some(incoming) = endpoint.accept().await {
        let inbound = inbound.clone();
        tokio::spawn(async move {
            let _ = handle_conn(incoming, inbound).await;
        });
    }
}

async fn handle_conn(
    incoming: iroh::endpoint::Incoming,
    inbound: mpsc::Sender<Inbound>,
) -> Result<()> {
    let conn = incoming.await?;
    let (mut send, mut recv) = conn.accept_bi().await?;

    // First frame must be Hello, and quickly.
    let hello = tokio::time::timeout(Duration::from_secs(10), recv_frame::<ClientMsg>(&mut recv))
        .await
        .context("timed out waiting for hello")??;
    let ClientMsg::Hello { name } = hello else {
        anyhow::bail!("expected hello");
    };
    let name = sanitize_name(&name);

    let (reply_tx, reply_rx) = oneshot::channel();
    inbound.send(Inbound::Join { name, reply: reply_tx }).await.ok().context("loop gone")?;
    let (conn_id, mut out_rx) = match reply_rx.await? {
        JoinReply::Accepted { conn, welcome, rx } => {
            if let Some(welcome) = welcome {
                send_frame(&mut send, &welcome).await?;
            }
            (conn, rx)
        }
        JoinReply::Rejected { reason } => {
            send_frame(&mut send, &ServerMsg::Rejected { reason }).await?;
            send.finish()?;
            return Ok(());
        }
    };

    // Writer: game loop -> peer.
    let writer = tokio::spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if send_frame(&mut send, &msg).await.is_err() {
                break;
            }
        }
    });

    // Reader: peer -> game loop. Runs on this task until the peer drops.
    while let Ok(msg) = recv_frame::<ClientMsg>(&mut recv).await {
        if inbound.send(Inbound::Msg { conn: conn_id, msg }).await.is_err() {
            break;
        }
    }
    let _ = inbound.send(Inbound::Disconnected { conn: conn_id }).await;
    writer.abort();
    Ok(())
}

fn sanitize_name(raw: &str) -> String {
    let cleaned: String =
        raw.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').take(12).collect();
    if cleaned.is_empty() {
        "anon".to_string()
    } else {
        cleaned
    }
}

async fn game_loop(opts: LoopOpts, mut inbound: mpsc::Receiver<Inbound>) {
    let config = GameConfig::default();
    let mut world = World::new(rand::random(), config.clone());
    let mut clients: Vec<Option<Client>> = Vec::new();
    let mut standings_sent = false;
    // Tick countdowns driving the arena lifecycle.
    let mut start_in: Option<u32> = None;
    let mut reset_in: Option<u32> = None;

    let mut ticker = tokio::time::interval(Duration::from_millis(config.tick_ms));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            ev = inbound.recv() => {
                let Some(ev) = ev else { return };
                match ev {
                    Inbound::Join { name, reply } => {
                        if clients.iter().flatten().count() >= config.max_players as usize {
                            let _ = reply.send(JoinReply::Rejected {
                                reason: "the arena is full — try again soon".into(),
                            });
                            continue;
                        }
                        let (tx, rx) = mpsc::channel::<ServerMsg>(64);
                        let player = if world.phase == MatchPhase::Lobby {
                            world.add_player(unique_name(&world, name.clone()), false)
                        } else {
                            None // mid-match: queue for the next one
                        };
                        if world.phase == MatchPhase::Lobby && player.is_none() {
                            let _ = reply.send(JoinReply::Rejected {
                                reason: "lobby is full".into(),
                            });
                            continue;
                        }
                        let welcome = player.map(|id| ServerMsg::Welcome {
                            id,
                            map: world.map.clone(),
                            config: config.clone(),
                        });
                        let conn = clients.iter().position(Option::is_none).unwrap_or_else(|| {
                            clients.push(None);
                            clients.len() - 1
                        });
                        let queued = player.is_none();
                        clients[conn] = Some(Client { name: name.clone(), tx, player });
                        let _ = reply.send(JoinReply::Accepted { conn, welcome, rx });
                        if queued {
                            send_to(&clients, conn, ServerMsg::Waiting {
                                alive: world.alive_count(),
                            });
                        } else {
                            broadcast_roster(&world, &clients, start_in);
                        }
                        if opts.log {
                            let humans = clients.iter().flatten().count();
                            println!("[join] {name} ({humans} connected, queued: {queued})");
                        }
                    }
                    Inbound::Msg { conn, msg } => {
                        let player = clients.get(conn).and_then(|c| c.as_ref()).and_then(|c| c.player);
                        match msg {
                            ClientMsg::Input(cmd) => {
                                if let Some(pid) = player {
                                    world.queue_input(pid, cmd);
                                }
                            }
                            ClientMsg::Start if opts.local_boss && conn == 0 => {
                                if world.phase == MatchPhase::Lobby {
                                    start_match(&mut world, &mut clients, opts.bots, start_in);
                                } else if world.phase == MatchPhase::Over {
                                    reset_to_lobby(
                                        &mut world, &mut clients, &config,
                                        &mut standings_sent, &mut start_in, &mut reset_in,
                                        opts.log,
                                    );
                                }
                            }
                            _ => {}
                        }
                    }
                    Inbound::Disconnected { conn } => {
                        if let Some(client) = clients.get_mut(conn).and_then(Option::take) {
                            if let Some(pid) = client.player {
                                world.player_disconnected(pid);
                            }
                            if opts.log {
                                println!("[left] {}", client.name);
                            }
                            if world.phase == MatchPhase::Lobby {
                                broadcast_roster(&world, &clients, start_in);
                            }
                        }
                    }
                }
            }
            _ = ticker.tick() => {
                if world.phase == MatchPhase::Lobby {
                    // Arena: count down to auto-start while humans are present.
                    let Some(secs) = opts.auto_start_secs else { continue };
                    let humans = clients.iter().flatten().filter(|c| c.player.is_some()).count();
                    if humans == 0 {
                        if start_in.take().is_some() {
                            broadcast_roster(&world, &clients, None);
                        }
                        continue;
                    }
                    let t = *start_in.get_or_insert(secs * TPS);
                    if t == 0 {
                        start_match(&mut world, &mut clients, opts.bots, start_in);
                        start_in = None;
                    } else {
                        if t.is_multiple_of(TPS) {
                            broadcast_roster(&world, &clients, Some(t / TPS));
                        }
                        start_in = Some(t - 1);
                    }
                    continue;
                }

                world.step();
                let feed = std::mem::take(&mut world.feed);
                for client in clients.iter().flatten() {
                    match client.player {
                        Some(pid) => {
                            let snap = world.snapshot_for(pid, &feed);
                            // Drop frames on a congested link, don't stall the match.
                            let _ = client.tx.try_send(ServerMsg::Snapshot(Box::new(snap)));
                        }
                        None => {
                            if world.tick.is_multiple_of(TPS as u64 * 2) {
                                let _ = client.tx.try_send(ServerMsg::Waiting {
                                    alive: world.alive_count(),
                                });
                            }
                        }
                    }
                }

                if world.phase == MatchPhase::Over {
                    if !standings_sent {
                        standings_sent = true;
                        send_standings(&world, &clients);
                        reset_in = opts.auto_reset_secs.map(|s| s * TPS);
                        if opts.log {
                            let winner = world
                                .winner
                                .map(|id| world.players[id as usize].name.clone())
                                .unwrap_or_else(|| "nobody".into());
                            println!("[over] {winner} won ({} players)", world.players.len());
                        }
                    }
                    if let Some(t) = reset_in {
                        if t == 0 {
                            reset_to_lobby(
                                &mut world, &mut clients, &config,
                                &mut standings_sent, &mut start_in, &mut reset_in,
                                opts.log,
                            );
                        } else {
                            reset_in = Some(t - 1);
                        }
                    }
                }
            }
        }
    }
}

/// Fill remaining slots with bots and launch the countdown.
fn start_match(world: &mut World, clients: &mut [Option<Client>], bots: u8, start_in: Option<u32>) {
    for i in 0..bots {
        let base = BOT_NAMES
            .get(i as usize)
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("bot-{i}"));
        let name = unique_name(world, base);
        if world.add_player(name, true).is_none() {
            break;
        }
    }
    broadcast_roster(world, clients, start_in);
    world.start_match();
}

/// Fresh island, same connections: everyone (including the queued) is
/// re-added in slot order and gets a new Welcome.
fn reset_to_lobby(
    world: &mut World,
    clients: &mut [Option<Client>],
    config: &GameConfig,
    standings_sent: &mut bool,
    start_in: &mut Option<u32>,
    reset_in: &mut Option<u32>,
    log: bool,
) {
    *world = World::new(rand::random(), config.clone());
    *standings_sent = false;
    *start_in = None;
    *reset_in = None;
    for client in clients.iter_mut().flatten() {
        let name = unique_name(world, client.name.clone());
        client.player = world.add_player(name, false);
        if let Some(id) = client.player {
            let _ = client.tx.try_send(ServerMsg::Welcome {
                id,
                map: world.map.clone(),
                config: config.clone(),
            });
        }
    }
    broadcast_roster(world, clients, None);
    if log {
        println!("[lobby] fresh island, {} back in", clients.iter().flatten().count());
    }
}

fn send_standings(world: &World, clients: &[Option<Client>]) {
    let mut standings: Vec<(u8, Standing)> = world
        .players
        .iter()
        .map(|p| {
            (p.id, Standing {
                name: p.name.clone(),
                placement: p.placement,
                kills: p.kills,
                is_you: false,
            })
        })
        .collect();
    standings.sort_by_key(|(_, s)| s.placement.unwrap_or(u8::MAX));
    for client in clients.iter().flatten() {
        let Some(pid) = client.player else { continue };
        let mut rows: Vec<Standing> = standings.iter().map(|(_, s)| s.clone()).collect();
        for (row, (id, _)) in rows.iter_mut().zip(&standings) {
            row.is_you = *id == pid;
        }
        let _ = client.tx.try_send(ServerMsg::End { standings: rows });
    }
}

fn send_to(clients: &[Option<Client>], conn: usize, msg: ServerMsg) {
    if let Some(client) = clients.get(conn).and_then(|c| c.as_ref()) {
        let _ = client.tx.try_send(msg);
    }
}

fn unique_name(world: &World, base: String) -> String {
    if !world.players.iter().any(|p| p.name == base) {
        return base;
    }
    for n in 2..99 {
        let candidate = format!("{base}{n}");
        if !world.players.iter().any(|p| p.name == candidate) {
            return candidate;
        }
    }
    base
}

fn broadcast_roster(world: &World, clients: &[Option<Client>], start_in: Option<u32>) {
    let names: Vec<String> = world
        .players
        .iter()
        .filter(|p| p.connected || p.is_bot)
        .map(|p| if p.is_bot { format!("{} [bot]", p.name) } else { p.name.clone() })
        .collect();
    let starting_in = start_in.map(|t| t / TPS);
    for client in clients.iter().flatten() {
        if client.player.is_some() {
            let _ = client.tx.try_send(ServerMsg::Roster {
                names: names.clone(),
                starting_in,
            });
        }
    }
}
