//! A rainy window at night: soft out-of-focus lights behind the glass, and droplets that sit,
//! slide down in bursts, trail when fast and merge when they touch.

use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, LOGO, Pixels, palette};

use crate::common::quantised;

const NIGHT: Color = Color::Rgb(6, 7, 12);
const WARM: Color = Color::Rgb(78, 52, 34);
const COOL: Color = Color::Rgb(30, 52, 84);
/// The lightest glass: text cells show the lights as they are, and droplets under them no brighter.
const TEXT_CEILING: Color = WARM;
const BLOBS: (usize, usize) = (8, 14);
/// Blob radius as a share of the window's height in pixels.
const BLOB_RADIUS: (f32, f32) = (0.12, 0.3);
const BLOB_SHADES: u8 = 3;
const DROPLET: Color = Color::Rgb(176, 204, 232);
const DROPLET_SHADES: u8 = 4;
const TRAIL: Color = Color::Rgb(120, 150, 184);
const TRAIL_SHADES: u8 = 3;
const CLEAR: Color = Color::Rgb(44, 52, 64);
const LOGO_TOP: Color = Color::Rgb(226, 238, 252);
const LOGO_BOTTOM: Color = Color::Rgb(142, 170, 204);
const STEP: f32 = 1.0 / 60.0;
/// The most simulated time one paint catches up on, so a long pause doesn't replay hours.
const MAX_GAP: f32 = 0.25;
const CELLS_PER_DROPLET: usize = 150;
const MAX_DROPLETS: usize = 200;
const MAX_MASS: f32 = 4.0;
/// Masses at or above this slide fast enough to leave a trail.
const TRAIL_MASS: f32 = 2.0;
const TRAIL_SECONDS: f64 = 1.0;
const SPAWN_SECONDS: (f64, f64) = (0.3, 0.8);
/// How long a slide lasts (for a mass of 1), and how long a droplet sits between slides.
const SLIDE_SECONDS: (f64, f64) = (0.3, 0.9);
const REST_SECONDS: (f64, f64) = (1.5, 6.0);
const HOVER_RANGE: f32 = 8.0;
/// Seconds a still pointer keeps holding droplets: a terminal never says the mouse has left.
const POINTER_MEMORY: f64 = 3.0;
const WIPE_HALF_WIDTH: f32 = 5.0;
const WIPE_LENGTH: f32 = 40.0;
/// Seconds before droplets form again in a wiped streak.
const REFILL_SECONDS: f64 = 8.0;
const MAX_WIPES: usize = 16;

pub fn window() -> Box<dyn Backdrop> {
    Box::<Window>::default()
}

/// Positions are in pixels relative to the window's area.
struct Droplet {
    position: (f32, f32),
    mass: f32,
    /// The `clock` the current slide ends at; a droplet is sliding while the clock is before it.
    slide_until: f64,
    next_slide: f64,
}

struct Wipe {
    /// Top centre of the streak.
    start: (f32, f32),
    born: f64,
}

impl Wipe {
    fn covers(&self, (x, y): (f32, f32)) -> bool {
        (x - self.start.0).abs() <= WIPE_HALF_WIDTH
            && (self.start.1..=self.start.1 + WIPE_LENGTH).contains(&y)
    }
}

#[derive(Default)]
struct Window {
    random: Rng,
    last: Option<Duration>,
    remainder: f32,
    /// The last painted area, which mouse events are relative to and the lights were built for.
    area: Rect,
    /// One colour per pixel, row by row.
    lights: Vec<Color>,
    droplets: Vec<Droplet>,
    /// Pixels a fast droplet passed, and the `clock` it passed them at.
    trails: Vec<(i32, i32, f64)>,
    wipes: Vec<Wipe>,
    next_droplet: f64,
    /// Simulated seconds; f64 so a window open for days keeps its precision.
    clock: f64,
    /// In pixels, and the `clock` it last moved at.
    pointer: Option<((f32, f32), f64)>,
    #[cfg(test)]
    steps: usize,
}

fn between(random: &mut Rng, (low, high): (f64, f64)) -> f64 {
    low + (high - low) * random.f64()
}

fn distance(first: (f32, f32), second: (f32, f32)) -> f32 {
    (first.0 - second.0).hypot(first.1 - second.1)
}

impl Window {
    fn noticed_pointer(&self) -> Option<(f32, f32)> {
        self.pointer
            .filter(|(_, moved)| self.clock - moved <= POINTER_MEMORY)
            .map(|(point, _)| point)
    }

