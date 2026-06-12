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
    pub alive: bool,
    pub kills: u8,
    pub placement: Option<u8>,
    pub brain: Option<BotBrain>,
    // Input intents, consumed each tick.
    pub queued_moves: VecDeque<Dir>,
    pub want_fire: bool,
    pub want_pickup: bool,
    pub want_heal: bool,
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
            fire_cd: 0,
            heal_cd: 0,
            alive: true,
            kills: 0,
            placement: None,
            brain: if is_bot { Some(BotBrain::default()) } else { None },
            queued_moves: VecDeque::new(),
            want_fire: false,
            want_pickup: false,
            want_heal: false,
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

pub struct World {
    pub tick: u64,
    pub map: Map,
    pub zone: Zone,
    pub players: Vec<Player>,
    pub bullets: Vec<Bullet>,
    pub loot: HashMap<Pos, ItemKind>,
    pub phase: MatchPhase,
    pub config: GameConfig,
    /// Human-readable feed lines produced this tick (kills, zone, win).
    pub feed: Vec<String>,
    pub winner: Option<u8>,
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
            loot,
            phase: MatchPhase::Lobby,
            config,
            feed: Vec::new(),
            winner: None,
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
            }
        }

        // Firing.
        for i in 0..self.players.len() {
            if !self.players[i].alive || !std::mem::take(&mut self.players[i].want_fire) {
                continue;
            }
            let (pos, dir, weapon) = {
                let p = &self.players[i];
                (p.pos, p.dir, p.weapon)
            };
            let stats = weapon.stats();
            let p = &mut self.players[i];
            if p.fire_cd > 0 || p.ammo < stats.ammo_cost {
                continue;
            }
            p.ammo -= stats.ammo_cost;
            p.fire_cd = stats.cooldown;
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

        // Bullet flight, cell by cell so nothing is skipped over.
        let mut bullets = std::mem::take(&mut self.bullets);
        bullets.retain_mut(|b| {
            for _ in 0..b.speed {
                b.pos = b.dir.step(b.pos);
                b.travel -= 1;
                if !self.map.in_bounds(b.pos) || self.map.get(b.pos).blocks_shot() {
                    return false;
                }
                if let Some(v) = self
                    .players
                    .iter()
                    .position(|q| q.alive && q.id != b.owner && q.pos == b.pos)
                {
                    self.hit(Some(b.owner), v as u8, b.damage, b.weapon);
                    return false;
                }
                if b.travel <= 0 {
                    return false;
                }
            }
            true
        });
        self.bullets = bullets;

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

    /// Apply weapon damage (armor absorbs half of each hit until it breaks).
    fn hit(&mut self, attacker: Option<u8>, victim: u8, damage: i32, weapon: WeaponKind) {
        let v = &mut self.players[victim as usize];
        let absorbed = v.armor.min(damage / 2);
        v.armor -= absorbed;
        v.hp -= damage - absorbed;
        if v.hp <= 0 {
            let vname = v.name.clone();
            let line = match attacker {
                Some(a) if a != victim => {
                    self.players[a as usize].kills += 1;
                    let aname = &self.players[a as usize].name;
                    format!("{aname} eliminated {vname} ({})", weapon.stats().name)
                }
                _ => format!("{vname} died"),
            };
            self.kill(victim, line);
        }
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
        let (vx, vy) = (self.config.view_x, self.config.view_y);
        let near = |pos: Pos| (pos.0 - me.pos.0).abs() <= vx && (pos.1 - me.pos.1).abs() <= vy;
        Snapshot {
            tick: self.tick,
            you: SelfView {
                pos: me.pos,
                dir: me.dir,
                hp: me.hp.max(0),
                armor: me.armor,
                weapon: me.weapon,
                ammo: me.ammo,
                medkits: me.medkits,
                fire_cd: me.fire_cd,
                heal_cd: me.heal_cd,
                alive: me.alive,
                kills: me.kills,
                placement: me.placement,
            },
            alive: self.alive_count(),
            players: self
                .players
                .iter()
                .filter(|p| p.alive && p.id != id && near(p.pos))
                .map(|p| PlayerView { pos: p.pos, dir: p.dir, weapon: p.weapon })
                .collect(),
            bullets: self
                .bullets
                .iter()
                .filter(|b| near(b.pos))
                .map(|b| (b.pos, b.dir))
                .collect(),
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
    pub fire_cd: u8,
    pub heal_cd: u8,
    pub alive: bool,
    pub kills: u8,
    pub placement: Option<u8>,
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
    pub bullets: Vec<(Pos, Dir)>,
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
