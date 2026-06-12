use std::collections::HashMap;

use rand::{rngs::StdRng, Rng};
use serde::{Deserialize, Serialize};

use super::map::Map;
use super::Pos;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WeaponKind {
    Fists,
    Pistol,
    Smg,
    Shotgun,
    Rifle,
    Sniper,
}

pub struct WeaponStats {
    pub name: &'static str,
    pub damage: i32,
    /// Ticks between shots.
    pub cooldown: u8,
    /// Bullet cells per tick; 0 = melee.
    pub speed: i32,
    /// Max bullet travel in cells.
    pub range: i32,
    pub ammo_cost: u16,
    /// Ammo that comes with the weapon when picked up — guns spawn loaded.
    pub bundled_ammo: u16,
}

impl WeaponKind {
    pub fn stats(self) -> WeaponStats {
        match self {
            WeaponKind::Fists => WeaponStats { name: "Fists", damage: 10, cooldown: 3, speed: 0, range: 1, ammo_cost: 0, bundled_ammo: 0 },
            WeaponKind::Pistol => WeaponStats { name: "Pistol", damage: 15, cooldown: 4, speed: 3, range: 25, ammo_cost: 1, bundled_ammo: 24 },
            WeaponKind::Smg => WeaponStats { name: "SMG", damage: 8, cooldown: 1, speed: 3, range: 20, ammo_cost: 1, bundled_ammo: 30 },
            WeaponKind::Shotgun => WeaponStats { name: "Shotgun", damage: 30, cooldown: 8, speed: 2, range: 8, ammo_cost: 2, bundled_ammo: 12 },
            WeaponKind::Rifle => WeaponStats { name: "Rifle", damage: 22, cooldown: 5, speed: 4, range: 35, ammo_cost: 1, bundled_ammo: 20 },
            WeaponKind::Sniper => WeaponStats { name: "Sniper", damage: 70, cooldown: 15, speed: 6, range: 60, ammo_cost: 2, bundled_ammo: 8 },
        }
    }

    /// Crude desirability ranking used by bots and auto-pickup decisions.
    pub fn rank(self) -> u8 {
        match self {
            WeaponKind::Fists => 0,
            WeaponKind::Pistol => 1,
            WeaponKind::Shotgun => 2,
            WeaponKind::Smg => 3,
            WeaponKind::Rifle => 4,
            WeaponKind::Sniper => 5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemKind {
    Weapon(WeaponKind),
    Ammo(u16),
    Medkit,
    Vest,
}

impl ItemKind {
    pub fn label(self) -> String {
        match self {
            ItemKind::Weapon(w) => w.stats().name.to_string(),
            ItemKind::Ammo(n) => format!("Ammo x{n}"),
            ItemKind::Medkit => "Medkit".to_string(),
            ItemKind::Vest => "Vest".to_string(),
        }
    }
}

pub const MAX_AMMO: u16 = 240;
pub const MAX_MEDKITS: u8 = 5;
pub const MAX_HP: i32 = 100;
pub const MEDKIT_HEAL: i32 = 40;
pub const VEST_ARMOR: i32 = 100;

fn roll_item(rng: &mut StdRng) -> ItemKind {
    match rng.random_range(0..100) {
        0..15 => ItemKind::Weapon(WeaponKind::Pistol),
        15..27 => ItemKind::Weapon(WeaponKind::Smg),
        27..37 => ItemKind::Weapon(WeaponKind::Shotgun),
        37..45 => ItemKind::Weapon(WeaponKind::Rifle),
        45..49 => ItemKind::Weapon(WeaponKind::Sniper),
        49..74 => ItemKind::Ammo(30),
        74..90 => ItemKind::Medkit,
        _ => ItemKind::Vest,
    }
}

/// Place loot: a few items around each building center, plus scatter in the open.
pub fn scatter_loot(map: &Map, rng: &mut StdRng) -> HashMap<Pos, ItemKind> {
    let mut loot = HashMap::new();
    for &(cx, cy) in &map.pois {
        let n = rng.random_range(2..=4);
        for _ in 0..n {
            for _attempt in 0..10 {
                let p = (cx + rng.random_range(-4..=4), cy + rng.random_range(-3..=3));
                if map.walkable(p) && !loot.contains_key(&p) {
                    loot.insert(p, roll_item(rng));
                    break;
                }
            }
        }
    }
    for _ in 0..60 {
        let p = map.random_walkable(rng);
        loot.entry(p).or_insert_with(|| roll_item(rng));
    }
    loot
}
