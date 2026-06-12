# ascii-royale — Implementation Plan

Status legend: [ ] todo · [x] done · [~] in progress

## Stage 1 — Scaffold — DONE
- [x] git init, DESIGN.md, PLAN.md
- [x] cargo scaffold, deps, .gitignore, commit

## Stage 2 — Game core (no I/O, fully testable) — DONE
- [x] map.rs: tile types, procedural gen (buildings/trees/lakes/roads), spawn points
- [x] items.rs: weapons table, loot kinds, loot placement
- [x] zone.rs: phase table, hold/shrink state machine, outside-damage
- [x] state.rs: World, Player, Bullet; tick step (inputs → move/fire/pickup/heal,
      bullet flight, damage/armor, death/drops, placements, win check, events)
- [x] bot.rs: FSM brain (flee storm > fight > loot > wander), LOS check
- [x] tests: map walkability, armor math, zone convergence, headless full-match sim

## Stage 3 — Networking — DONE
- [x] protocol.rs: ClientMsg/ServerMsg/Snapshot, postcard framing helpers
- [x] host.rs: game loop task, local client channel pair, iroh accept loop,
      per-conn reader/writer tasks, lobby join handling, disconnects
- [x] client.rs: iroh connect, hello, msg pump
- [x] test: protocol roundtrip
- [x] tests/e2e.rs: real two-peer join/start/snapshot/forfeit over iroh
      (`cargo test --test e2e -- --ignored`, needs network)

## Stage 4 — TUI — DONE
- [x] tui.rs: lobby / countdown / match / results screens, map viewport widget
      (storm wash, next-zone ring, loot, bullets, players), HUD sidebar, kill feed
- [x] TestBackend render tests + `preview` frame-dump dev tool

## Stage 5 — Wiring & polish — DONE
- [x] main.rs: clap (host/join/solo), tokio runtime, terminal guard
- [x] solo mode (host with no iroh endpoint, auto-start)
- [x] README.md with usage + controls
- [x] full build, clippy clean, headless sim test, pty smoke test (solo)

## Post-launch features — DONE
- [x] Guns spawn loaded; aim crosshair; NO AMMO warning; nearest-gun hint
- [x] Aim snap, tracer trails + impact markers, latched fire, damage retune
- [x] Procedural 8-bit sound (rodio, synthesized, snapshot-diff driven)
- [x] Rebindable keys: `k` config screen, ~/.config/ascii-royale/keys.conf
- [x] README with captured frames, LICENSE, published to GitHub

## SSH arena (boxd VM `royale`) — LIVE: ssh -p 22222 play@bore.pub
- [x] `serve` mode + join queue + lifecycle (see above)
- [x] VM created (royale, auto-suspend off), binary built & installed
- [x] sshd on :2222 hardened; `play` guest (no password — none-auth works)
- [x] royale-arena.service active, logs to /var/log/royale.log
- [x] public ingress: bore-tunnel.service → bore.pub:22222 (boxd has no raw
      TCP; bore.pub is a free community relay — swap to a boxd TCP port or
      vanity domain later by replacing that one unit)
- [x] verified from the public internet: keyless stranger → call sign → lobby
- [ ] consider a cargo feature to build without rodio for headless servers
- [ ] nicer long-term ingress: ask boxd for TCP ports (capability exists)

## Ideas for later (not started)
- [ ] Spectate the killer instead of your corpse; match restart from results
- [ ] Shotgun spread / diagonal aiming; throwables; airdrops
- [ ] Minimap; mouse aiming on terminals that report mouse
- [ ] Ship full EndpointAddr in ticket (faster dial, less DNS dependence)
- [ ] Host migration if the host quits (hard: sim state handoff)

## Notes / decisions log
- iroh 1.0.0-rc.1 (current). Ticket = host EndpointId string (n0 discovery
  resolves it; no need to ship full addresses).
- 4-directional movement/fire (simpler, fair vs bots, clean on a grid).
- Per-client visibility-filtered snapshots = fog of war + tiny bandwidth.
- Host can't cheat-proof itself (it IS the server) — acceptable for friends.
