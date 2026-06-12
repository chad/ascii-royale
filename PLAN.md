# ascii-royale — Implementation Plan

Status legend: [ ] todo · [x] done · [~] in progress

## Stage 1 — Scaffold
- [x] git init, DESIGN.md, PLAN.md
- [ ] cargo scaffold, deps, .gitignore, commit

## Stage 2 — Game core (no I/O, fully testable)
- [ ] map.rs: tile types, procedural gen (buildings/trees/lakes/roads), spawn points
- [ ] items.rs: weapons table, loot kinds, loot placement
- [ ] zone.rs: phase table, hold/shrink state machine, outside-damage
- [ ] state.rs: World, Player, Bullet; tick step (inputs → move/fire/pickup/heal,
      bullet flight, damage/armor, death/drops, placements, win check, events)
- [ ] bot.rs: FSM brain (flee storm > fight > loot > wander), LOS check
- [ ] tests: map walkability, armor math, zone convergence, headless full-match sim

## Stage 3 — Networking
- [ ] protocol.rs: ClientMsg/ServerMsg/Snapshot, postcard framing helpers
- [ ] host.rs: game loop task, local client channel pair, iroh accept loop,
      per-conn reader/writer tasks, lobby join handling, disconnects
- [ ] client.rs: iroh connect, hello, msg pump
- [ ] test: protocol roundtrip

## Stage 4 — TUI
- [ ] tui.rs: input thread, lobby / countdown / match / results screens,
      map viewport widget (storm wash, loot, bullets, players), HUD sidebar, kill feed

## Stage 5 — Wiring & polish
- [ ] main.rs: clap (host/join/solo), tokio runtime, terminal guard
- [ ] solo mode (host with no listener)
- [ ] README.md with usage + controls
- [ ] full build, clippy, run headless sim test, manual smoke (solo)

## Notes / decisions log
- iroh 1.0.0-rc.1 (current). Ticket = host EndpointId string (n0 discovery
  resolves it; no need to ship full addresses).
- 4-directional movement/fire (simpler, fair vs bots, clean on a grid).
- Per-client visibility-filtered snapshots = fog of war + tiny bandwidth.
- Host can't cheat-proof itself (it IS the server) — acceptable for friends.
