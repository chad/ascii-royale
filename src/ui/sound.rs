//! Procedural 8-bit sound effects — synthesized square waves and noise,
//! no audio assets. Everything is derived client-side by diffing snapshots,
//! so the wire protocol knows nothing about sound.

use rodio::buffer::SamplesBuffer;
use rodio::stream::MixerDeviceSink;

use crate::game::items::WeaponKind;
use crate::game::state::Snapshot;

const RATE: u32 = 22_050;

pub struct Sounds {
    sink: Option<MixerDeviceSink>,
    pub muted: bool,
}

impl Default for Sounds {
    fn default() -> Self {
        Self::new()
    }
}

impl Sounds {
    /// Opens the default output device; if there is none (SSH session,
    /// CI box), sound is simply off rather than an error.
    pub fn new() -> Self {
        let sink = rodio::stream::DeviceSinkBuilder::open_default_sink().ok().map(|mut s| {
            s.log_on_drop(false);
            s
        });
        Sounds { sink, muted: false }
    }

    /// No device, no output — for tests and headless use.
    #[cfg(test)]
    pub fn disabled() -> Self {
        Sounds { sink: None, muted: true }
    }

    pub fn available(&self) -> bool {
        self.sink.is_some()
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
    }

    fn play(&self, samples: Vec<f32>) {
        if self.muted {
            return;
        }
        if let Some(sink) = &self.sink {
            let mono = std::num::NonZero::<u16>::MIN;
            let rate = std::num::NonZero::new(RATE).expect("RATE is nonzero");
            sink.mixer().add(SamplesBuffer::new(mono, rate, samples));
        }
    }

    /// Compare two consecutive snapshots and play whatever changed.
    pub fn on_snapshot(&self, prev: Option<&Snapshot>, snap: &Snapshot) {
        let Some(prev) = prev else {
            return;
        };
        let (old, you) = (&prev.you, &snap.you);

        // Countdown blips and the starting gun.
        if snap.countdown != prev.countdown {
            match snap.countdown {
                Some(_) => self.play(square(880.0, 60, 0.25)),
                None => self.play(chirp(&[(660.0, 70), (880.0, 70), (1320.0, 140)], 0.3)),
            }
            return;
        }

        if you.alive && old.alive {
            // Own shot: cooldown jumped up.
            if you.fire_cd > old.fire_cd {
                self.play(shot(you.weapon));
            }
            // Took damage.
            if you.hp < old.hp {
                self.play(hurt());
            }
            // Healed.
            if you.hp > old.hp {
                self.play(chirp(&[(392.0, 50), (523.0, 50), (659.0, 90)], 0.25));
            }
            // Picked something up (gear improved without firing).
            if you.weapon != old.weapon
                || you.medkits > old.medkits
                || you.armor > old.armor
                || (you.ammo > old.ammo && you.weapon == old.weapon)
            {
                self.play(chirp(&[(523.0, 40), (784.0, 70)], 0.25));
            }
            // Scored a kill.
            if you.kills > old.kills {
                self.play(chirp(&[(784.0, 60), (988.0, 60), (1175.0, 120)], 0.3));
            }
            // Threw a grenade.
            if you.grenades < old.grenades {
                self.play(sweep(520.0, 900.0, 120, 0.18));
            }
        }

        // Explosions near you (grenade bursts / deaths): a low noisy boom,
        // louder the closer the nearest blast lands.
        if you.alive {
            let nearest = snap
                .effects
                .iter()
                .filter(|(_, k)| matches!(k, crate::game::state::EffectKind::Blast))
                .map(|(p, _)| (p.0 - you.pos.0).abs().max((p.1 - you.pos.1).abs()))
                .min();
            if let Some(d) = nearest {
                if d <= 18 {
                    let vol = 0.35 * (1.0 - d as f32 / 22.0);
                    self.play(mix(noise(180, vol), sweep(260.0, 70.0, 180, vol * 0.7)));
                }
            }
        }

        // You died.
        if old.alive && !you.alive {
            self.play(sweep(600.0, 70.0, 450, 0.35));
        }

        // Victory.
        if snap.over && !prev.over && you.alive {
            self.play(chirp(
                &[(523.0, 90), (659.0, 90), (784.0, 90), (1046.0, 260)],
                0.3,
            ));
        }

        // The storm starts moving.
        if snap.zone.shrinking && !prev.zone.shrinking {
            self.play(chirp(&[(440.0, 120), (554.0, 120), (440.0, 120), (554.0, 120)], 0.25));
        }

        // Bullet impacts nearby (yours and theirs): a quiet tick, louder
        // and lower the closer they land.
        if you.alive {
            let closest = snap
                .bullets
                .iter()
                .filter(|(_, _, impact)| *impact)
                .map(|(p, _, _)| (p.0 - you.pos.0).abs().max((p.1 - you.pos.1).abs()))
                .min();
            if let Some(d) = closest {
                if d <= 20 {
                    let vol = 0.16 * (1.0 - d as f32 / 24.0);
                    self.play(noise(25, vol));
                }
            }
        }
    }
}

