use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::{Deserialize, Serialize};

use super::Pos;

pub const MAP_W: i32 = 160;
pub const MAP_H: i32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tile {
    Grass,
    Tree,
    Water,
    Wall,
    Floor,
    Road,
}

impl Tile {
    pub fn walkable(self) -> bool {
        matches!(self, Tile::Grass | Tile::Floor | Tile::Road)
    }

    pub fn blocks_shot(self) -> bool {
        matches!(self, Tile::Wall | Tile::Tree)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Map {
    pub w: i32,
    pub h: i32,
    pub tiles: Vec<Tile>,
    /// Building centers — points of interest for bots and loot.
    pub pois: Vec<Pos>,
}

impl Map {
    pub fn in_bounds(&self, (x, y): Pos) -> bool {
        x >= 0 && y >= 0 && x < self.w && y < self.h
    }

    pub fn get(&self, pos: Pos) -> Tile {
        if !self.in_bounds(pos) {
            return Tile::Wall;
        }
        self.tiles[(pos.1 * self.w + pos.0) as usize]
    }

    fn set(&mut self, pos: Pos, t: Tile) {
        if self.in_bounds(pos) {
            self.tiles[(pos.1 * self.w + pos.0) as usize] = t;
        }
    }

    pub fn walkable(&self, pos: Pos) -> bool {
        self.get(pos).walkable()
    }

    /// True when a straight shot between two cells (sharing a row or column)
    /// is not blocked by walls or trees. Endpoints excluded.
    pub fn clear_shot(&self, a: Pos, b: Pos) -> bool {
        if a.0 != b.0 && a.1 != b.1 {
            return false;
        }
        let dx = (b.0 - a.0).signum();
        let dy = (b.1 - a.1).signum();
        let mut p = (a.0 + dx, a.1 + dy);
        while p != b {
            if self.get(p).blocks_shot() {
                return false;
            }
            p = (p.0 + dx, p.1 + dy);
        }
        true
    }

    pub fn random_walkable(&self, rng: &mut StdRng) -> Pos {
        loop {
            let p = (rng.random_range(1..self.w - 1), rng.random_range(1..self.h - 1));
            if self.walkable(p) {
                return p;
            }
        }
    }
}

pub fn generate(seed: u64) -> Map {
    let mut rng = StdRng::seed_from_u64(seed);
    let mut map = Map {
        w: MAP_W,
        h: MAP_H,
        tiles: vec![Tile::Grass; (MAP_W * MAP_H) as usize],
        pois: Vec::new(),
    };

    // Lakes: random-walk blobs of water.
    for _ in 0..4 {
        let mut p = (rng.random_range(15..MAP_W - 15), rng.random_range(10..MAP_H - 10));
        for _ in 0..160 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    map.set((p.0 + dx, p.1 + dy), Tile::Water);
                }
            }
            p.0 += rng.random_range(-2..=2);
            p.1 += rng.random_range(-2..=2);
        }
    }

    // Roads: two vertical, two horizontal, drawn over everything (bridges).
    for _ in 0..2 {
        let x = rng.random_range(20..MAP_W - 20);
        for y in 0..MAP_H {
            map.set((x, y), Tile::Road);
            map.set((x + 1, y), Tile::Road);
        }
        let y = rng.random_range(15..MAP_H - 15);
        for x in 0..MAP_W {
            map.set((x, y), Tile::Road);
            map.set((x, y + 1), Tile::Road);
        }
    }

    // Forests: clusters of trees on grass.
    for _ in 0..16 {
        let c = (rng.random_range(5..MAP_W - 5), rng.random_range(5..MAP_H - 5));
        for _ in 0..45 {
            let p = (c.0 + rng.random_range(-7..=7), c.1 + rng.random_range(-5..=5));
            if map.get(p) == Tile::Grass {
                map.set(p, Tile::Tree);
            }
        }
    }

    // Buildings: rectangles of wall + floor with doors, on clear-ish ground.
    let mut placed: Vec<(i32, i32, i32, i32)> = Vec::new();
    'attempts: for _ in 0..120 {
        if placed.len() >= 16 {
            break;
        }
        let bw = rng.random_range(7..=14);
        let bh = rng.random_range(5..=9);
        let x0 = rng.random_range(2..MAP_W - bw - 2);
        let y0 = rng.random_range(2..MAP_H - bh - 2);
        // Keep buildings off water/roads and apart from each other.
        for &(px, py, pw, ph) in &placed {
            if x0 < px + pw + 3 && px < x0 + bw + 3 && y0 < py + ph + 3 && py < y0 + bh + 3 {
                continue 'attempts;
            }
        }
        for y in y0..y0 + bh {
            for x in x0..x0 + bw {
                if matches!(map.get((x, y)), Tile::Water | Tile::Road) {
                    continue 'attempts;
                }
            }
        }
        for y in y0..y0 + bh {
            for x in x0..x0 + bw {
                let edge = x == x0 || y == y0 || x == x0 + bw - 1 || y == y0 + bh - 1;
                map.set((x, y), if edge { Tile::Wall } else { Tile::Floor });
            }
        }
        // Two doors on opposite-ish sides (never corners).
        let door_x = rng.random_range(x0 + 1..x0 + bw - 1);
        map.set((door_x, y0), Tile::Floor);
        let door_x = rng.random_range(x0 + 1..x0 + bw - 1);
        map.set((door_x, y0 + bh - 1), Tile::Floor);
        placed.push((x0, y0, bw, bh));
        map.pois.push((x0 + bw / 2, y0 + bh / 2));
    }

    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_walkable_map_with_buildings() {
        let map = generate(42);
        assert_eq!(map.tiles.len(), (MAP_W * MAP_H) as usize);
        assert!(map.pois.len() >= 8, "want plenty of buildings, got {}", map.pois.len());
        let walkable = map.tiles.iter().filter(|t| t.walkable()).count();
        assert!(walkable > map.tiles.len() / 2, "most of the map should be walkable");
    }

    #[test]
    fn clear_shot_blocked_by_walls() {
        let map = generate(7);
        let (px, py) = map.pois[0];
        // A point inside a building cannot be hit from far outside through the wall.
        let mut x = px;
        while map.get((x, py)) != Tile::Wall && x > 0 {
            x -= 1;
        }
        assert!(!map.clear_shot((x - 2, py), (px, py)));
    }
}
