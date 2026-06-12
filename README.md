# ascii-royale

A battle royale you play in your terminal. Pure text, peer-to-peer over
[iroh](https://iroh.computer) — no game server, no accounts.

Up to 16 combatants drop onto a procedurally generated ASCII island. Scavenge
weapons, dodge visible bullets, outrun the storm. Last one standing wins.
Bots fill empty slots, so it's playable solo too.

```
┌ the island ──────────────────────────────┐┌ status ────────────────┐
│%%%%%%%%%%%....T...#########....~~~~~~....││alive 7   kills 2       │
│%%%%%%%%......)....#,,,,,,,#....~~~~~.....││                        │
│%%%%%......@......+#,,,],,,#..............││HP  #########-----  64  │
│%%%%......... -    #,,,,,,,#....:::::::...││ARM ####----------  31  │
│%%%%.....@.........####,####..............││                        │
│%%%%%..........T.....................T....││Rifle  ammo 24          │
│%%%%%%%..T.T......o..................     ││STORM CLOSING 12s       │
└──────────────────────────────────────────┘└────────────────────────┘
    wasd/arrows move · f/space fire · e pickup · h heal · q quit
```

## Install / build

```sh
cargo build --release        # binary at target/release/ascii-royale
```

## Play

**Host a match** (you play too):

```sh
ascii-royale host                 # default name $USER, 7 bots at start
ascii-royale host --bots 3 --name chad
```

The lobby shows a **ticket** (also printed to the terminal). Send it to
friends. Press **Enter** when everyone's in — bots fill the empty slots.

**Join a match:**

```sh
ascii-royale join <ticket>
```

**Play offline against bots:**

```sh
ascii-royale solo --bots 9
```

## Controls

| key | action |
|---|---|
| `wasd` / arrows | move — this also sets your aim |
| `f` / space | fire — auto-aims at the nearest enemy lined up with you in any direction; otherwise shoots along your `^ v < >` crosshair. Pressing during cooldown fires the instant the weapon is ready. |
| `e` / `g` | pick up the item under you |
| `h` / `m` | use a medkit (+40 HP) |
| Enter | start the match (host, in lobby) |
| `q` / Esc | quit |

## How to win

- **Get a gun first.** You spawn with fists. Buildings (`#` boxes) hold most
  of the loot — walk onto a `)` and press `e`. Guns come loaded; ammo `=`
  packs keep them fed. When you're unarmed the sidebar points at the
  nearest gun on screen.
- Bullets draw full tracer lines (`-` `|`) as they fly and a `*` where they
  land — sidestep incoming fire. To hit someone they must share your row or
  column when you pull the trigger; auto-aim picks the direction, your job
  is getting lined up (and not being lined up yourself).
- Walls `#` and trees `T` block shots; water `~` blocks you but not bullets.
- Loot: `)` weapon · `=` ammo · `+` medkit · `]` vest (absorbs half of each hit).
- The blue `%` wash is the storm. The `o` ring marks where it settles next.
  Storm damage ignores armor and escalates every phase.
- Weapons, roughly fists < pistol < shotgun < SMG < rifle < sniper. The
  sniper hits like a truck but fires once per 1.5s. Shotgun and sniper
  burn 2 ammo per shot.

## How it works

One player hosts; their process runs the authoritative simulation at 10
ticks/second. Everyone else sends inputs and receives personalized,
visibility-filtered snapshots (~1 KB each) — which doubles as fog of war.
Connections are direct QUIC between peers, established by iroh via NAT
holepunching; the ticket is just the host's public key.

Honest caveat: there's no central *game* server, but iroh's default discovery
and relay infrastructure (run by [n0](https://n0.computer)) is used to find
and reach the host. Traffic falls back to relays only when holepunching fails.

See `DESIGN.md` for the full design and `PLAN.md` for implementation status.

## Development

```sh
cargo test                                   # unit + sim + render tests
cargo test --test e2e -- --ignored           # real two-peer match over iroh
cargo test --lib preview -- --ignored --nocapture   # print a rendered frame
```

The sim is fully headless-testable: `full_bot_match_produces_a_winner` runs
entire 8-bot matches to completion in milliseconds.