fn shot(weapon: WeaponKind) -> Vec<f32> {
    match weapon {
        WeaponKind::Fists => noise(25, 0.12),
        WeaponKind::Pistol => sweep(880.0, 440.0, 45, 0.22),
        WeaponKind::Smg => sweep(700.0, 500.0, 25, 0.18),
        WeaponKind::Shotgun => mix(noise(110, 0.3), sweep(300.0, 90.0, 110, 0.2)),
        WeaponKind::Rifle => sweep(560.0, 320.0, 60, 0.25),
        WeaponKind::Sniper => mix(sweep(320.0, 130.0, 140, 0.3), noise(60, 0.15)),
    }
}

fn hurt() -> Vec<f32> {
    mix(noise(70, 0.3), square(110.0, 70, 0.25))
}

// ---- tiny synthesizer ----

fn ms(n: u32) -> usize {
    (RATE as usize * n as usize) / 1000
}

/// Square wave with an exponential decay envelope.
fn square(freq: f32, dur_ms: u32, vol: f32) -> Vec<f32> {
    let n = ms(dur_ms);
    (0..n)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let phase = (t * freq).fract();
            let s = if phase < 0.5 { 1.0 } else { -1.0 };
            s * vol * env(i, n)
        })
        .collect()
}

/// Square wave gliding from one pitch to another.
fn sweep(f0: f32, f1: f32, dur_ms: u32, vol: f32) -> Vec<f32> {
    let n = ms(dur_ms);
    let mut phase = 0.0f32;
    (0..n)
        .map(|i| {
            let k = i as f32 / n as f32;
            let f = f0 + (f1 - f0) * k;
            phase = (phase + f / RATE as f32).fract();
            let s = if phase < 0.5 { 1.0 } else { -1.0 };
            s * vol * env(i, n)
        })
        .collect()
}

/// White-ish noise burst (xorshift), for shots and impacts.
fn noise(dur_ms: u32, vol: f32) -> Vec<f32> {
    let n = ms(dur_ms);
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    (0..n)
        .map(|i| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let s = (state as f32 / u64::MAX as f32) * 2.0 - 1.0;
            s * vol * env(i, n)
        })
        .collect()
}

/// A little melody: sequence of (freq, ms) notes.
fn chirp(notes: &[(f32, u32)], vol: f32) -> Vec<f32> {
    let mut out = Vec::new();
    for &(f, d) in notes {
        out.extend(square(f, d, vol));
    }
    out
}

fn mix(a: Vec<f32>, b: Vec<f32>) -> Vec<f32> {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    let mut out = long;
    for (o, s) in out.iter_mut().zip(short) {
        *o = (*o + s).clamp(-1.0, 1.0);
    }
    out
}

/// Percussive decay envelope with a tiny attack to avoid clicks.
fn env(i: usize, n: usize) -> f32 {
    let attack = (n / 50).max(8);
    let a = if i < attack { i as f32 / attack as f32 } else { 1.0 };
    let k = i as f32 / n as f32;
    a * (1.0 - k).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_output_is_bounded_and_nonempty() {
        for samples in [
            square(440.0, 50, 0.3),
            sweep(880.0, 110.0, 80, 0.3),
            noise(40, 0.3),
            chirp(&[(523.0, 40), (784.0, 60)], 0.3),
            shot(WeaponKind::Shotgun),
            hurt(),
        ] {
            assert!(!samples.is_empty());
            assert!(samples.iter().all(|s| s.abs() <= 1.0), "samples must not clip");
        }
    }

    #[test]
    fn mix_keeps_longer_tail() {
        let m = mix(square(440.0, 100, 0.2), noise(30, 0.2));
        assert_eq!(m.len(), ms(100));
    }

    /// Not an assertion — plays every effect through the speakers.
    /// cargo test --lib audible_demo -- --ignored --nocapture
    #[test]
    #[ignore = "audible: plays each sound effect on the default output device"]
    fn audible_demo() {
        let s = Sounds::new();
        assert!(s.available(), "no audio output device found");
        let pause = std::time::Duration::from_millis(450);
        let all: Vec<(&str, Vec<f32>)> = vec![
            ("pistol", shot(WeaponKind::Pistol)),
            ("smg", shot(WeaponKind::Smg)),
            ("shotgun", shot(WeaponKind::Shotgun)),
            ("rifle", shot(WeaponKind::Rifle)),
            ("sniper", shot(WeaponKind::Sniper)),
            ("hurt", hurt()),
            ("pickup", chirp(&[(523.0, 40), (784.0, 70)], 0.25)),
            ("heal", chirp(&[(392.0, 50), (523.0, 50), (659.0, 90)], 0.25)),
            ("kill", chirp(&[(784.0, 60), (988.0, 60), (1175.0, 120)], 0.3)),
            ("storm", chirp(&[(440.0, 120), (554.0, 120), (440.0, 120), (554.0, 120)], 0.25)),
            ("death", sweep(600.0, 70.0, 450, 0.35)),
            ("victory", chirp(&[(523.0, 90), (659.0, 90), (784.0, 90), (1046.0, 260)], 0.3)),
        ];
        for (name, samples) in all {
            println!("  {name}");
            s.play(samples);
            std::thread::sleep(pause);
        }
        std::thread::sleep(std::time::Duration::from_millis(600));
    }
}
