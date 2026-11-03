//! The deep sea: near-black water with a slow caustic, jellyfish that pulse as they rise, and
//! marine snow. A click sends a ring out that lights up the jellyfish near it; a pointer
//! nearby makes them drift away.

use std::f32::consts::PI;
use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, LOGO, Pixels, palette};

use crate::common::{quantised, value_noise};

const WATER: Color = Color::Rgb(2, 6, 14);
const CAUSTIC: Color = Color::Rgb(9, 32, 46);
const CAUSTIC_SHADES: u8 = 4;
/// The lightest water: text cells show the water as it is, and jellyfish under them no brighter.
const TEXT_CEILING: Color = CAUSTIC;
/// Cyan, magenta and teal.
const TINTS: [Color; 3] = [
    Color::Rgb(70, 220, 240),
    Color::Rgb(230, 80, 200),
    Color::Rgb(50, 200, 160),
];
const SNOW: Color = Color::Rgb(70, 100, 120);
const RING: Color = Color::Rgb(120, 200, 230);
const LOGO_TOP: Color = Color::Rgb(150, 235, 245);
const LOGO_BOTTOM: Color = Color::Rgb(96, 160, 214);
/// Steps from the water to a light: few, so a jellyfish changes colour rarely.
const GLOW_SHADES: u8 = 5;
const PULSE_LEVELS: f32 = 3.0;
const CELLS_PER_JELLY: usize = 700;
const JELLIES: (usize, usize) = (4, 7);
const CELLS_PER_FLECK: usize = 150;
const STEP: f32 = 1.0 / 60.0;
/// The most simulated time one paint catches up on, so a long pause doesn't replay hours.
const MAX_GAP: f32 = 0.25;
const MAX_RINGS: usize = 8;
const RING_SPEED: f32 = 30.0;
const RING_SECONDS: f64 = 0.8;
/// Pixels from a click that light a jellyfish for `LIT_SECONDS`.
const LIGHT_REACH: f32 = 24.0;
const LIT_SECONDS: f64 = 2.0;
/// Pixels from the pointer inside which a jellyfish is pushed away, at pixels per second.
const SHY_REACH: f32 = 20.0;
const SHY_SPEED: f32 = 10.0;
/// Seconds a still pointer keeps its effect: a terminal never says the mouse has left.
const POINTER_MEMORY: f64 = 3.0;
/// The part of each pulse spent contracting; the rest is a glide.
const SQUEEZE: f32 = 0.3;

pub fn deep() -> Box<dyn Backdrop> {
    Box::new(Abyss::default())
}

struct Jelly {
    position: (f32, f32),
    /// Pixels per second sideways.
    drift: f32,
    radius: f32,
    tint: Color,
    tentacles: usize,
    tentacle_length: i32,
    pulse_seconds: f32,
    pulse_offset: f32,
    /// The `clock` until which a click keeps it bright.
    lit_until: f64,
}

struct Fleck {
    position: (f32, f32),
    speed: f32,
}

struct Ring {
    center: (f32, f32),
    born: f64,
}

#[derive(Default)]
struct Abyss {
    random: Rng,
    last: Option<Duration>,
    remainder: f32,
    /// The last painted area, which mouse events are relative to and the scene was built for.
    area: Rect,
    /// One colour per pixel, row by row.
    water: Vec<Color>,
    jellies: Vec<Jelly>,
    snow: Vec<Fleck>,
    rings: Vec<Ring>,
    /// Simulated seconds; f64 so a scene open for days keeps its precision.
    clock: f64,
    /// In pixels, and the `clock` it last moved at.
    pointer: Option<((f32, f32), f64)>,
    #[cfg(test)]
    steps: usize,
}

fn distance(first: (f32, f32), second: (f32, f32)) -> f32 {
    (first.0 - second.0).hypot(first.1 - second.1)
}

impl Jelly {
    /// 0 gliding, up to 1 at the hardest squeeze of the bell.
    fn contraction(&self, clock: f64) -> f32 {
        let phase = (clock as f32 / self.pulse_seconds + self.pulse_offset).fract();
        if phase < SQUEEZE {
            (phase / SQUEEZE * PI).sin()
        } else {
            0.0
        }
    }
}

impl Abyss {
    fn noticed_pointer(&self) -> Option<(f32, f32)> {
        self.pointer
            .filter(|(_, moved)| self.clock - moved <= POINTER_MEMORY)
            .map(|(point, _)| point)
    }