    /// Soft round lights on near-black; where they overlap the stronger one wins.
    fn blur_lights(&mut self, width: u16, rows: u16) {
        let blobs: Vec<_> = (0..self.random.usize(BLOBS.0..=BLOBS.1))
            .map(|_| {
                let radius = f32::from(rows)
                    * (BLOB_RADIUS.0 + (BLOB_RADIUS.1 - BLOB_RADIUS.0) * self.random.f32());
                let light = if self.random.bool() { WARM } else { COOL };
                (
                    (
                        self.random.f32() * f32::from(width),
                        self.random.f32() * f32::from(rows),
                    ),
                    radius,
                    light,
                )
            })
            .collect();
        self.lights = (0..rows)
            .flat_map(|y| (0..width).map(move |x| (f32::from(x), f32::from(y))))
            .map(|pixel| {
                let (strength, light) = blobs
                    .iter()
                    .map(|&(centre, radius, light)| {
                        let edge = (1.0 - distance(pixel, centre) / radius).clamp(0.0, 1.0);
                        (edge * edge * (3.0 - 2.0 * edge), light)
                    })
                    .fold(
                        (0.0, NIGHT),
                        |best, blob| if blob.0 > best.0 { blob } else { best },
                    );
                quantised(NIGHT, light, strength, BLOB_SHADES)
            })
            .collect();
    }

    fn respawn(&mut self, area: Rect) {
        self.area = area;
        let rows = area.height * 2;
        self.blur_lights(area.width, rows);
        self.droplets.clear();
        self.trails.clear();
        self.wipes.clear();
        let count = (usize::from(area.width) * usize::from(area.height) / CELLS_PER_DROPLET)
            .min(MAX_DROPLETS);
        for _ in 0..count {
            self.spawn();
        }
        self.next_droplet = self.clock + between(&mut self.random, SPAWN_SECONDS);
    }

    /// A bead somewhere on the glass, unless it is full or the spot was just wiped.
    fn spawn(&mut self) {
        let position = (
            self.random.f32() * f32::from(self.area.width),
            self.random.f32() * f32::from(self.area.height) * 2.0,
        );
        let wiped = self.wipes.iter().any(|wipe| wipe.covers(position));
        if wiped || self.droplets.len() >= MAX_DROPLETS {
            return;
        }
        self.droplets.push(Droplet {
            position,
            mass: 1.0 + 0.5 * self.random.f32(),
            slide_until: 0.0,
            // Staggered, so the glass is never all at rest or all sliding.
            next_slide: self.clock + between(&mut self.random, REST_SECONDS),
        });
    }

    fn start_slide(&mut self, index: usize) {
        let droplet = &mut self.droplets[index];
        let length = between(&mut self.random, SLIDE_SECONDS) * f64::from(droplet.mass.sqrt());
        droplet.slide_until = self.clock + length;
        droplet.next_slide = droplet.slide_until + between(&mut self.random, REST_SECONDS);
    }

    /// Droplets that touch become one: heavier, and so faster.
    fn merge_touching(&mut self) {
        let mut first = 0;
        while first < self.droplets.len() {
            let mut second = first + 1;
            while second < self.droplets.len() {
                let (one, other) = (&self.droplets[first], &self.droplets[second]);
                let reach = 0.5 * (one.mass + other.mass) + 0.5;
                if distance(one.position, other.position) >= reach {
                    second += 1;
                    continue;
                }
                let gone = self.droplets.swap_remove(second);
                let kept = &mut self.droplets[first];
                let mass = kept.mass + gone.mass;
                kept.position = (
                    (kept.position.0 * kept.mass + gone.position.0 * gone.mass) / mass,
                    (kept.position.1 * kept.mass + gone.position.1 * gone.mass) / mass,
                );
                kept.mass = mass.min(MAX_MASS);
                self.start_slide(first);
            }
            first += 1;
        }
    }

