use super::items::{ItemKind, WeaponKind};
use super::state::World;
use super::{Dir, Pos};

/// Per-bot persistent state.
#[derive(Debug, Clone, Default)]
pub struct BotBrain {
    target: Option<Pos>,
    repath_in: u32,
}

#[derive(Debug, Default)]
pub struct BotAction {
    pub mv: Option<Dir>,
    pub face: Option<Dir>,
    pub fire: bool,
    pub pickup: bool,
    pub heal: bool,
}

/// Cheap deterministic "randomness" so bots don't need the world RNG.
fn chance(world_tick: u64, id: u8, salt: u64, pct: u64) -> bool {
    let mut x = world_tick
        .wrapping_mul(0x9E3779B97F4A7C15)
        .wrapping_add((id as u64) << 32)
        .wrapping_add(salt);
    x ^= x >> 33;
    x = x.wrapping_mul(0xFF51AFD7ED558CCD);
    x ^= x >> 33;
    x % 100 < pct
}

fn manhattan(a: Pos, b: Pos) -> i32 {
    (a.0 - b.0).abs() + (a.1 - b.1).abs()
}

/// Priorities: heal when hurt > fight a reachable enemy > escape the storm >
/// grab nearby loot > wander between buildings.
pub fn think(w: &World, me: u8, brain: &mut BotBrain) -> BotAction {
    let mut act = BotAction::default();
    let p = &w.players[me as usize];
    let tick = w.tick;

    if p.hp < 55 && p.medkits > 0 && p.heal_cd == 0 {
        act.heal = true;
    }

    // Anything worth picking up where we stand?
    if let Some(item) = w.loot.get(&p.pos) {
        if wants(p, *item) {
            act.pickup = true;
        }
    }

    // Nearest live enemy.
    let enemy = w
        .players
        .iter()
        .filter(|q| q.alive && q.id != me)
        .min_by_key(|q| manhattan(p.pos, q.pos));

    let outside = !w.zone.contains(p.pos);
    let stats = p.weapon.stats();
    let has_gun = p.weapon != WeaponKind::Fists && p.ammo >= stats.ammo_cost;

    if let Some(e) = enemy {
        let dist = manhattan(p.pos, e.pos);
        let aligned = p.pos.0 == e.pos.0 || p.pos.1 == e.pos.1;
        let in_range = dist <= stats.range;
        if aligned && in_range && w.map.clear_shot(p.pos, e.pos) {
            // Face them and shoot (imperfect trigger discipline = dodgeable).
            let d = face_toward(p.pos, e.pos);
            act.face = Some(d);
            if (has_gun || dist == 1) && chance(tick, me, 1, 70) {
                act.fire = true;
            }
            // Sidestep occasionally so duels aren't static.
            if chance(tick, me, 2, 25) {
                act.mv = Some(perpendicular(d, chance(tick, me, 3, 50)));
            }
            return finish(w, p, act, brain);
        }
        // Armed and they're close: maneuver to share a row/column.
        if has_gun && dist <= stats.range.min(18) && !outside {
            let dx = (e.pos.0 - p.pos.0).abs();
            let dy = (e.pos.1 - p.pos.1).abs();
            let d = if dx <= dy {
                if e.pos.0 > p.pos.0 { Dir::East } else { Dir::West }
            } else if e.pos.1 > p.pos.1 {
                Dir::South
            } else {
                Dir::North
            };
            act.mv = Some(d);
            return finish(w, p, act, brain);
        }
        // Unarmed and being approached: run away.
        if !has_gun && dist <= 8 {
            brain.target = Some((
                p.pos.0 + (p.pos.0 - e.pos.0).signum() * 12,
                p.pos.1 + (p.pos.1 - e.pos.1).signum() * 12,
            ));
            brain.repath_in = 20;
        }
    }

    // Storm beats everything else: head for the safe circle.
    let safe_center = (w.zone.target_center.0 as i32, w.zone.target_center.1 as i32);
    if outside
        || (w.zone.shrinking
            && manhattan(p.pos, safe_center) as f32 > w.zone.target_radius * 0.7)
    {
        brain.target = Some(safe_center);
        brain.repath_in = 30;
        return finish(w, p, act, brain);
    }

    // Loot run: nearest useful item within sight.
    if brain.target.is_none() || brain.repath_in == 0 {
        let want = w
            .loot
            .iter()
            .filter(|(pos, item)| manhattan(p.pos, **pos) <= 14 && wants(p, **item))
            .min_by_key(|(pos, _)| manhattan(p.pos, **pos));
        if let Some((pos, _)) = want {
            brain.target = Some(*pos);
            brain.repath_in = 60;
        }
    }

    // Otherwise wander between buildings inside the zone.
    if brain.target.is_none() || brain.repath_in == 0 || brain.target == Some(p.pos) {
        let n = w.map.pois.len() as u64;
        if n > 0 {
            let pick = (tick / 7 + me as u64 * 13 + (tick % 5)) % n;
            let poi = w.map.pois[pick as usize];
            if w.zone.contains(poi) {
                brain.target = Some(poi);
            } else {
                brain.target = Some(safe_center);
            }
            brain.repath_in = 200;
        }
    }

    finish(w, p, act, brain)
}