    fn spawn_jelly(&mut self, vertical: f32) -> Jelly {
        let radius = 4.0 + 3.0 * self.random.f32();
        Jelly {
            position: (self.random.f32() * f32::from(self.area.width), vertical),
            drift: self.random.f32() * 2.0 - 1.0,
            radius,
            tint: TINTS[self.random.usize(..TINTS.len())],
            tentacles: self.random.usize(3..=5),
            tentacle_length: self.random.i32(8..=14),
            pulse_seconds: 4.0 + 3.0 * self.random.f32(),
            pulse_offset: self.random.f32(),
            lit_until: 0.0,
        }
    }

    fn respawn(&mut self, area: Rect) {
        self.area = area;
        let rows = f32::from(area.height) * 2.0;
        let cells = usize::from(area.width) * usize::from(area.height);
        let count = (cells / CELLS_PER_JELLY).clamp(JELLIES.0, JELLIES.1);
        self.jellies = (0..count)
            .map(|_| {
                let vertical = self.random.f32() * rows;
                self.spawn_jelly(vertical)
            })
            .collect();
        self.snow = (0..cells / CELLS_PER_FLECK)
            .map(|_| Fleck {
                position: (
                    self.random.f32() * f32::from(area.width),
                    self.random.f32() * rows,
                ),
                speed: 1.5 + 2.0 * self.random.f32(),
            })
            .collect();
        self.rings.clear();
    }

    fn advance(&mut self, elapsed: Duration, area: Rect) {
        if area != self.area {
            self.respawn(area);
        }
        let gap = self
            .last
            .map_or(Duration::ZERO, |last| elapsed.saturating_sub(last));
        self.last = Some(elapsed);
        self.remainder += gap.as_secs_f32().min(MAX_GAP);
        let rows = f32::from(area.height) * 2.0;
        let width = f32::from(area.width);
        while self.remainder >= STEP {
            self.remainder -= STEP;
            self.clock += f64::from(STEP);
            let pointer = self.noticed_pointer();
            for index in 0..self.jellies.len() {
                let jelly = &mut self.jellies[index];
                jelly.position.1 -= (2.5 + 4.0 * jelly.contraction(self.clock)) * STEP;
                jelly.position.0 += jelly.drift * STEP;
                if let Some(pointer) = pointer {
                    let away = distance(jelly.position, pointer);
                    if away < SHY_REACH {
                        jelly.position.0 +=
                            (jelly.position.0 - pointer.0) / away.max(1.0) * SHY_SPEED * STEP;
                        jelly.position.1 +=
                            (jelly.position.1 - pointer.1) / away.max(1.0) * SHY_SPEED * STEP;
                    }
                }
                let (x, y) = jelly.position;
                let gone = y + jelly.tentacle_length as f32 + 2.0 < 0.0
                    || x < -jelly.radius
                    || x > width + jelly.radius;
                if gone {
                    let radius = jelly.radius;
                    self.jellies[index] = self.spawn_jelly(rows + radius);
                }
            }
            for fleck in &mut self.snow {
                fleck.position.1 += fleck.speed * STEP;
                if fleck.position.1 >= rows {
                    fleck.position = (self.random.f32() * width, 0.0);
                }
            }
            let now = self.clock;
            self.rings.retain(|ring| now - ring.born < RING_SECONDS);
            #[cfg(test)]
            {
                self.steps += 1;
            }
        }
    }

    fn caustic(&mut self, width: u16, rows: u16) {
        let drift = self.clock as f32 * 0.02;
        self.water.clear();
        self.water.extend((0..rows).flat_map(|y| {
            (0..width).map(move |x| {
                let light = value_noise(f32::from(x) * 0.05 + drift, f32::from(y) * 0.1, 3);
                quantised(WATER, CAUSTIC, light, CAUSTIC_SHADES)
            })
        }));
    }

    /// Pixel (x, y) `amount` of the way from the water behind it to `light`; nothing off screen.
    fn glow(&self, pixels: &mut Pixels, (x, y): (i32, i32), light: Color, amount: f32) {
        let on_screen = (0..i32::from(pixels.width())).contains(&x)
            && (0..i32::from(pixels.height())).contains(&y);
        if on_screen {
            let behind = self.water[y as usize * usize::from(pixels.width()) + x as usize];
            pixels.set(x, y, quantised(behind, light, amount, GLOW_SHADES));
        }
    }

