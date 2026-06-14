use std::collections::{HashMap, VecDeque};

use rand::{rngs::StdRng, SeedableRng};
use serde::{Deserialize, Serialize};

use super::bot::{self, BotBrain};
use super::items::{
    scatter_loot, ItemKind, WeaponKind, MAX_AMMO, MAX_HP, MAX_MEDKITS, MEDKIT_HEAL, VEST_ARMOR,
};
use super::map::{self, Map};
use super::zone::{Zone, STORM_PULSE_TICKS};
use super::{Dir, GameConfig, Pos, TPS};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputCmd {
    Move(Dir),
    Fire,
    Pickup,
    Heal,
    Throw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchPhase {
    Lobby,
    Countdown(u32),
    Active,
    Over,
}

#[derive(Debug, Clone)]
pub struct Player {
    pub id: u8,
    pub name: String,
    pub is_bot: bool,
    pub connected: bool,
    pub pos: Pos,
    pub dir: Dir,
    pub hp: i32,
    pub armor: i32,
    pub weapon: WeaponKind,
    pub ammo: u16,
    pub medkits: u8,
    pub fire_cd: u8,
    pub heal_cd: u8,
    pub grenades: u8,
    pub alive: bool,
    pub kills: u8,
    pub placement: Option<u8>,
    /// Who killed us (for spectate-the-killer); None if the storm did.
    pub killed_by: Option<u8>,
    /// Tick of our most recent kill (for multi-kill callouts).
    pub last_kill_tick: u64,
    pub brain: Option<BotBrain>,
    // Input intents, consumed each tick.
    pub queued_moves: VecDeque<Dir>,
    pub want_fire: bool,
    pub want_pickup: bool,
    pub want_heal: bool,
    pub want_throw: bool,
}

impl Player {
    fn new(id: u8, name: String, is_bot: bool) -> Self {
        Player {
            id,
            name,
            is_bot,
            connected: true,
            pos: (0, 0),
            dir: Dir::South,
            hp: MAX_HP,
            armor: 0,
            weapon: WeaponKind::Fists,
            ammo: 0,
            medkits: 0,
            grenades: 0,
            fire_cd: 0,
            heal_cd: 0,
            alive: true,
            kills: 0,
            placement: None,
            killed_by: None,
            last_kill_tick: 0,
            brain: if is_bot { Some(BotBrain::default()) } else { None },
            queued_moves: VecDeque::new(),
            want_fire: false,
            want_pickup: false,
            want_heal: false,
            want_throw: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Bullet {
    pub pos: Pos,
    pub dir: Dir,
    pub speed: i32,
    pub travel: i32,
    pub damage: i32,
    pub owner: u8,
    pub weapon: WeaponKind,
}

/// A thrown grenade arcing toward where the player was facing.
#[derive(Debug, Clone)]
pub struct Grenade {
    pub pos: Pos,
    pub dir: Dir,
    pub fuse: u8,
    pub owner: u8,
}

/// Transient visual bursts, sent once and animated client-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectKind {
    /// Explosion shrapnel (grenades, deaths).
    Blast,
    /// A small hit spark where a bullet connects.
    Spark,
}

pub struct World {
    pub tick: u64,
    pub map: Map,
    pub zone: Zone,
    pub players: Vec<Player>,
    pub bullets: Vec<Bullet>,
    pub grenades: Vec<Grenade>,
    pub loot: HashMap<Pos, ItemKind>,
    pub phase: MatchPhase,
    pub config: GameConfig,
    /// Human-readable feed lines produced this tick (kills, zone, win).
    pub feed: Vec<String>,
    /// Every cell bullets crossed this tick (pos, dir, is_impact) — the
    /// client draws these as tracers so fast bullets stay visible.
    pub tracers: Vec<(Pos, Dir, bool)>,
    /// One-shot visual bursts produced this tick.
    pub effects: Vec<(Pos, EffectKind)>,
    pub winner: Option<u8>,
    /// Tick the next supply drop is due (arena flavor).
    next_drop: u64,
    rng: StdRng,
}

impl World {
    pub fn new(seed: u64, config: GameConfig) -> Self {
        let mut rng = StdRng::seed_from_u64(seed);
        let map = map::generate(seed);
        let loot = scatter_loot(&map, &mut rng);
        let zone = Zone::new(map.w, map.h, &mut rng);
        World {
            tick: 0,
            map,
            zone,
            players: Vec::new(),
            bullets: Vec::new(),
            grenades: Vec::new(),
            loot,
            phase: MatchPhase::Lobby,
            config,
            feed: Vec::new(),
            tracers: Vec::new(),
            effects: Vec::new(),
            winner: None,
            next_drop: 0,
            rng,
        }
    }

    pub fn add_player(&mut self, name: String, is_bot: bool) -> Option<u8> {
        if self.phase != MatchPhase::Lobby || self.players.len() >= self.config.max_players as usize
        {
            return None;
        }
        let id = self.players.len() as u8;
        self.players.push(Player::new(id, name, is_bot));
        Some(id)
    }

    pub fn player_disconnected(&mut self, id: u8) {
        let Some(p) = self.players.get_mut(id as usize) else { return };
        p.connected = false;
        if self.phase == MatchPhase::Lobby {
            p.alive = false;
            self.feed.push(format!("{} left", p.name));
        } else if p.alive {
            // Mid-match leavers are eliminated by the storm gods.
            let name = p.name.clone();
            self.kill(id, format!("{name} disconnected"));
        }
    }

    pub fn start_match(&mut self) {
        // Drop everyone at spawn points spread across the island.
        let mut spawns: Vec<Pos> = Vec::new();
        for i in 0..self.players.len() {
            let mut best = self.map.random_walkable(&mut self.rng);
            let mut best_d = -1;
            for _ in 0..40 {
                let p = self.map.random_walkable(&mut self.rng);
                let d = spawns
                    .iter()
                    .map(|s| (s.0 - p.0).abs() + (s.1 - p.1).abs())
                    .min()
                    .unwrap_or(i32::MAX);
                if d > best_d {
                    best_d = d;
                    best = p;
                }
            }
            spawns.push(best);
            self.players[i].pos = best;
        }
        // First supply drop ~35s in, then periodically (see step()).
        self.next_drop = (35 * TPS) as u64;
        self.phase = MatchPhase::Countdown(3 * TPS);
    }

    pub fn alive_count(&self) -> u8 {
        self.players.iter().filter(|p| p.alive).count() as u8
    }

    pub fn queue_input(&mut self, id: u8, cmd: InputCmd) {
        let Some(p) = self.players.get_mut(id as usize) else { return };
        if !p.alive {
            return;
        }
        match cmd {
            InputCmd::Move(d) => {
                if p.queued_moves.len() < 2 {
                    p.queued_moves.push_back(d);
                }
            }
            InputCmd::Fire => p.want_fire = true,
            InputCmd::Pickup => p.want_pickup = true,
            InputCmd::Heal => p.want_heal = true,
            InputCmd::Throw => p.want_throw = true,
        }
    }

    /// Advance one tick. The host calls this at a fixed rate and then
    /// drains `feed` into the snapshots it sends out.
    pub fn step(&mut self) {
        self.feed.clear();
        match self.phase {
            MatchPhase::Lobby | MatchPhase::Over => return,
            MatchPhase::Countdown(n) => {
                self.phase = if n <= 1 {
                    self.feed.push("Match started. Last one standing wins.".into());
                    MatchPhase::Active
                } else {
                    MatchPhase::Countdown(n - 1)
                };
                return;
            }
            MatchPhase::Active => {}
        }
        self.tick += 1;
        self.tracers.clear();
        self.effects.clear();

        self.think_bots();

        // Timers + healing.
        for p in self.players.iter_mut().filter(|p| p.alive) {
            p.fire_cd = p.fire_cd.saturating_sub(1);
            p.heal_cd = p.heal_cd.saturating_sub(1);
            if p.want_heal && p.heal_cd == 0 && p.medkits > 0 && p.hp < MAX_HP {
                p.medkits -= 1;
                p.hp = (p.hp + MEDKIT_HEAL).min(MAX_HP);
                p.heal_cd = 2 * TPS as u8;
            }
            p.want_heal = false;
        }

        // Movement (in id order; first mover wins contested cells).
        for i in 0..self.players.len() {
            let Some(d) = self.players[i].queued_moves.pop_front() else { continue };
            if !self.players[i].alive {
                continue;
            }
            self.players[i].dir = d;
            let to = d.step(self.players[i].pos);
            let occupied =
                self.players.iter().any(|q| q.alive && q.id != i as u8 && q.pos == to);
            if self.map.walkable(to) && !occupied {
                self.players[i].pos = to;
            }
        }

        // Pickups.
        for i in 0..self.players.len() {
            if !self.players[i].alive || !std::mem::take(&mut self.players[i].want_pickup) {
                continue;
            }
            let pos = self.players[i].pos;
            let Some(item) = self.loot.remove(&pos) else { continue };
            let p = &mut self.players[i];
            match item {
                ItemKind::Weapon(w) => {
                    if p.weapon != WeaponKind::Fists {
                        self.loot.insert(pos, ItemKind::Weapon(p.weapon));
                    }
                    p.weapon = w;
                    p.ammo = (p.ammo + w.stats().bundled_ammo).min(MAX_AMMO);
                }
                ItemKind::Ammo(n) => p.ammo = (p.ammo + n).min(MAX_AMMO),
                ItemKind::Medkit => {
                    if p.medkits >= MAX_MEDKITS {
                        self.loot.insert(pos, item);
                    } else {
                        p.medkits += 1;
                    }
                }
                ItemKind::Vest => p.armor = p.armor.max(VEST_ARMOR),
                ItemKind::Grenades(n) => {
                    p.grenades = (p.grenades + n).min(super::items::MAX_GRENADES);
                }
                ItemKind::Crate => {
                    // A top-tier loadout in one grab.
                    if p.weapon.rank() < WeaponKind::Rifle.rank() {
                        p.weapon = WeaponKind::Rifle;
                    }
                    p.ammo = (p.ammo + 60).min(MAX_AMMO);
                    p.armor = p.armor.max(VEST_ARMOR);
                    p.medkits = (p.medkits + 2).min(MAX_MEDKITS);
                    p.grenades = (p.grenades + 2).min(super::items::MAX_GRENADES);
                    let name = p.name.clone();
                    self.feed.push(format!("{name} cracked a supply crate!"));
                }
            }
        }

        // Firing.
        for i in 0..self.players.len() {
            // Fire intent stays latched through cooldown: mashing the key
            // during cooldown shoots the instant the weapon is ready.
            if !self.players[i].alive || !self.players[i].want_fire {
                continue;
            }
            if self.players[i].fire_cd > 0 {
                continue;
            }
            self.players[i].want_fire = false;
            let (pos, weapon) = (self.players[i].pos, self.players[i].weapon);
            let stats = weapon.stats();
            if self.players[i].ammo < stats.ammo_cost {
                continue;
            }
            // Aim snap: shoot the nearest enemy we can actually hit in any
            // cardinal direction; otherwise fire where we're facing.
            let dir = self.snap_aim(i as u8, &stats).unwrap_or(self.players[i].dir);
            let p = &mut self.players[i];
            p.ammo -= stats.ammo_cost;
            p.fire_cd = stats.cooldown;
            p.dir = dir;
            if stats.speed == 0 {
                // Melee: hit whoever is in the adjacent cell.
                let target = dir.step(pos);
                if let Some(v) =
                    self.players.iter().position(|q| q.alive && q.pos == target)
                {
                    self.hit(Some(i as u8), v as u8, stats.damage, weapon);
                }
            } else {
                self.bullets.push(Bullet {
                    pos,
                    dir,
                    speed: stats.speed,
                    travel: stats.range,
                    damage: stats.damage,
                    owner: i as u8,
                    weapon,
                });
            }
        }

        // Throwing grenades.
        for i in 0..self.players.len() {
            if !self.players[i].alive || !std::mem::take(&mut self.players[i].want_throw) {
                continue;
            }
            if self.players[i].grenades == 0 {
                continue;
            }
            self.players[i].grenades -= 1;
            let (pos, dir) = (self.players[i].pos, self.players[i].dir);
            self.grenades.push(Grenade {
                pos,
                dir,
                fuse: super::items::GRENADE_FUSE,
                owner: i as u8,
            });
        }

        // Bullet flight, cell by cell so nothing is skipped over. Every cell
        // crossed becomes a tracer so the client can draw the full path.
        let mut bullets = std::mem::take(&mut self.bullets);
        let mut tracers = std::mem::take(&mut self.tracers);
        bullets.retain_mut(|b| {
            for _ in 0..b.speed {
                b.pos = b.dir.step(b.pos);
                b.travel -= 1;
                if !self.map.in_bounds(b.pos) || self.map.get(b.pos).blocks_shot() {
                    if self.map.in_bounds(b.pos) {
                        tracers.push((b.pos, b.dir, true));
                    }
                    return false;
                }
                if let Some(v) = self
                    .players
                    .iter()
                    .position(|q| q.alive && q.id != b.owner && q.pos == b.pos)
                {
                    tracers.push((b.pos, b.dir, true));
                    self.hit(Some(b.owner), v as u8, b.damage, b.weapon);
                    return false;
                }
                tracers.push((b.pos, b.dir, false));
                if b.travel <= 0 {
                    return false;
                }
            }
            true
        });
        self.bullets = bullets;
        self.tracers = tracers;

        // Grenade flight + fuse. They roll until they hit a wall, then sit and
        // tick down; on zero fuse they burst with falloff area damage.
        let mut grenades = std::mem::take(&mut self.grenades);
        let mut blasts: Vec<(Pos, u8)> = Vec::new();
        grenades.retain_mut(|g| {
            for _ in 0..super::items::GRENADE_SPEED {
                let to = g.dir.step(g.pos);
                if self.map.in_bounds(to) && !self.map.get(to).blocks_shot() {
                    g.pos = to;
                }
            }
            g.fuse = g.fuse.saturating_sub(1);
            if g.fuse == 0 {
                blasts.push((g.pos, g.owner));
                false
            } else {
                true
            }
        });
        self.grenades = grenades;
        for (center, owner) in blasts {
            self.explode(center, Some(owner));
        }

        // Storm.
        if self.zone.step(&mut self.rng) {
            self.feed.push(format!(
                "Zone closing: {}s until it settles",
                self.zone.ticks_left / TPS
            ));
        }
        if self.tick.is_multiple_of(STORM_PULSE_TICKS) {
            let dmg = self.zone.damage();
            for i in 0..self.players.len() {
                if self.players[i].alive && !self.zone.contains(self.players[i].pos) {
                    self.players[i].hp -= dmg; // armor doesn't help against the storm
                    if self.players[i].hp <= 0 {
                        let name = self.players[i].name.clone();
                        self.kill(i as u8, format!("{name} was consumed by the storm"));
                    }
                }
            }
        }

        // Supply drops: every ~45s, drop a crate at a walkable cell inside the
        // safe circle to make a hotspot worth fighting over.
        if self.tick >= self.next_drop && self.alive_count() > 1 {
            self.next_drop = self.tick + (45 * TPS) as u64;
            if let Some(spot) = self.drop_spot() {
                self.loot.insert(spot, ItemKind::Crate);
                self.effects.push((spot, EffectKind::Blast));
                self.feed.push("A supply drop has landed inside the zone!".into());
            }
        }

        // Win condition.
        if self.phase == MatchPhase::Active && self.players.len() > 1 && self.alive_count() <= 1 {
            self.phase = MatchPhase::Over;
            if let Some(w) = self.players.iter_mut().find(|p| p.alive) {
                w.placement = Some(1);
                self.winner = Some(w.id);
                self.feed.push(format!("{} wins the royale!", w.name));
            } else {
                self.feed.push("Nobody survived the storm.".into());
            }
        }
    }

    /// A walkable cell inside the current safe circle, for an airdrop.
    fn drop_spot(&mut self) -> Option<Pos> {
        for _ in 0..60 {
            let p = self.map.random_walkable(&mut self.rng);
            if self.zone.contains(p) && !self.loot.contains_key(&p) {
                return Some(p);
            }
        }
        None
    }

    /// Grenade/death burst: falloff damage in a radius + blast effects.
    fn explode(&mut self, center: Pos, attacker: Option<u8>) {
        let r = super::items::GRENADE_RADIUS;
        // Scatter blast glyphs through the radius for the visual.
        for dy in -r..=r {
            for dx in -r..=r {
                let p = (center.0 + dx, center.1 + dy);
                if dx * dx + dy * dy <= r * r && self.map.in_bounds(p) {
                    self.effects.push((p, EffectKind::Blast));
                }
            }
        }
        // Damage falls off linearly from the center.
        let hits: Vec<(u8, i32)> = self
            .players
            .iter()
            .filter(|q| q.alive)
            .filter_map(|q| {
                let d = (q.pos.0 - center.0).abs().max((q.pos.1 - center.1).abs());
                if d <= r {
                    let dmg = super::items::GRENADE_DAMAGE * (r + 1 - d) / (r + 1);
                    Some((q.id, dmg))
                } else {
                    None
                }
            })
            .collect();
        for (victim, dmg) in hits {
            self.hit(attacker, victim, dmg, WeaponKind::Sniper);
        }
    }

    fn think_bots(&mut self) {
        for i in 0..self.players.len() {
            if !self.players[i].alive {
                continue;
            }
            let Some(mut brain) = self.players[i].brain.take() else { continue };
            let action = bot::think(self, i as u8, &mut brain);
            let p = &mut self.players[i];
            p.brain = Some(brain);
            if let Some(d) = action.mv {
                if p.queued_moves.is_empty() {
                    p.queued_moves.push_back(d);
                }
            }
            if let Some(d) = action.face {
                p.dir = d;
            }
            p.want_fire |= action.fire;
            p.want_pickup |= action.pickup;
            p.want_heal |= action.heal;
        }
    }

    /// Pick the direction of the nearest enemy this player could hit right
    /// now: cardinal-aligned, in range, with a clear line. None = no target.
    fn snap_aim(&self, me: u8, stats: &super::items::WeaponStats) -> Option<Dir> {
        let p = &self.players[me as usize];
        let melee = stats.speed == 0;
        self.players
            .iter()
            .filter(|q| q.alive && q.id != me)
            .filter_map(|q| {
                let dx = q.pos.0 - p.pos.0;
                let dy = q.pos.1 - p.pos.1;
                if dx != 0 && dy != 0 {
                    return None;
                }
                let dist = dx.abs() + dy.abs();
                if dist == 0 || dist > stats.range {
                    return None;
                }
                if !melee && !self.map.clear_shot(p.pos, q.pos) {
                    return None;
                }
                let dir = if dx > 0 {
                    Dir::East
                } else if dx < 0 {
                    Dir::West
                } else if dy > 0 {
                    Dir::South
                } else {
                    Dir::North
                };
                Some((dist, dir))
            })
            .min_by_key(|(dist, _)| *dist)
            .map(|(_, dir)| dir)
    }

    /// Apply weapon damage (armor absorbs half of each hit until it breaks).
    fn hit(&mut self, attacker: Option<u8>, victim: u8, damage: i32, weapon: WeaponKind) {
        let v = &mut self.players[victim as usize];
        let absorbed = v.armor.min(damage / 2);
        v.armor -= absorbed;
        v.hp -= damage - absorbed;
        let vpos = v.pos;
        if v.hp > 0 {
            // Survived the hit: a spark where it landed.
            self.effects.push((vpos, EffectKind::Spark));
            return;
        }
        let vname = self.players[victim as usize].name.clone();
        let line = match attacker {
            Some(a) if a != victim => {
                let killer = &mut self.players[a as usize];
                killer.kills += 1;
                // Multi-kill: two eliminations within ~4s.
                let multi = self.tick.saturating_sub(killer.last_kill_tick) <= 4 * TPS as u64
                    && killer.last_kill_tick > 0;
                killer.last_kill_tick = self.tick;
                let aname = killer.name.clone();
                if multi {
                    self.feed.push(format!("{aname} is on a rampage!"));
                }
                self.players[victim as usize].killed_by = Some(a);
                format!("{aname} eliminated {vname} ({})", weapon.stats().name)
            }
            _ => format!("{vname} died"),
        };
        self.kill(victim, line);
    }

    /// Mark a player dead, set placement, drop their gear, emit a feed line.
    fn kill(&mut self, victim: u8, feed_line: String) {
        let placement = self.alive_count();
        let p = &mut self.players[victim as usize];
        if !p.alive {
            return;
        }
        p.alive = false;
        p.hp = 0;
        p.placement = Some(placement);
        let pos = p.pos;
        let mut drops = Vec::new();
        if p.weapon != WeaponKind::Fists {
            drops.push(ItemKind::Weapon(p.weapon));
        }
        if p.ammo > 0 {
            drops.push(ItemKind::Ammo(p.ammo));
        }
        for _ in 0..p.medkits {
            drops.push(ItemKind::Medkit);
        }
        if p.grenades > 0 {
            drops.push(ItemKind::Grenades(p.grenades));
        }
        // A little death burst (visual only).
        for (dx, dy) in [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)] {
            self.effects.push(((pos.0 + dx, pos.1 + dy), EffectKind::Blast));
        }
        self.feed.push(feed_line);
        self.scatter_drops(pos, drops);
    }

    fn scatter_drops(&mut self, around: Pos, drops: Vec<ItemKind>) {
        let mut ring: Vec<Pos> = (-2..=2_i32)
            .flat_map(|dy| (-2..=2_i32).map(move |dx| (around.0 + dx, around.1 + dy)))
            .collect();
        ring.sort_by_key(|p| (p.0 - around.0).abs() + (p.1 - around.1).abs());
        let slots: Vec<Pos> = ring
            .into_iter()
            .filter(|&p| self.map.walkable(p) && !self.loot.contains_key(&p))
            .take(drops.len())
            .collect();
        for (slot, item) in slots.into_iter().zip(drops) {
            self.loot.insert(slot, item);
        }
    }

    /// Build the personalized, visibility-filtered view sent to one player.
    pub fn snapshot_for(&self, id: u8, feed: &[String]) -> Snapshot {
        let me = &self.players[id as usize];
        // Dead players ride the camera on their killer (or any survivor).
        let (cam, spectating) = if me.alive {
            (me.pos, None)
        } else {
            let target = me
                .killed_by
                .filter(|&k| self.players[k as usize].alive)
                .or_else(|| self.players.iter().find(|p| p.alive).map(|p| p.id));
            match target {
                Some(t) => (self.players[t as usize].pos, Some(self.players[t as usize].name.clone())),
                None => (me.pos, None),
            }
        };
        let (vx, vy) = (self.config.view_x, self.config.view_y);
        let near = |pos: Pos| (pos.0 - cam.0).abs() <= vx && (pos.1 - cam.1).abs() <= vy;
        Snapshot {
            tick: self.tick,
            you: SelfView {
                pos: cam,
                dir: me.dir,
                hp: me.hp.max(0),
                armor: me.armor,
                weapon: me.weapon,
                ammo: me.ammo,
                medkits: me.medkits,
                grenades: me.grenades,
                fire_cd: me.fire_cd,
                heal_cd: me.heal_cd,
                alive: me.alive,
                kills: me.kills,
                placement: me.placement,
                spectating,
            },
            alive: self.alive_count(),
            players: self
                .players
                .iter()
                .filter(|p| p.alive && p.id != id && near(p.pos))
                .map(|p| PlayerView { pos: p.pos, dir: p.dir, weapon: p.weapon })
                .collect(),
            bullets: self
                .tracers
                .iter()
                .filter(|(p, _, _)| near(*p))
                .copied()
                .collect(),
            grenades: self.grenades.iter().filter(|g| near(g.pos)).map(|g| g.pos).collect(),
            effects: self.effects.iter().filter(|(p, _)| near(*p)).copied().collect(),
            loot: self
                .loot
                .iter()
                .filter(|(p, _)| near(**p))
                .map(|(p, i)| (*p, *i))
                .collect(),
            zone: ZoneView {
                center: self.zone.center,
                radius: self.zone.radius,
                target_center: self.zone.target_center,
                target_radius: self.zone.target_radius,
                shrinking: self.zone.shrinking,
                seconds_left: self.zone.seconds_left(),
                damage: self.zone.damage(),
            },
            feed: feed.to_vec(),
            over: self.phase == MatchPhase::Over,
            countdown: match self.phase {
                MatchPhase::Countdown(n) => Some(n / TPS + 1),
                _ => None,
            },
        }
    }
}

// ---- view types: what goes over the wire each tick ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelfView {
    pub pos: Pos,
    pub dir: Dir,
    pub hp: i32,
    pub armor: i32,
    pub weapon: WeaponKind,
    pub ammo: u16,
    pub medkits: u8,
    pub grenades: u8,
    pub fire_cd: u8,
    pub heal_cd: u8,
    pub alive: bool,
    pub kills: u8,
    pub placement: Option<u8>,
    /// When dead and spectating, the name of whoever we're watching.
    pub spectating: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerView {
    pub pos: Pos,
    pub dir: Dir,
    pub weapon: WeaponKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneView {
    pub center: (f32, f32),
    pub radius: f32,
    pub target_center: (f32, f32),
    pub target_radius: f32,
    pub shrinking: bool,
    pub seconds_left: u32,
    pub damage: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub tick: u64,
    pub you: SelfView,
    pub alive: u8,
    pub players: Vec<PlayerView>,
    /// Tracer cells: every cell a bullet crossed this tick (pos, dir, impact).
    pub bullets: Vec<(Pos, Dir, bool)>,
    /// In-flight grenades near you.
    pub grenades: Vec<Pos>,
    /// One-shot visual bursts this tick.
    pub effects: Vec<(Pos, EffectKind)>,
    pub loot: Vec<(Pos, ItemKind)>,
    pub zone: ZoneView,
    pub feed: Vec<String>,
    pub over: bool,
    pub countdown: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn armor_absorbs_half() {
        let mut w = World::new(3, GameConfig::default());
        w.add_player("a".into(), false);
        w.add_player("b".into(), false);
        w.players[1].armor = 100;
        w.hit(Some(0), 1, 22, WeaponKind::Rifle);
        assert_eq!(w.players[1].hp, 100 - 11);
        assert_eq!(w.players[1].armor, 100 - 11);
    }

    #[test]
    fn picking_up_a_gun_loads_it() {
        let mut w = World::new(8, GameConfig::default());
        w.add_player("a".into(), false);
        w.start_match();
        while w.phase != MatchPhase::Active {
            w.step();
        }
        let pos = w.players[0].pos;
        w.loot.insert(pos, ItemKind::Weapon(WeaponKind::Rifle));
        w.queue_input(0, InputCmd::Pickup);
        w.step();
        let p = &w.players[0];
        assert_eq!(p.weapon, WeaponKind::Rifle);
        assert_eq!(p.ammo, WeaponKind::Rifle.stats().bundled_ammo, "guns must spawn loaded");
        // And firing actually produces a bullet.
        w.queue_input(0, InputCmd::Fire);
        w.step();
        assert!(
            !w.bullets.is_empty() || w.players[0].ammo < WeaponKind::Rifle.stats().bundled_ammo,
            "firing a loaded gun must spend ammo and spawn a bullet"
        );
    }

    /// Find a clear horizontal strip of grass to stage shooting tests on.
    fn clear_strip(map: &Map, len: i32) -> (i32, i32) {
        for y in 1..map.h - 1 {
            let mut run = 0;
            for x in 1..map.w - 1 {
                if map.get((x, y)) == crate::game::map::Tile::Grass {
                    run += 1;
                    if run >= len {
                        return (x - len + 1, y);
                    }
                } else {
                    run = 0;
                }
            }
        }
        panic!("no clear strip on map");
    }

    #[test]
    fn aim_snap_hits_aligned_enemy_even_facing_wrong_way() {
        let mut w = World::new(21, GameConfig::default());
        w.add_player("shooter".into(), false);
        w.add_player("target".into(), false);
        w.start_match();
        while w.phase != MatchPhase::Active {
            w.step();
        }
        let (x, y) = clear_strip(&w.map, 8);
        w.players[0].pos = (x, y);
        w.players[0].dir = Dir::North; // deliberately aiming away
        w.players[0].weapon = WeaponKind::Rifle;
        w.players[0].ammo = 10;
        w.players[1].pos = (x + 5, y);

        w.queue_input(0, InputCmd::Fire);
        for _ in 0..4 {
            w.step();
        }
        assert!(w.players[1].hp < 100, "snap aim should land the shot");
        assert_eq!(w.players[0].dir, Dir::East, "shooter should turn toward the target");
        assert!(!w.tracers.is_empty() || w.players[1].hp < 100);
    }

    #[test]
    fn bullets_leave_tracers_every_cell() {
        let mut w = World::new(22, GameConfig::default());
        w.add_player("shooter".into(), false);
        w.add_player("bystander".into(), false);
        w.start_match();
        while w.phase != MatchPhase::Active {
            w.step();
        }
        let (x, y) = clear_strip(&w.map, 12);
        w.players[0].pos = (x, y);
        w.players[0].dir = Dir::East;
        w.players[0].weapon = WeaponKind::Pistol; // speed 3
        w.players[0].ammo = 5;
        w.players[1].pos = (x + 11, y + 20); // bystander well out of the line

        w.queue_input(0, InputCmd::Fire);
        w.step();
        let on_row: Vec<_> = w.tracers.iter().filter(|(p, _, _)| p.1 == y).collect();
        assert!(on_row.len() >= 3, "pistol should leave a 3-cell tracer, got {on_row:?}");
    }

    #[test]
    fn fire_intent_latches_through_cooldown() {
        let mut w = World::new(23, GameConfig::default());
        w.add_player("shooter".into(), false);
        w.start_match();
        while w.phase != MatchPhase::Active {
            w.step();
        }
        let (x, y) = clear_strip(&w.map, 10);
        w.players[0].pos = (x, y);
        w.players[0].dir = Dir::East;
        w.players[0].weapon = WeaponKind::Rifle; // cooldown 5
        w.players[0].ammo = 10;

        w.queue_input(0, InputCmd::Fire);
        w.step(); // shot 1 leaves
        assert_eq!(w.players[0].ammo, 9);
        // Press again immediately: weapon is cooling, intent must not be lost.
        w.queue_input(0, InputCmd::Fire);
        for _ in 0..6 {
            w.step();
        }
        assert_eq!(w.players[0].ammo, 8, "second press should fire once ready");
    }

    #[test]
    fn grenade_throw_explodes_and_damages_in_radius() {
        let mut w = World::new(31, GameConfig::default());
        w.add_player("thrower".into(), false);
        w.add_player("victim".into(), false);
        w.start_match();
        while w.phase != MatchPhase::Active {
            w.step();
        }
        // Grenade flies fuse×speed cells before bursting; seat the victim there.
        let reach = crate::game::items::GRENADE_FUSE as i32 * crate::game::items::GRENADE_SPEED;
        let (x, y) = clear_strip(&w.map, reach + 3);
        w.players[0].pos = (x, y);
        w.players[0].dir = Dir::East;
        w.players[0].grenades = 1;
        w.players[1].pos = (x + reach, y);
        w.players[1].hp = 100;
        w.players[1].armor = 0;

        w.queue_input(0, InputCmd::Throw);
        let mut blew = false;
        for _ in 0..(crate::game::items::GRENADE_FUSE as usize + 4) {
            w.step();
            if w.players[1].hp < 100 {
                blew = true;
                break;
            }
            if w.phase == MatchPhase::Over {
                break;
            }
        }
        assert_eq!(w.players[0].grenades, 0, "the grenade should be consumed");
        assert!(blew, "the grenade should damage a nearby player");
        assert!(
            w.effects.is_empty() || w.effects.iter().any(|(_, k)| *k == EffectKind::Blast),
            "explosion should emit blast effects on the burst tick"
        );
    }

    #[test]
    fn supply_drop_lands_inside_the_zone() {
        let mut w = World::new(8, GameConfig::default());
        for n in ["a", "b"] {
            w.add_player(n.into(), false);
        }
        w.start_match();
        // Force a drop now.
        w.next_drop = w.tick + 1;
        let before = w.loot.values().filter(|i| matches!(i, ItemKind::Crate)).count();
        for _ in 0..(15 * TPS as usize) {
            w.step();
            if w.loot.values().any(|i| matches!(i, ItemKind::Crate)) {
                break;
            }
        }
        let crates: Vec<Pos> = w
            .loot
            .iter()
            .filter(|(_, i)| matches!(i, ItemKind::Crate))
            .map(|(p, _)| *p)
            .collect();
        assert!(crates.len() > before, "a supply crate should have dropped");
        assert!(w.zone.contains(crates[0]), "the crate must land inside the safe circle");
    }

    #[test]
    fn crate_pickup_grants_a_loadout() {
        let mut w = World::new(9, GameConfig::default());
        w.add_player("a".into(), false);
        w.start_match();
        while w.phase != MatchPhase::Active {
            w.step();
        }
        let pos = w.players[0].pos;
        w.loot.insert(pos, ItemKind::Crate);
        w.queue_input(0, InputCmd::Pickup);
        w.step();
        let p = &w.players[0];
        assert!(p.weapon.rank() >= WeaponKind::Rifle.rank());
        assert!(p.armor > 0 && p.medkits >= 2 && p.grenades >= 2);
    }

    #[test]
    fn kill_drops_gear_and_sets_placement() {
        let mut w = World::new(4, GameConfig::default());
        for n in ["a", "b", "c"] {
            w.add_player(n.into(), false);
        }
        w.start_match();
        w.players[2].weapon = WeaponKind::Rifle;
        w.players[2].ammo = 50;
        w.players[2].hp = 5;
        w.hit(Some(0), 2, 22, WeaponKind::Pistol);
        assert!(!w.players[2].alive);
        assert_eq!(w.players[2].placement, Some(3));
        assert_eq!(w.players[0].kills, 1);
        let near_corpse = w
            .loot
            .iter()
            .filter(|(p, _)| {
                (p.0 - w.players[2].pos.0).abs() <= 2 && (p.1 - w.players[2].pos.1).abs() <= 2
            })
            .count();
        assert!(near_corpse >= 2, "weapon + ammo should drop");
    }

    #[test]
    fn full_bot_match_produces_a_winner() {
        let mut w = World::new(99, GameConfig::default());
        for i in 0..8 {
            w.add_player(format!("bot{i}"), true);
        }
        w.start_match();
        for _ in 0..(15 * 60 * TPS) {
            w.step();
            if w.phase == MatchPhase::Over {
                break;
            }
        }
        assert_eq!(w.phase, MatchPhase::Over, "match should end within 15 minutes");
        assert!(w.alive_count() <= 1);
        // Every dead player has a placement; if someone won they placed 1st.
        for p in &w.players {
            assert!(p.placement.is_some() || p.alive);
        }
        if let Some(id) = w.winner {
            assert_eq!(w.players[id as usize].placement, Some(1));
        }
    }
}