    fn step(&mut self) {
        self.clock += f64::from(STEP);
        let now = self.clock;
        let pointer = self.noticed_pointer();
        for index in 0..self.droplets.len() {
            if now >= self.droplets[index].next_slide && now >= self.droplets[index].slide_until {
                self.start_slide(index);
            }
        }
        let mut moved = false;
        for droplet in &mut self.droplets {
            let held = pointer.is_some_and(|at| distance(at, droplet.position) <= HOVER_RANGE);
            if now >= droplet.slide_until || held {
                continue;
            }
            moved = true;
            let before = droplet.position.1.round();
            droplet.position.1 += (8.0 + 9.0 * droplet.mass) * STEP;
            if droplet.mass >= TRAIL_MASS && droplet.position.1.round() != before {
                self.trails
                    .push((droplet.position.0.round() as i32, before as i32, now));
            }
        }
        let rows = f32::from(self.area.height) * 2.0;
        self.droplets
            .retain(|droplet| droplet.position.1 < rows + 2.0);
        if moved {
            self.merge_touching();
        }
        self.trails.retain(|trail| now - trail.2 < TRAIL_SECONDS);
        self.wipes.retain(|wipe| now - wipe.born < REFILL_SECONDS);
        if now >= self.next_droplet {
            self.spawn();
            self.next_droplet = now + between(&mut self.random, SPAWN_SECONDS);
        }
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
        while self.remainder >= STEP {
            self.remainder -= STEP;
            self.step();
            #[cfg(test)]
            {
                self.steps += 1;
            }
        }
    }

    /// Pixel (x, y) `amount` of the way from the glass behind it to `light`; nothing off the glass.
    fn glow(&self, pixels: &mut Pixels, (x, y): (i32, i32), light: Color, amount: f32, shades: u8) {
        let on_glass = (0..i32::from(pixels.width())).contains(&x)
            && (0..i32::from(pixels.height())).contains(&y);
        if on_glass {
            let behind = self.lights[y as usize * usize::from(pixels.width()) + x as usize];
            pixels.set(x, y, quantised(behind, light, amount, shades));
        }
    }

    fn draw_wipe(&self, wipe: &Wipe, pixels: &mut Pixels) {
        let fade = 1.0 - ((self.clock - wipe.born) / REFILL_SECONDS) as f32;
        for y in 0..WIPE_LENGTH as i32 {
            for x in -(WIPE_HALF_WIDTH as i32)..=WIPE_HALF_WIDTH as i32 {
                let point = (
                    wipe.start.0.round() as i32 + x,
                    wipe.start.1.round() as i32 + y,
                );
                self.glow(pixels, point, CLEAR, 0.3 * fade, BLOB_SHADES);
            }
        }
    }
}

