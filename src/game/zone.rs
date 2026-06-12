use rand::{rngs::StdRng, Rng};
use serde::{Deserialize, Serialize};

use super::{Pos, TPS};

/// (hold seconds, shrink seconds, next-radius multiplier, damage per storm pulse)
const PHASES: &[(u32, u32, f32, i32)] = &[
    (25, 20, 0.62, 1),
    (20, 15, 0.58, 2),
    (15, 12, 0.55, 3),
    (12, 10, 0.50, 5),
    (10, 8, 0.45, 7),
    (8, 6, 0.40, 10),
    (8, 6, 0.0, 13),
];

/// Storm damage is applied every this many ticks to players outside the circle.
pub const STORM_PULSE_TICKS: u64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Zone {
    pub center: (f32, f32),
    pub radius: f32,
    pub target_center: (f32, f32),
    pub target_radius: f32,
    pub phase: usize,
    pub shrinking: bool,
    pub ticks_left: u32,
    start_center: (f32, f32),
    start_radius: f32,
    shrink_ticks: u32,
}

impl Zone {
    pub fn new(w: i32, h: i32, rng: &mut StdRng) -> Self {
        let center = (w as f32 / 2.0, h as f32 / 2.0);
        let radius = ((w * w + h * h) as f32).sqrt() / 2.0 + 2.0;
        let mut z = Zone {
            center,
            radius,
            target_center: center,
            target_radius: radius,
            phase: 0,
            shrinking: false,
            ticks_left: PHASES[0].0 * TPS,
            start_center: center,
            start_radius: radius,
            shrink_ticks: 1,
        };
        z.pick_target(rng);
        z
    }

    fn pick_target(&mut self, rng: &mut StdRng) {
        let (_, _, mult, _) = PHASES[self.phase.min(PHASES.len() - 1)];
        self.target_radius = self.radius * mult;
        // New circle must fit inside the current one.
        let max_off = (self.radius - self.target_radius).max(0.0);
        let ang = rng.random_range(0.0..std::f32::consts::TAU);
        let off = rng.random_range(0.0..max_off * 0.9);
        self.target_center =
            (self.center.0 + ang.cos() * off, self.center.1 + ang.sin() * off);
    }

    /// Advance one tick. Returns true when a new phase begins (for the feed).
    pub fn step(&mut self, rng: &mut StdRng) -> bool {
        if self.phase >= PHASES.len() {
            return false;
        }
        if self.ticks_left > 0 {
            self.ticks_left -= 1;
            if self.shrinking {
                let t = 1.0 - self.ticks_left as f32 / self.shrink_ticks as f32;
                self.radius = self.start_radius + (self.target_radius - self.start_radius) * t;
                self.center.0 =
                    self.start_center.0 + (self.target_center.0 - self.start_center.0) * t;
                self.center.1 =
                    self.start_center.1 + (self.target_center.1 - self.start_center.1) * t;
            }
            return false;
        }
        if !self.shrinking {
            // Hold elapsed: start closing.
            self.shrinking = true;
            self.shrink_ticks = PHASES[self.phase].1 * TPS;
            self.ticks_left = self.shrink_ticks;
            self.start_center = self.center;
            self.start_radius = self.radius;
            true
        } else {
            // Shrink finished: next phase holds.
            self.center = self.target_center;
            self.radius = self.target_radius;
            self.shrinking = false;
            self.phase += 1;
            if self.phase < PHASES.len() {
                self.ticks_left = PHASES[self.phase].0 * TPS;
                self.pick_target(rng);
            }
            false
        }
    }

    pub fn contains(&self, (x, y): Pos) -> bool {
        let dx = x as f32 + 0.5 - self.center.0;
        let dy = y as f32 + 0.5 - self.center.1;
        dx * dx + dy * dy <= self.radius * self.radius
    }

    pub fn damage(&self) -> i32 {
        PHASES[self.phase.min(PHASES.len() - 1)].3
    }

    pub fn seconds_left(&self) -> u32 {
        self.ticks_left / TPS
    }

    /// Exercised by tests; the sim itself just keeps applying final-phase damage.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn done(&self) -> bool {
        self.phase >= PHASES.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn zone_converges_to_nothing() {
        let mut rng = StdRng::seed_from_u64(1);
        let mut z = Zone::new(160, 100, &mut rng);
        let initial = z.radius;
        for _ in 0..(60 * 60 * TPS) {
            z.step(&mut rng);
            if z.done() {
                break;
            }
        }
        assert!(z.done(), "zone should run out of phases");
        assert!(z.radius < initial * 0.01, "final radius ~0, got {}", z.radius);
    }
}
