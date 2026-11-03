//! The night sky: a still navy-to-indigo gradient, stars that twinkle on slow timers of their
//! own, a shooting star now and then, and faint lines from the pointer to the stars near it.

use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, Pixels};

use crate::common::{
    Pointer, STEP, Steps, between, distance, draw_logo, fill, glow, logo_size, pixel_of, quantised,
    value_noise,
};

const NAVY: Color = Color::Rgb(7, 10, 30);
const INDIGO: Color = Color::Rgb(30, 22, 64);
const SKY_SHADES: u8 = 4;
/// The lightest sky: text cells show the sky as it is, and stars under them no brighter.
const TEXT_CEILING: Color = INDIGO;
const CELLS_PER_STAR: usize = 40;
/// Cool white, pale blue and a warm few.
const STAR_TINTS: [Color; 4] = [
    Color::Rgb(226, 232, 255),
    Color::Rgb(226, 232, 255),
    Color::Rgb(168, 194, 255),
    Color::Rgb(255, 222, 182),
];
/// How far each twinkle level lifts a star from the sky towards its tint.
const TWINKLE_LEVELS: [f32; 4] = [0.25, 0.5, 0.75, 1.0];
/// Steps from the sky to a light: few, so a star or a fading trail changes colour rarely.
const GLOW_SHADES: u8 = 5;
/// Seconds a star keeps its brightness before its next twinkle.
const TWINKLE_SECONDS: (f64, f64) = (4.0, 12.0);
const METEOR_SECONDS: (f64, f64) = (6.0, 14.0);
const METEOR: Color = Color::Rgb(255, 248, 232);
const LINK: Color = Color::Rgb(62, 70, 124);
const LINK_RANGE: f32 = 15.0;
const LINKS: usize = 3;
const LOGO_TOP: Color = Color::Rgb(238, 240, 255);
const LOGO_BOTTOM: Color = Color::Rgb(150, 160, 222);
const MAX_METEORS: usize = 64;

pub fn sky() -> Box<dyn Backdrop> {
    Box::<Sky>::default()
}

/// Positions are in pixels relative to the sky's area.
struct Star {
    position: (i32, i32),
    tint: Color,
    level: usize,
    /// The `clock` of its next twinkle.
    next: f64,
}

struct Meteor {
    head: (f32, f32),
    /// Pixels per second.
    velocity: (f32, f32),
    /// Pixels of trail behind the head.
    length: f32,
    born: f64,
    lifetime: f32,
}

#[derive(Default)]
struct Sky {
    random: Rng,
    steps: Steps,
    /// The last painted area, which mouse events are relative to and the sky was built for.
    area: Rect,
    /// One colour per pixel, row by row.
    gradient: Vec<Color>,
    stars: Vec<Star>,
    meteors: Vec<Meteor>,
    next_meteor: f64,
    /// Simulated seconds; f64 so a sky open for days keeps its precision.
    clock: f64,
    pointer: Pointer,
}

/// Darker overhead, lighter at the horizon, with soft wavy edges between the bands.
fn shade(x: u16, y: u32, rows: u32) -> Color {
    let wobble = (value_noise(f32::from(x) * 0.04, y as f32 * 0.08, 1) - 0.5) * 0.2;
    quantised(NAVY, INDIGO, y as f32 / rows as f32 + wobble, SKY_SHADES)
}

impl Sky {
    fn respawn(&mut self, area: Rect) {
        self.area = area;
        let (width, rows) = (area.width, u32::from(area.height) * 2);
        self.gradient = (0..rows)
            .flat_map(|y| (0..width).map(move |x| shade(x, y, rows)))
            .collect();
        let count = usize::from(area.width) * usize::from(area.height) / CELLS_PER_STAR;
        self.stars = (0..count)
            .map(|_| Star {
                position: (
                    self.random.i32(0..i32::from(width)),
                    self.random.i32(0..rows as i32),
                ),
                tint: STAR_TINTS[self.random.usize(..STAR_TINTS.len())],
                level: self.random.usize(..TWINKLE_LEVELS.len()),
                // Staggered from the start, so the sky twinkles at once and never all together.
                next: self.clock + self.random.f64() * TWINKLE_SECONDS.1,
            })
            .collect();
        self.meteors.clear();
        self.next_meteor = self.clock + between(&mut self.random, METEOR_SECONDS);
    }

