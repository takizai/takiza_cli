//! A deterministic, sparse snowfall composed into unused terminal cells.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    OnceLock,
};
use std::time::Instant;
use std::{collections::BTreeMap, time::Duration};

pub const FRAME_INTERVAL: Duration = Duration::from_millis(16);

pub fn input_poll_interval() -> Duration {
    if enabled() {
        FRAME_INTERVAL
    } else {
        Duration::from_millis(100)
    }
}

static START: OnceLock<Instant> = OnceLock::new();
static LAST_TICK: AtomicU64 = AtomicU64::new(u64::MAX);

pub fn enabled() -> bool {
    crate::theme::current() == crate::theme::Theme::EveningIrkutsk
}

fn tick() -> u64 {
    (START.get_or_init(Instant::now).elapsed().as_millis() / FRAME_INTERVAL.as_millis()) as u64
}

pub fn due() -> bool {
    enabled() && LAST_TICK.load(Ordering::Relaxed) != tick()
}

fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e3779b97f4a7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

pub struct Frame {
    width: usize,
    flakes: Vec<Vec<Flake>>,
}

#[derive(Clone)]
struct Flake {
    column: usize,
    glyph: &'static str,
    color: (u8, u8, u8),
}

impl Frame {
    pub fn new(width: usize, height: usize) -> Self {
        let mut frame = Self {
            width,
            flakes: vec![Vec::new(); height],
        };
        if !enabled() || width < 8 || height == 0 {
            return frame;
        }
        let elapsed = START.get_or_init(Instant::now).elapsed();
        let time = elapsed.as_secs_f64();
        LAST_TICK.store(
            (elapsed.as_millis() / FRAME_INTERVAL.as_millis()) as u64,
            Ordering::Relaxed,
        );
        let count = (width.saturating_mul(height) / 140).clamp(3, 160);
        // Preserve a recognizable snowflake silhouette. Fade the same glyph
        // between neighboring cells instead of replacing it with Braille dots.
        const GLYPHS: [&str; 3] = ["❄\u{fe0e}", "❅", "❆"];
        let mut cells: BTreeMap<(usize, usize), (&'static str, f64, f64)> = BTreeMap::new();
        for index in 0..count {
            let seed = mix(index as u64);
            let speed = 0.65 + (seed % 71) as f64 / 100.0;
            let phase = ((seed >> 32) % 628) as f64 / 100.0;
            let direction = if seed & 1 == 0 { 1.0 } else { -1.0 };
            // Wind moves flakes several columns per second, with broad swaying
            // arcs. Cell aspect ratio makes this visibly diagonal on a terminal.
            let wind = 1.8 + ((seed >> 8) % 101) as f64 / 100.0;
            let sway = 1.5 + ((seed >> 24) % 101) as f64 / 100.0;
            let y =
                (((seed >> 16) % height as u64) as f64 + time * speed).rem_euclid(height as f64);
            let x = ((seed % width as u64) as f64
                + (time * 0.65 + phase).sin() * sway
                + time * direction * wind)
                .rem_euclid(width as f64);
            let glyph = GLYPHS[index % GLYPHS.len()];
            for dy in 0..2 {
                for dx in 0..2 {
                    let coverage = (if dy == 0 { 1.0 - y.fract() } else { y.fract() })
                        * (if dx == 0 { 1.0 - x.fract() } else { x.fract() });
                    if coverage < 0.12 {
                        continue;
                    }
                    let sy = (y.floor() as usize + dy) % height;
                    let sx = (x.floor() as usize + dx) % width;
                    let cell = cells.entry((sy, sx)).or_insert((glyph, 0.0, 0.0));
                    if coverage > cell.2 {
                        cell.0 = glyph;
                        cell.2 = coverage;
                    }
                    cell.1 += coverage;
                }
            }
        }
        for ((row, column), (glyph, coverage, _)) in cells {
            let light = coverage.min(1.0).powf(0.85);
            frame.flakes[row].push(Flake {
                column,
                glyph,
                color: (
                    (31.0 + 115.0 * light) as u8,
                    (37.0 + 140.0 * light) as u8,
                    (44.0 + 151.0 * light) as u8,
                ),
            });
        }
        frame
    }

    pub fn line(&self, text: &str, row: usize) -> String {
        let Some(flakes) = self.flakes.get(row) else {
            return text.to_string();
        };
        // Keep whitespace within messages, logos, code and diff backgrounds intact.
        let occupied = crate::markdown::visible_width(text);
        let first_free = if occupied == 0 {
            0
        } else {
            occupied.saturating_add(3)
        };
        if text.contains("\x1b[48;") || first_free >= self.width {
            return text.to_string();
        }
        let mut result = text.to_string();
        let mut column = occupied;
        for flake in flakes {
            let target = flake.column;
            if target < first_free || target >= self.width {
                continue;
            }
            result.push_str(&" ".repeat(target.saturating_sub(column)));
            let (red, green, blue) = flake.color;
            result.push_str(&format!("\x1b[38;2;{red};{green};{blue}m"));
            result.push_str(flake.glyph);
            result.push_str("\x1b[0m");
            column = target + 1;
        }
        result
    }
}