/// Shared movement tail: walk toward the brain's target.
fn finish(
    w: &World,
    p: &super::state::Player,
    mut act: BotAction,
    brain: &mut BotBrain,
) -> BotAction {
    brain.repath_in = brain.repath_in.saturating_sub(1);
    if act.mv.is_none() {
        if let Some(t) = brain.target {
            if t == p.pos {
                brain.target = None;
            } else if chance(w.tick, p.id, 4, 85) {
                act.mv = step_toward(w, p.pos, t, w.tick, p.id);
            }
        }
    }
    // Don't walk into walls/water/players: re-aim the step if blocked.
    if let Some(d) = act.mv {
        if !can_step(w, p.id, d.step(p.pos)) {
            act.mv = Dir::ALL
                .iter()
                .copied()
                .find(|alt| can_step(w, p.id, alt.step(p.pos)) && chance(w.tick, p.id, 5, 80));
        }
    }
    act
}

fn wants(p: &super::state::Player, item: ItemKind) -> bool {
    match item {
        ItemKind::Weapon(wk) => wk.rank() > p.weapon.rank(),
        ItemKind::Ammo(_) => p.ammo < 120,
        ItemKind::Medkit => p.medkits < 3,
        ItemKind::Vest => p.armor < 50,
    }
}

fn face_toward(from: Pos, to: Pos) -> Dir {
    if from.0 == to.0 {
        if to.1 > from.1 {
            Dir::South
        } else {
            Dir::North
        }
    } else if to.0 > from.0 {
        Dir::East
    } else {
        Dir::West
    }
}

fn perpendicular(d: Dir, flip: bool) -> Dir {
    match (d, flip) {
        (Dir::North | Dir::South, false) => Dir::East,
        (Dir::North | Dir::South, true) => Dir::West,
        (Dir::East | Dir::West, false) => Dir::North,
        (Dir::East | Dir::West, true) => Dir::South,
    }
}

fn can_step(w: &World, me: u8, to: Pos) -> bool {
    w.map.walkable(to) && !w.players.iter().any(|q| q.alive && q.id != me && q.pos == to)
}

/// Greedy pathing: prefer the axis with the larger gap, fall back to the other.
fn step_toward(w: &World, from: Pos, to: Pos, tick: u64, id: u8) -> Option<Dir> {
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let mut prefs = Vec::with_capacity(2);
    let horiz = if dx > 0 { Dir::East } else { Dir::West };
    let vert = if dy > 0 { Dir::South } else { Dir::North };
    if dx.abs() >= dy.abs() {
        if dx != 0 {
            prefs.push(horiz);
        }
        if dy != 0 {
            prefs.push(vert);
        }
    } else {
        if dy != 0 {
            prefs.push(vert);
        }
        if dx != 0 {
            prefs.push(horiz);
        }
    }
    // A pinch of noise to shake loose from concave obstacles.
    if chance(tick, id, 6, 12) {
        prefs.reverse();
    }
    prefs.into_iter().find(|d| can_step(w, id, d.step(from)))
}