    fn draw_jelly(&self, jelly: &Jelly, pixels: &mut Pixels) {
        let squeeze = (jelly.contraction(self.clock) * PULSE_LEVELS).round() / PULSE_LEVELS;
        let lit = self.clock < jelly.lit_until;
        let brightness = if lit { 1.0 } else { 0.4 + 0.25 * squeeze };
        let (center_x, center_y) = (jelly.position.0.round(), jelly.position.1.round());
        let half_width = jelly.radius * (1.0 - 0.3 * squeeze);
        let height = (jelly.radius * 0.7 * (1.0 + 0.25 * squeeze)).round();
        for rise in 0..=height as i32 {
            let across = (1.0 - (rise as f32 / height).powi(2)).sqrt() * half_width;
            for offset in -(across.round() as i32)..=across.round() as i32 {
                let rim = rise == 0 || rise == height as i32 || offset.abs() as f32 >= across - 0.5;
                let amount = if rim { brightness } else { brightness * 0.5 };
                let point = (center_x as i32 + offset, center_y as i32 - rise);
                self.glow(pixels, point, jelly.tint, amount);
            }
        }
        let length = jelly.tentacle_length;
        for index in 0..jelly.tentacles {
            let along = index as f32 / (jelly.tentacles - 1) as f32;
            let start = center_x - half_width * 0.7 + along * half_width * 1.4;
            for step in 1..=length {
                let sway =
                    (self.clock as f32 * 0.5 + index as f32 * 1.7 + step as f32 * 0.45).sin() * 1.5;
                let fade = 1.0 - step as f32 / length as f32;
                let point = ((start + sway).round() as i32, center_y as i32 + step);
                self.glow(pixels, point, jelly.tint, brightness * 0.8 * fade);
            }
        }
    }

    fn draw_ring(&self, ring: &Ring, pixels: &mut Pixels) {
        let age = (self.clock - ring.born) as f32;
        let radius = age * RING_SPEED;
        let fade = 1.0 - age / RING_SECONDS as f32;
        let samples = (2.0 * PI * radius).ceil().max(1.0) as i32;
        for sample in 0..samples {
            let angle = sample as f32 / samples as f32 * 2.0 * PI;
            let point = (
                (ring.center.0 + radius * angle.cos()).round() as i32,
                (ring.center.1 + radius * angle.sin()).round() as i32,
            );
            self.glow(pixels, point, RING, fade * 0.6);
        }
    }
}

impl Backdrop for Abyss {
    fn frames_per_second(&self) -> u64 {
        12
    }

    fn logo_size(&self) -> (u16, u16) {
        (LOGO[0].chars().count() as u16, LOGO.len() as u16)
    }

    fn paint(
        &mut self,
        elapsed: Duration,
        area: Rect,
        logo: Option<Rect>,
        text: &Buffer,
        buffer: &mut Buffer,
    ) {
        self.advance(elapsed, area);
        self.caustic(area.width, area.height * 2);
        let mut pixels = Pixels::new(area.width, area.height);
        let width = usize::from(area.width);
        for (index, color) in self.water.iter().enumerate() {
            pixels.set((index % width) as i32, (index / width) as i32, *color);
        }
        for fleck in &self.snow {
            let point = (fleck.position.0 as i32, fleck.position.1 as i32);
            self.glow(&mut pixels, point, SNOW, 1.0);
        }
        for jelly in &self.jellies {
            self.draw_jelly(jelly, &mut pixels);
        }
        for ring in &self.rings {
            self.draw_ring(ring, &mut pixels);
        }
        pixels.render(area, text, WATER, TEXT_CEILING, buffer);
        if let Some(logo) = logo {
            for (row, line) in LOGO.iter().enumerate() {
                let color = quantised(
                    LOGO_TOP,
                    LOGO_BOTTOM,
                    row as f32 / (LOGO.len() - 1) as f32,
                    LOGO.len() as u8,
                );
                for (column, glyph) in line.chars().enumerate().filter(|(_, glyph)| *glyph != ' ') {
                    buffer[(logo.x + column as u16, logo.y + row as u16)]
                        .set_char(glyph)
                        .set_fg(palette(color));
                }
            }
        }
    }

    fn mouse(&mut self, event: MouseEvent) {
        if !self.area.contains(Position::new(event.column, event.row)) {
            return;
        }
        let point = (
            f32::from(event.column - self.area.x),
            f32::from(event.row - self.area.y) * 2.0 + 1.0,
        );
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.rings.len() < MAX_RINGS {
                    self.rings.push(Ring {
                        center: point,
                        born: self.clock,
                    });
                }
                for jelly in &mut self.jellies {
                    if distance(jelly.position, point) <= LIGHT_REACH {
                        jelly.lit_until = self.clock + LIT_SECONDS;
                    }
                }
            }
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.pointer = Some((point, self.clock));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::style::Style;
    use tx_home::backdrop::layout;

    use super::*;
    use crate::common::tests::{assert_nothing_bright_under_text, assert_within_budget};

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn seeded() -> Abyss {
        Abyss {
            random: Rng::with_seed(7),
            ..Abyss::default()
        }
    }

    fn paint(abyss: &mut Abyss, elapsed: Duration, area: Rect) {
        let (logo, _) = layout(area, abyss.logo_size());
        let blank = Buffer::empty(area);
        abyss.paint(elapsed, area, logo, &blank, &mut Buffer::empty(area));
    }

