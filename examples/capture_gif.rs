//! Renders a deterministic bot match through the real TUI draw code and
//! encodes it as an animated GIF for the README. Fully headless.
//!
//!     cargo run --example capture_gif [seed] [out.gif]
//!
//! The match is simulated twice with the same seed: pass one finds the
//! winner, pass two records the match from the winner's point of view
//! (early drop, mid-game, and the finale, with jump cuts between).

use std::collections::VecDeque;
use std::fs::File;

use ascii_royale::game::state::{MatchPhase, Snapshot, World};
use ascii_royale::game::GameConfig;
use ascii_royale::ui::tui::GameView;
use font8x8::legacy::{BASIC_LEGACY, BOX_LEGACY, LATIN_LEGACY};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

const COLS: u16 = 100;
const ROWS: u16 = 28;
const CELL_W: usize = 8;
const CELL_H: usize = 16; // 8x8 font doubled vertically: terminal cell aspect
const BOTS: usize = 8;
/// GIF frame delay in centiseconds; one frame per 2 sim ticks (200 ms of
/// game time shown for 90 ms) ≈ 2.2x speed.
const FRAME_DELAY_CS: u16 = 9;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let seed: u64 = args.next().map(|s| s.parse()).transpose()?.unwrap_or(2026);
    let out = args.next().unwrap_or_else(|| "assets/gameplay.gif".to_string());

    // Pass 1: who wins, and how long does it take?
    let (winner, total_ticks) = {
        let mut w = sim(seed);
        while w.phase != MatchPhase::Over {
            w.step();
        }
        let Some(winner) = w.winner else {
            eprintln!("seed {seed}: stalemate (storm killed everyone) — try another seed");
            std::process::exit(1);
        };
        (winner, w.tick)
    };
    eprintln!("seed {seed}: bot {winner} wins after {total_ticks} ticks; recording...");

    // Segments of interest (tick ranges), with jump cuts between.
    let segments = [
        (5u64, 120u64),                                      // the drop + first loot
        (total_ticks / 2, total_ticks / 2 + 100),            // mid-game scrap
        (total_ticks.saturating_sub(260), total_ticks + 1),  // the finale
    ];

    // Pass 2: same seed → identical match; record the winner's POV.
    let mut world = sim(seed);
    let mut terminal = Terminal::new(TestBackend::new(COLS, ROWS))?;
    let mut feed: VecDeque<String> = VecDeque::new();
    let mut frames: Vec<Vec<u8>> = Vec::new();
    let mut palette: Vec<[u8; 3]> = Vec::new();

    while world.phase != MatchPhase::Over {
        world.step();
        let tick = world.tick;
        let lines = world.feed.clone();
        for line in &lines {
            feed.push_front(line.clone());
        }
        feed.truncate(12);
        let in_segment = segments.iter().any(|(a, b)| tick >= *a && tick < *b);
        if !in_segment || tick % 2 != 0 {
            continue;
        }
        let snap = world.snapshot_for(winner, &lines);
        frames.push(render_frame(&mut terminal, &world, &snap, &feed, &mut palette)?);
    }
    // Hold the final frame (winner banner in the feed) for a beat.
    for _ in 0..14 {
        frames.push(frames.last().expect("at least one frame").clone());
    }

    write_gif(&out, &frames, &palette)?;
    let bytes = std::fs::metadata(&out)?.len();
    eprintln!("wrote {} frames to {out} ({:.1} MiB)", frames.len(), bytes as f64 / 1048576.0);
    Ok(())
}

fn sim(seed: u64) -> World {
    let mut w = World::new(seed, GameConfig::default());
    for i in 0..BOTS {
        w.add_player(format!("bot{i}"), true);
    }
    w.start_match();
    w
}

fn render_frame(
    terminal: &mut Terminal<TestBackend>,
    world: &World,
    snap: &Snapshot,
    feed: &VecDeque<String>,
    palette: &mut Vec<[u8; 3]>,
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let view = GameView {
        map: &world.map,
        snap,
        feed,
        controls: "wasd/arrows move+aim · f fire · e pickup · h heal · M mute · q quit".into(),
    };
    terminal.draw(|f| view.draw(f))?;
    Ok(rasterize(terminal.backend().buffer(), palette))
}

/// Buffer of styled cells -> indexed pixels, growing the palette as needed.
fn rasterize(buf: &Buffer, palette: &mut Vec<[u8; 3]>) -> Vec<u8> {
    let (w, h) = (buf.area.width as usize, buf.area.height as usize);
    let (pw, ph) = (w * CELL_W, h * CELL_H);
    let mut px = vec![0u8; pw * ph];
    for cy in 0..h {
        for cx in 0..w {
            let cell = &buf[(cx as u16, cy as u16)];
            let style = cell.style();
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            let fg = color_index(rgb_of(style.fg, style.add_modifier, true), palette);
            let bg = color_index(rgb_of(style.bg, style.add_modifier, false), palette);
            let glyph = glyph_bits(ch);
            for gy in 0..CELL_H {
                let bits = glyph[gy / 2]; // double each font row vertically
                for gx in 0..CELL_W {
                    let on = bits & (1 << gx) != 0;
                    let p = (cy * CELL_H + gy) * pw + cx * CELL_W + gx;
                    px[p] = if on { fg } else { bg };
                }
            }
        }
    }
    px
}