impl Backdrop for Window {
    fn frames_per_second(&self) -> u64 {
        15
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
        let mut pixels = Pixels::new(area.width, area.height);
        let width = usize::from(area.width);
        for (index, color) in self.lights.iter().enumerate() {
            pixels.set((index % width) as i32, (index / width) as i32, *color);
        }
        for wipe in &self.wipes {
            self.draw_wipe(wipe, &mut pixels);
        }
        for &(x, y, born) in &self.trails {
            let fade = 1.0 - ((self.clock - born) / TRAIL_SECONDS) as f32;
            self.glow(&mut pixels, (x, y), TRAIL, 0.5 * fade, TRAIL_SHADES);
        }
        for droplet in &self.droplets {
            let (x, y) = (
                droplet.position.0.round() as i32,
                droplet.position.1.round() as i32,
            );
            for below in 0..droplet.mass.floor() as i32 {
                self.glow(&mut pixels, (x, y + below), DROPLET, 0.55, DROPLET_SHADES);
            }
        }
        pixels.render(area, text, NIGHT, TEXT_CEILING, buffer);
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
            MouseEventKind::Down(MouseButton::Left) if self.wipes.len() < MAX_WIPES => {
                let wipe = Wipe {
                    start: point,
                    born: self.clock,
                };
                self.droplets
                    .retain(|droplet| !wipe.covers(droplet.position));
                self.wipes.push(wipe);
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

    fn seeded() -> Window {
        Window {
            random: Rng::with_seed(7),
            ..Window::default()
        }
    }

    fn paint(window: &mut Window, elapsed: Duration, area: Rect) {
        let (logo, _) = layout(area, window.logo_size());
        let blank = Buffer::empty(area);
        window.paint(elapsed, area, logo, &blank, &mut Buffer::empty(area));
    }

    /// A seeded window painted once at 80x24, with no droplets and none due.
    fn bare() -> Window {
        let mut window = seeded();
        paint(&mut window, Duration::ZERO, Rect::new(0, 0, 80, 24));
        window.droplets.clear();
        window.next_droplet = f64::MAX;
        window
    }

    fn droplet(position: (f32, f32), sliding: bool) -> Droplet {
        Droplet {
            position,
            mass: 1.0,
            slide_until: if sliding { 100.0 } else { 0.0 },
            next_slide: f64::MAX,
        }
    }

    #[test]
    fn paints_any_area_without_panicking() {
        let mut window = Window::default();
        let (logo_width, logo_height) = window.logo_size();
        for (width, height) in [(0, 0), (1, 1), (80, 24), (250, 70)] {
            let area = Rect::new(0, 0, width, height);
            let boxes = [
                None,
                (width >= logo_width && height >= logo_height)
                    .then(|| Rect::new(0, 0, logo_width, logo_height)),
            ];
            for logo in boxes {
                let mut buffer = Buffer::empty(area);
                window.paint(
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
        let mut window = Window::default();
        for (second, (width, height)) in [(250, 70), (10, 3), (0, 0), (80, 24)]
            .into_iter()
            .enumerate()
        {
            let area = Rect::new(0, 0, width, height);
            let (logo, content) = layout(area, window.logo_size());
            let mut text = Buffer::empty(area);
            text.set_style(content, Style::new().bg(Color::Red));
            window.mouse(click(width / 2, height / 2));
            let mut buffer = Buffer::empty(area);
            window.paint(
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
        let mut window = Window::default();
        let area = Rect::new(0, 0, 80, 24);
        paint(&mut window, Duration::ZERO, area);
        paint(&mut window, Duration::from_secs(3600), area);
        assert!(window.steps <= (MAX_GAP / STEP).ceil() as usize);
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
    fn droplets_merge() {
        let mut window = bare();
        window.droplets = vec![droplet((10.0, 10.0), true), droplet((10.0, 14.0), false)];
        let area = Rect::new(0, 0, 80, 24);
        for tick in 1..=6 {
            paint(&mut window, Duration::from_millis(tick * 100), area);
        }
        assert_eq!(window.droplets.len(), 1);
        assert_eq!(window.droplets[0].mass, 2.0);
    }

    #[test]
    fn a_wipe_clears_droplets() {
        let mut window = bare();
        window.droplets = vec![
            droplet((10.0, 20.0), false),
            droplet((10.0, 5.0), false),
            droplet((60.0, 20.0), false),
        ];
        window.mouse(click(10, 5));
        let remaining: Vec<_> = window.droplets.iter().map(|d| d.position).collect();
        assert_eq!(remaining, [(10.0, 5.0), (60.0, 20.0)]);
        for _ in 0..50 {
            window.spawn();
        }
        assert!(
            window
                .droplets
                .iter()
                .all(|d| !window.wipes[0].covers(d.position)),
            "nothing forms in a fresh streak"
        );
    }

    #[test]
    fn droplets_never_exceed_the_cap() {
        let mut window = bare();
        for _ in 0..1000 {
            window.spawn();
        }
        assert_eq!(window.droplets.len(), MAX_DROPLETS);
    }

    #[test]
    fn clicks_are_capped() {
        let mut window = bare();
        for _ in 0..200 {
            window.mouse(click(10, 5));
        }
        assert!(window.wipes.len() <= MAX_WIPES, "{}", window.wipes.len());
    }

    #[test]
    fn a_hovered_droplet_holds_still() {
        let mut window = bare();
        window.droplets = vec![droplet((40.0, 20.0), true), droplet((10.0, 20.0), true)];
        window.mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(36, 10)
        });
        paint(
            &mut window,
            Duration::from_millis(100),
            Rect::new(0, 0, 80, 24),
        );
        assert_eq!(window.droplets[0].position.1, 20.0);
        assert!(window.droplets[1].position.1 > 20.0);
    }

    #[test]
    fn events_outside_the_window_are_ignored() {
        let mut window = bare();
        window.mouse(click(500, 500));
        window.mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(80, 0)
        });
        assert!(window.wipes.is_empty());
        assert_eq!(window.pointer, None);
    }

    #[test]
    fn the_pointer_is_forgotten_after_three_seconds() {
        let mut window = bare();
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(10, 5)
        };
        window.mouse(moved);
        assert_eq!(window.noticed_pointer(), Some((10.0, 11.0)));
        for tick in 1..=80 {
            paint(
                &mut window,
                Duration::from_millis(tick * 50),
                Rect::new(0, 0, 80, 24),
            );
        }
        assert_eq!(window.noticed_pointer(), None);
    }
}
