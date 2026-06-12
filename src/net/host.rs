use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::presets;
use iroh::Endpoint;
use tokio::sync::{mpsc, oneshot};
use tokio::time::MissedTickBehavior;

use crate::game::state::{MatchPhase, World};
use crate::game::GameConfig;

use super::protocol::{recv_frame, send_frame, ClientMsg, ServerHandle, ServerMsg, Standing, ALPN};

const BOT_NAMES: &[&str] = &[
    "Rusty", "Clanker", "Gritbox", "Vex", "Mortar", "Slag", "Pixel", "Doomba", "Crank", "Widget",
    "Bolt", "Socket", "Gauge", "Piston", "Sprocket",
];

/// Everything the game loop hears from the outside world.
enum Inbound {
    Join { name: String, reply: oneshot::Sender<JoinReply> },
    Msg { id: u8, msg: ClientMsg },
    Disconnected { id: u8 },
}

enum JoinReply {
    Accepted { id: u8, welcome: ServerMsg, rx: mpsc::Receiver<ServerMsg> },
    Rejected { reason: String },
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

/// Boot a match: spawn the authoritative game loop, optionally start
/// listening on iroh, and join the host's own player through the same
/// path remote players use.
pub async fn start(opts: HostOpts) -> Result<Hosted> {
    let (inbound_tx, inbound_rx) = mpsc::channel::<Inbound>(256);

    let ticket = if opts.networked {
        let endpoint = Endpoint::builder(presets::N0)
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .context("binding iroh endpoint")?;
        // Wait until we're reachable (relay + discovery published).
        endpoint.online().await;
        let ticket = endpoint.id().to_string();
        tokio::spawn(accept_loop(endpoint, inbound_tx.clone()));
        Some(ticket)
    } else {
        None
    };

    let seed = rand::random::<u64>();
    tokio::spawn(game_loop(seed, opts.bots, inbound_rx));

    // The host's player joins like anyone else, minus the network.
    let (reply_tx, reply_rx) = oneshot::channel();
    inbound_tx
        .send(Inbound::Join { name: opts.name, reply: reply_tx })
        .await
        .ok()
        .context("game loop gone")?;
    let JoinReply::Accepted { id: _, welcome, rx } = reply_rx.await? else {
        anyhow::bail!("host player rejected by own lobby");
    };

    // Feed the Welcome through the same channel the loop will use later.
    let (ui_tx, ui_rx) = mpsc::channel::<ServerMsg>(64);
    ui_tx.send(welcome).await.ok();
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<ClientMsg>(64);
    let pump_in = inbound_tx.clone();
    tokio::spawn(async move {
        // host's local client is always player 0
        let mut rx = rx;
        loop {
            tokio::select! {
                m = rx.recv() => match m {
                    Some(m) => { if ui_tx.send(m).await.is_err() { break } }
                    None => break,
                },
                c = cmd_rx.recv() => match c {
                    Some(c) => { let _ = pump_in.send(Inbound::Msg { id: 0, msg: c }).await; }
                    None => break,
                },
            }
        }
    });

    Ok(Hosted { handle: ServerHandle { rx: ui_rx, tx: cmd_tx }, ticket })
}

async fn accept_loop(endpoint: Endpoint, inbound: mpsc::Sender<Inbound>) {
    while let Some(incoming) = endpoint.accept().await {
        let inbound = inbound.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(incoming, inbound).await {
                tracing_lite(&format!("connection ended: {e:#}"));
            }
        });
    }
}

