pub mod bot;
pub mod items;
pub mod map;
pub mod state;
pub mod zone;

use serde::{Deserialize, Serialize};

pub type Pos = (i32, i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dir {
    North,
    East,
    South,
    West,
}

impl Dir {
    pub const ALL: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];

    pub fn delta(self) -> (i32, i32) {
        match self {
            Dir::North => (0, -1),
            Dir::East => (1, 0),
            Dir::South => (0, 1),
            Dir::West => (-1, 0),
        }
    }

    pub fn step(self, (x, y): Pos) -> Pos {
        let (dx, dy) = self.delta();
        (x + dx, y + dy)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameConfig {
    pub tick_ms: u64,
    pub max_players: u8,
    /// Half-extents of the area included in snapshots (fog of war).
    pub view_x: i32,
    pub view_y: i32,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self { tick_ms: 100, max_players: 16, view_x: 48, view_y: 22 }
    }
}

/// Ticks per second, used to convert seconds to ticks in tuning tables.
pub const TPS: u32 = 10;
