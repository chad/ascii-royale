# ascii-royale — Design

A battle-royale you play in your terminal. Pure text, peer-to-peer, no central server.

## Concept

Up to 16 combatants drop onto a procedurally generated 160×100 ASCII island —
buildings, forests, lakes, roads. You scavenge weapons and gear, fight with
visible, dodgeable projectiles, and outrun a shrinking storm zone. Last one
standing wins. Bots fill empty lobby slots so a match is always playable.

Design pillars:

1. **Terminal-native, not terminal-limited.** The constraints of a character
   grid become the aesthetic: bullets are glyphs flying across the map that you
   can physically dodge, the storm is a wall of blue creeping across the screen.
2. **No server, no signup.** One player hosts; the match lives in their
   process. Joining is pasting one short ticket string. iroh handles NAT
   traversal/holepunching so it works across home networks.
3. **A match fits in a coffee break.** ~5–8 minutes, 10 ticks/second pace —
   fast enough to be tense, slow enough to play over SSH.

## Architecture

```
┌────────────── host process ──────────────┐
│  Simulation (authoritative, 10 Hz)       │
│  ├─ world step: input → move/fire/loot   │
│  ├─ bots (FSM brains)                    │
│  └─ zone, bullets, damage, win check     │
│        ▲ ClientMsg          │ ServerMsg  │
│  ┌─────┴─────┬──────────────┴─────────┐  │
│  │ local client (channel pair)        │  │
│  │ iroh accept loop ── conn per peer ─┼──┼── QUIC (iroh, ALPN ascii-royale/0)
│  └────────────────────────────────────┘  │
└──────────────────────────────────────────┘
          joiners: iroh connect → same ClientMsg/ServerMsg protocol
```

- **Host-authoritative.** The host runs the only simulation. Clients send
  discrete input commands; the host applies them on the next tick and sends
  each client a personalized snapshot. No client prediction — at 10 Hz on a
  text grid, raw round-trips feel fine and cheating is structurally hard.
- **The host is also a player**, wired through an in-process channel pair that
  speaks the exact same protocol as remote peers. `solo` mode is just a host
  with zero listeners and bots.
- **Transport**: iroh `Endpoint`, one bidirectional QUIC stream per client,
  length-prefixed postcard frames. The "ticket" is the host's EndpointId;
  joiners dial it directly (n0 discovery + holepunching, relay fallback).
- **Bandwidth**: snapshots only include entities within ~50×25 cells of the
  recipient (also gives fog-of-war), plus global facts (alive count, zone,
  kill feed). The static map is sent once at join. Worst case ≈ 1–2 KB ×
  10 Hz × 15 peers from the host — trivial.

## Simulation

- **Tick**: 100 ms. Move = 1 cell/tick, 4-directional. Facing = last move.
- **Tiles**: grass `.`, tree `T` (blocks walk+shots), water `~` (blocks walk),
  wall `#` (blocks walk+shots), floor, road. Loot concentrates in buildings.
- **Weapons** (bullets travel N cells/tick, so range and dodge are real):

  | weapon  | dmg | cooldown | speed | range | ammo/shot |
  |---------|-----|----------|-------|-------|-----------|
  | Fists   | 10  | 3        | melee | 1     | 0         |
  | Pistol  | 15  | 4        | 3     | 25    | 1         |
  | SMG     | 8   | 1        | 3     | 20    | 1         |
  | Shotgun | 30  | 8        | 2     | 8     | 2         |
  | Rifle   | 22  | 5        | 4     | 35    | 1         |
  | Sniper  | 70  | 15       | 6     | 60    | 2         |

- **Gear**: one weapon slot (pickup swaps, old drops), shared ammo pool,
  medkits (+40 HP, short cooldown), vest (armor pool, absorbs half of each
  hit until depleted).
- **Zone**: circle that holds, then shrinks toward a drifting center through
  7 phases with escalating tick damage. Standing outside hurts, then kills.
- **Bots**: per-bot FSM — flee storm > fight visible enemy in range (with aim
  error and strafing) > grab loot > wander toward points of interest. Run
  inside the host sim; indistinguishable on the wire because they aren't on it.
- **Death**: your gear drops where you fall; you spectate from your corpse.
  Placement = players remaining + 1. Kill feed events go to everyone.

## Protocol (postcard over QUIC, u32-LE length prefix)

```
ClientMsg: Hello{name} | Input(Move dir | Fire | Pickup | Heal) | Start | Ping
ServerMsg: Welcome{id, map, config} | Roster | Countdown | Snapshot | GameOver | Pong
Snapshot:  tick, self state (hp/armor/weapon/ammo/medkits/cooldowns),
           nearby players/bullets/loot, zone (current + next circle, damage),
           alive count, events since last tick
```

`Start` is only honored from the host's local client. Joins are lobby-only.

## TUI (ratatui)

```
┌ viewport (centered on you, storm = blue wash) ─┐┌ sidebar ────────┐
│ . . T T . # # # # .   - bullet                 ││ HP   ███████ 82 │
│ . @ . . . # + . r # < loot                     ││ ARM  ██░░░░░ 25 │
│ . . ~ ~ . # # = # #                            ││ Rifle  ammo 24  │
│ enemies @ red, you @ yellow                    ││ Medkits 2  K 3  │
│                                                ││ Alive 7         │
│                                                ││ Zone shrink 38s │
│                                                ││ ─ kill feed ─   │
└────────────────────────────────────────────────┘└─────────────────┘
 wasd/arrows move · f/space fire · e pickup · h heal · q quit
```

Screens: lobby (ticket + roster, host presses Enter) → countdown → match →
results (placement, winner, kills).

## Crate layout

```
src/main.rs        clap CLI: host | join <ticket> | solo
src/game/          map.rs (gen), state.rs (world + step), combat in state,
                   items.rs, zone.rs, bot.rs
src/net/           protocol.rs (messages + framing), host.rs, client.rs
src/ui/            tui.rs (screens, map widget, input thread)
```

Deps: iroh 1.0.0-rc.1, tokio, ratatui 0.30, serde + postcard, rand, clap, anyhow.