// No logging stack in a TUI app; keep a stub so errors aren't silently eaten
// if we ever want to wire this to a debug file.
fn tracing_lite(_msg: &str) {}

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
    let (id, mut out_rx) = match reply_rx.await? {
        JoinReply::Accepted { id, welcome, rx } => {
            send_frame(&mut send, &welcome).await?;
            (id, rx)
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
        if inbound.send(Inbound::Msg { id, msg }).await.is_err() {
            break;
        }
    }
    let _ = inbound.send(Inbound::Disconnected { id }).await;
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

async fn game_loop(seed: u64, bots: u8, mut inbound: mpsc::Receiver<Inbound>) {
    let config = GameConfig::default();
    let mut world = World::new(seed, config.clone());
    // Outboxes indexed by player id; None for bots and the departed.
    let mut outboxes: Vec<Option<mpsc::Sender<ServerMsg>>> = Vec::new();
    let mut ended = false;

    let mut ticker = tokio::time::interval(Duration::from_millis(config.tick_ms));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            ev = inbound.recv() => {
                let Some(ev) = ev else { return };
                match ev {
                    Inbound::Join { name, reply } => {
                        let unique = unique_name(&world, name);
                        match world.add_player(unique, false) {
                            Some(id) => {
                                let (tx, rx) = mpsc::channel::<ServerMsg>(64);
                                outboxes.push(Some(tx));
                                debug_assert_eq!(outboxes.len() - 1, id as usize);
                                let welcome = ServerMsg::Welcome {
                                    id,
                                    map: world.map.clone(),
                                    config: config.clone(),
                                };
                                let _ = reply.send(JoinReply::Accepted { id, welcome, rx });
                                broadcast_roster(&world, &outboxes);
                            }
                            None => {
                                let reason = if world.phase == MatchPhase::Lobby {
                                    "lobby is full".to_string()
                                } else {
                                    "match already started".to_string()
                                };
                                let _ = reply.send(JoinReply::Rejected { reason });
                            }
                        }
                    }
                    Inbound::Msg { id, msg } => match msg {
                        ClientMsg::Input(cmd) => world.queue_input(id, cmd),
                        ClientMsg::Start => {
                            if id == 0 && world.phase == MatchPhase::Lobby {
                                for i in 0..bots {
                                    let name = BOT_NAMES
                                        .get(i as usize)
                                        .map(|s| s.to_string())
                                        .unwrap_or_else(|| format!("bot-{i}"));
                                    if world.add_player(name, true).is_some() {
                                        outboxes.push(None);
                                    }
                                }
                                broadcast_roster(&world, &outboxes);
                                world.start_match();
                            }
                        }
                        ClientMsg::Hello { .. } => {}
                    },
                    Inbound::Disconnected { id } => {
                        world.player_disconnected(id);
                        if let Some(slot) = outboxes.get_mut(id as usize) {
                            *slot = None;
                        }
                        if world.phase == MatchPhase::Lobby {
                            broadcast_roster(&world, &outboxes);
                        }
                    }
                }
            }
            _ = ticker.tick() => {
                if world.phase == MatchPhase::Lobby {
                    continue;
                }
                world.step();
                let feed = std::mem::take(&mut world.feed);
                for (id, outbox) in outboxes.iter().enumerate() {
                    let Some(tx) = outbox else { continue };
                    let snap = world.snapshot_for(id as u8, &feed);
                    // Drop frames on a congested link rather than stalling the match.
                    let _ = tx.try_send(ServerMsg::Snapshot(Box::new(snap)));
                }
                if world.phase == MatchPhase::Over && !ended {
                    ended = true;
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
                    for (id, outbox) in outboxes.iter().enumerate() {
                        let Some(tx) = outbox else { continue };
                        let mut rows: Vec<Standing> =
                            standings.iter().map(|(_, s)| s.clone()).collect();
                        for (row, (pid, _)) in rows.iter_mut().zip(&standings) {
                            row.is_you = *pid == id as u8;
                        }
                        let _ = tx.try_send(ServerMsg::End { standings: rows });
                    }
                }
            }
        }
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

fn broadcast_roster(world: &World, outboxes: &[Option<mpsc::Sender<ServerMsg>>]) {
    let names: Vec<String> = world
        .players
        .iter()
        .filter(|p| p.connected || p.is_bot)
        .map(|p| if p.is_bot { format!("{} [bot]", p.name) } else { p.name.clone() })
        .collect();
    for outbox in outboxes.iter().flatten() {
        let _ = outbox.try_send(ServerMsg::Roster { names: names.clone() });
    }
}