fn glyph_bits(ch: char) -> [u8; 8] {
    let cp = ch as u32;
    match cp {
        0x20..=0x7E => BASIC_LEGACY[cp as usize],
        0xA0..=0xFF => LATIN_LEGACY[(cp - 0xA0) as usize],
        0x2500..=0x257F => BOX_LEGACY[(cp - 0x2500) as usize],
        _ => BASIC_LEGACY[b'?' as usize],
    }
}

fn color_index(rgb: [u8; 3], palette: &mut Vec<[u8; 3]>) -> u8 {
    if let Some(i) = palette.iter().position(|c| *c == rgb) {
        return i as u8;
    }
    assert!(palette.len() < 256, "palette overflow");
    palette.push(rgb);
    (palette.len() - 1) as u8
}

/// Terminal-ish color scheme (Tokyo Night flavored).
fn rgb_of(color: Option<Color>, mods: Modifier, is_fg: bool) -> [u8; 3] {
    let base = match color {
        Some(Color::Black) => [0x15, 0x16, 0x1e],
        Some(Color::Red) => [0xf7, 0x76, 0x8e],
        Some(Color::Green) => [0x9e, 0xce, 0x6a],
        Some(Color::Yellow) => [0xe0, 0xaf, 0x68],
        Some(Color::Blue) => [0x7a, 0xa2, 0xf7],
        Some(Color::Magenta) => [0xbb, 0x9a, 0xf7],
        Some(Color::Cyan) => [0x7d, 0xcf, 0xff],
        Some(Color::Gray) => [0xa9, 0xb1, 0xd6],
        Some(Color::DarkGray) => [0x56, 0x5f, 0x89],
        Some(Color::LightRed) => [0xff, 0x89, 0x9d],
        Some(Color::LightGreen) => [0xb9, 0xf2, 0x7c],
        Some(Color::LightYellow) => [0xff, 0xc7, 0x77],
        Some(Color::LightBlue) => [0x8d, 0xb2, 0xff],
        Some(Color::LightMagenta) => [0xc7, 0xa9, 0xff],
        Some(Color::LightCyan) => [0xb4, 0xf9, 0xf8],
        Some(Color::White) => [0xc0, 0xca, 0xf5],
        Some(Color::Rgb(r, g, b)) => [r, g, b],
        _ => {
            if is_fg {
                [0xc0, 0xca, 0xf5] // default fg
            } else {
                [0x1a, 0x1b, 0x26] // default bg
            }
        }
    };
    let scale = |c: [u8; 3], k: f32| c.map(|v| ((v as f32 * k).min(255.0)) as u8);
    if is_fg && mods.contains(Modifier::DIM) {
        scale(base, 0.6)
    } else if is_fg && mods.contains(Modifier::BOLD) {
        scale(base, 1.15)
    } else {
        base
    }
}

fn write_gif(
    path: &str,
    frames: &[Vec<u8>],
    palette: &[[u8; 3]],
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let (pw, ph) = (COLS as usize * CELL_W, ROWS as usize * CELL_H);
    let flat: Vec<u8> = palette.iter().flatten().copied().collect();
    let mut enc = gif::Encoder::new(File::create(path)?, pw as u16, ph as u16, &flat)?;
    enc.set_repeat(gif::Repeat::Infinite)?;
    for (i, frame) in frames.iter().enumerate() {
        let mut f = gif::Frame {
            width: pw as u16,
            height: ph as u16,
            buffer: std::borrow::Cow::Borrowed(frame),
            delay: FRAME_DELAY_CS,
            ..Default::default()
        };
        // Frame differencing: encode only the rectangle that changed.
        if i > 0 {
            if let Some(rect) = dirty_rect(&frames[i - 1], frame, pw, ph) {
                let (x0, y0, x1, y1) = rect;
                let (rw, rh) = (x1 - x0 + 1, y1 - y0 + 1);
                let mut sub = Vec::with_capacity(rw * rh);
                for y in y0..=y1 {
                    sub.extend_from_slice(&frame[y * pw + x0..y * pw + x1 + 1]);
                }
                f.left = x0 as u16;
                f.top = y0 as u16;
                f.width = rw as u16;
                f.height = rh as u16;
                f.buffer = std::borrow::Cow::Owned(sub);
            } else {
                // Nothing changed: 1px no-op frame to keep the timing.
                f.width = 1;
                f.height = 1;
                f.buffer = std::borrow::Cow::Owned(vec![frame[0]]);
            }
        }
        enc.write_frame(&f)?;
    }
    Ok(())
}

fn dirty_rect(a: &[u8], b: &[u8], w: usize, h: usize) -> Option<(usize, usize, usize, usize)> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..h {
        let row = &a[y * w..(y + 1) * w];
        let row_b = &b[y * w..(y + 1) * w];
        if row == row_b {
            continue;
        }
        let first = row.iter().zip(row_b).position(|(p, q)| p != q).unwrap();
        let last = w - 1 - row.iter().zip(row_b).rev().position(|(p, q)| p != q).unwrap();
        x0 = x0.min(first);
        x1 = x1.max(last);
        if y0 == usize::MAX {
            y0 = y;
        }
        y1 = y;
    }
    (y0 != usize::MAX).then_some((x0, y0, x1, y1))
}