    /// A shooting star from `head`, falling to either side, never straight down.
    fn launch(&mut self, head: (f32, f32)) {
        let angle = 0.3 + 0.5 * self.random.f32();
        let side = if self.random.bool() { 1.0 } else { -1.0 };
        let speed = 45.0 + 25.0 * self.random.f32();
        self.meteors.push(Meteor {
            head,
            velocity: (side * speed * angle.cos(), speed * angle.sin()),
            length: 4.0 + 4.0 * self.random.f32(),
            born: self.clock,
            lifetime: 0.7 + 0.5 * self.random.f32(),
        });
    }

    fn advance(&mut self, elapsed: Duration, area: Rect) {
        if area != self.area {
            self.respawn(area);
        }
        for _ in 0..self.steps.due(elapsed, STEP) {
            self.clock += f64::from(STEP);
            let now = self.clock;
            for meteor in &mut self.meteors {
                meteor.head.0 += meteor.velocity.0 * STEP;
                meteor.head.1 += meteor.velocity.1 * STEP;
            }
            self.meteors
                .retain(|meteor| ((now - meteor.born) as f32) < meteor.lifetime);
            if now >= self.next_meteor {
                let start = (
                    self.random.f32() * f32::from(area.width),
                    self.random.f32() * f32::from(area.height) * 0.8,
                );
                self.launch(start);
                self.next_meteor = now + between(&mut self.random, METEOR_SECONDS);
            }
        }
        for star in &mut self.stars {
            if self.clock >= star.next {
                star.level = self.random.usize(..TWINKLE_LEVELS.len());
                star.next = self.clock + between(&mut self.random, TWINKLE_SECONDS);
            }
        }
    }

    fn draw_links(&self, pointer: (f32, f32), pixels: &mut Pixels) {
        let mut near: Vec<(f32, (f32, f32))> = self
            .stars
            .iter()
            .map(|star| {
                let position = (star.position.0 as f32, star.position.1 as f32);
                (distance(pointer, position), position)
            })
            .filter(|(reach, _)| *reach <= LINK_RANGE)
            .collect();
        near.sort_by(|first, second| first.0.total_cmp(&second.0));
        for (reach, star) in near.into_iter().take(LINKS) {
            let samples = reach.ceil() as i32;
            for sample in 0..samples {
                let along = sample as f32 / samples as f32;
                let x = pointer.0 + (star.0 - pointer.0) * along;
                let y = pointer.1 + (star.1 - pointer.1) * along;
                pixels.set(x.round() as i32, y.round() as i32, LINK);
            }
        }
    }

    fn draw_meteor(&self, meteor: &Meteor, pixels: &mut Pixels) {
        let fade = 1.0 - (self.clock - meteor.born) as f32 / meteor.lifetime;
        let speed = meteor.velocity.0.hypot(meteor.velocity.1);
        let direction = (meteor.velocity.0 / speed, meteor.velocity.1 / speed);
        for behind in 0..meteor.length.round() as i32 {
            let back = behind as f32;
            let point = (
                (meteor.head.0 - direction.0 * back).round() as i32,
                (meteor.head.1 - direction.1 * back).round() as i32,
            );
            let amount = fade * (1.0 - back / meteor.length);
            glow(pixels, &self.gradient, point, METEOR, amount, GLOW_SHADES);
        }
    }
}

impl Backdrop for Sky {
    fn frames_per_second(&self) -> u64 {
        15
    }

    fn logo_size(&self) -> (u16, u16) {
        logo_size()
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
        fill(&mut pixels, &self.gradient);
        if let Some(pointer) = self.pointer.noticed(self.clock) {
            self.draw_links(pointer, &mut pixels);
        }
        for star in &self.stars {
            glow(
                &mut pixels,
                &self.gradient,
                star.position,
                star.tint,
                TWINKLE_LEVELS[star.level],
                GLOW_SHADES,
            );
        }
        for meteor in &self.meteors {
            self.draw_meteor(meteor, &mut pixels);
        }
        pixels.render(area, text, NAVY, TEXT_CEILING, buffer);
        draw_logo(logo, LOGO_TOP, LOGO_BOTTOM, buffer);
    }