    /// A seeded scene painted once at 80x24.
    fn painted() -> Abyss {
        let mut abyss = seeded();
        paint(&mut abyss, Duration::ZERO, Rect::new(0, 0, 80, 24));
        abyss
    }

    #[test]
    fn paints_any_area_without_panicking() {
        let mut abyss = Abyss::default();
        let (logo_width, logo_height) = abyss.logo_size();
        for (width, height) in [(0, 0), (1, 1), (80, 24), (250, 70)] {
            let area = Rect::new(0, 0, width, height);
            let boxes = [
                None,
                (width >= logo_width && height >= logo_height)
                    .then(|| Rect::new(0, 0, logo_width, logo_height)),
            ];
            for logo in boxes {
                let mut buffer = Buffer::empty(area);
                abyss.paint(
                    Duration::from_secs(3),
                    area,
                    logo,
                    &Buffer::empty(area),
                    &mut buffer,
                );
            }
        }
    }

    #[test]
    fn a_shrinking_area_never_panics() {
        let mut abyss = Abyss::default();
        for (second, (width, height)) in [(250, 70), (10, 3), (0, 0), (80, 24)]
            .into_iter()
            .enumerate()
        {
            let area = Rect::new(0, 0, width, height);
            let (logo, content) = layout(area, abyss.logo_size());
            let mut text = Buffer::empty(area);
            text.set_style(content, Style::new().bg(Color::Red));
            abyss.mouse(click(width / 2, height / 2));
            let mut buffer = Buffer::empty(area);
            abyss.paint(
                Duration::from_secs(second as u64),
                area,
                logo,
                &text,
                &mut buffer,
            );
        }
    }

    #[test]
    fn a_long_gap_is_clamped() {
        let mut abyss = Abyss::default();
        let area = Rect::new(0, 0, 80, 24);
        paint(&mut abyss, Duration::ZERO, area);
        paint(&mut abyss, Duration::from_secs(3600), area);
        assert!(abyss.steps <= (MAX_GAP / STEP).ceil() as usize);
    }

    #[test]
    fn stays_within_the_budget() {
        assert_within_budget(&mut seeded());
    }

    #[test]
    fn nothing_bright_under_text() {
        assert_nothing_bright_under_text(&mut seeded(), TEXT_CEILING);
    }

    #[test]
    fn a_click_lights_up_nearby_jellies() {
        let mut abyss = painted();
        abyss.jellies.truncate(2);
        abyss.jellies[0].position = (10.0, 11.0);
        abyss.jellies[1].position = (70.0, 40.0);
        abyss.mouse(click(12, 5));
        assert!(abyss.jellies[0].lit_until > abyss.clock, "near one lights");
        assert_eq!(abyss.jellies[1].lit_until, 0.0, "far one stays dim");
        let area = Rect::new(0, 0, 80, 24);
        for tick in 1..=50 {
            paint(&mut abyss, Duration::from_millis(tick * 100), area);
        }
        assert!(abyss.clock > abyss.jellies[0].lit_until, "and then fades");
    }

    #[test]
    fn jellies_drift_away_from_the_pointer() {
        let mut abyss = painted();
        abyss.jellies.truncate(1);
        abyss.jellies[0].position = (40.0, 30.0);
        abyss.jellies[0].drift = 0.0;
        abyss.mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(35, 15)
        });
        paint(
            &mut abyss,
            Duration::from_millis(200),
            Rect::new(0, 0, 80, 24),
        );
        assert!(abyss.jellies[0].position.0 > 40.0, "pushed to the right");
    }

    #[test]
    fn clicks_are_capped() {
        let mut abyss = painted();
        for _ in 0..200 {
            abyss.mouse(click(10, 5));
        }
        assert!(abyss.rings.len() <= MAX_RINGS, "{}", abyss.rings.len());
    }

    #[test]
    fn events_outside_the_water_are_ignored() {
        let mut abyss = painted();
        abyss.mouse(click(500, 500));
        abyss.mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(80, 0)
        });
        assert!(abyss.rings.is_empty());
        assert_eq!(abyss.pointer, None);
    }

    #[test]
    fn the_pointer_is_forgotten_after_three_seconds() {
        let mut abyss = painted();
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(10, 5)
        };
        abyss.mouse(moved);
        assert_eq!(abyss.noticed_pointer(), Some((10.0, 11.0)));
        let area = Rect::new(0, 0, 80, 24);
        for tick in 1..=80 {
            paint(&mut abyss, Duration::from_millis(tick * 50), area);
        }
        assert_eq!(abyss.noticed_pointer(), None);
    }
}