    fn mouse(&mut self, event: MouseEvent) {
        let Some(point) = pixel_of(self.area, event) else {
            return;
        };
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) if self.meteors.len() < MAX_METEORS => {
                self.launch(point);
            }
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.pointer.moved(point, self.clock);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::MouseEventKind;
    use tx_home::backdrop::layout;

    use super::*;
    use crate::common::MAX_GAP;
    use crate::common::tests::{
        assert_no_boxes_under_text, assert_nothing_bright_under_text, assert_paints_any_area,
        assert_survives_shrinking, assert_within_budget, click,
    };

    fn seeded() -> Sky {
        Sky {
            random: Rng::with_seed(7),
            ..Sky::default()
        }
    }

    fn paint(sky: &mut Sky, elapsed: Duration, area: Rect) {
        let (logo, _) = layout(area, sky.logo_size());
        let blank = Buffer::empty(area);
        sky.paint(elapsed, area, logo, &blank, &mut Buffer::empty(area));
    }

    /// A seeded sky painted once at 80x24.
    fn painted() -> Sky {
        let mut sky = seeded();
        paint(&mut sky, Duration::ZERO, Rect::new(0, 0, 80, 24));
        sky
    }

    #[test]
    fn paints_any_area_without_panicking() {
        assert_paints_any_area(&mut Sky::default());
    }

    #[test]
    fn a_shrinking_area_never_panics() {
        assert_survives_shrinking(&mut Sky::default());
    }

    #[test]
    fn a_long_gap_is_clamped() {
        let mut sky = Sky::default();
        let area = Rect::new(0, 0, 80, 24);
        paint(&mut sky, Duration::ZERO, area);
        paint(&mut sky, Duration::from_secs(3600), area);
        assert!(sky.steps.taken <= (MAX_GAP / STEP).ceil() as usize);
    }

    #[test]
    fn stays_within_the_budget() {
        assert_within_budget(&mut seeded());
    }

    #[test]
    fn no_boxes_under_text() {
        assert_no_boxes_under_text(&mut seeded());
    }

    #[test]
    fn nothing_bright_under_text() {
        assert_nothing_bright_under_text(&mut seeded(), TEXT_CEILING);
    }

    #[test]
    fn a_click_launches_a_shooting_star() {
        let mut sky = painted();
        sky.meteors.clear();
        sky.mouse(click(10, 5));
        assert_eq!(sky.meteors.len(), 1);
        assert_eq!(sky.meteors[0].head, (10.0, 11.0));
        paint(
            &mut sky,
            Duration::from_millis(200),
            Rect::new(0, 0, 80, 24),
        );
        assert_ne!(sky.meteors[0].head, (10.0, 11.0), "it flies");
    }

    #[test]
    fn clicks_are_capped() {
        let mut sky = painted();
        for _ in 0..200 {
            sky.mouse(click(10, 5));
        }
        assert!(sky.meteors.len() <= MAX_METEORS, "{}", sky.meteors.len());
    }

    #[test]
    fn events_outside_the_sky_are_ignored() {
        let mut sky = painted();
        sky.meteors.clear();
        sky.mouse(click(500, 500));
        sky.mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(80, 0)
        });
        assert!(sky.meteors.is_empty());
        assert_eq!(sky.pointer.noticed(sky.clock), None);
    }

    #[test]
    fn the_pointer_is_forgotten_after_three_seconds() {
        let mut sky = painted();
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(10, 5)
        };
        sky.mouse(moved);
        assert_eq!(sky.pointer.noticed(sky.clock), Some((10.0, 11.0)));
        let area = Rect::new(0, 0, 80, 24);
        for tick in 1..=80 {
            paint(&mut sky, Duration::from_millis(tick * 50), area);
        }
        assert_eq!(sky.pointer.noticed(sky.clock), None);
        sky.mouse(moved);
        assert_eq!(sky.pointer.noticed(sky.clock), Some((10.0, 11.0)));
    }
}
