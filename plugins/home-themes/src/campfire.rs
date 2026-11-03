//! A small fire at the bottom centre, on dark ground under home's footer: a few shades of orange
//! glow flickering on a slow timer, and sparks that rise with a sway, cooling from yellow to red
//! to dark. A click stokes it.

use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, Pixels};

use crate::common::{
    STEP, Steps, draw_logo, logo_size, noise_time, pixel_of, quantised, value_noise,
};

const NIGHT: Color = Color::Rgb(14, 9, 10);
const EMBER_GLOW: Color = Color::Rgb(214, 96, 22);
const GLOW_SHADES: u8 = 5;
/// The glow's second-brightest shade: a list row over the glow shows it as it is, or near enough
/// at the core that it is not a dark box, and stays readable.
const TEXT_CEILING: Color = Color::Rgb(164, 74, 19);
/// Pixel rows of dark ground under the fire: the bottom cell row, where home's footer goes.
const GROUND_ROWS: f32 = 2.0;
const FLAME_HOT: Color = Color::Rgb(255, 226, 120);
const FLAME_COOL: Color = Color::Rgb(240, 120, 30);
const SPARK_HOT: Color = Color::Rgb(255, 222, 110);
const SPARK_RED: Color = Color::Rgb(200, 48, 24);
const SPARK_DEAD: Color = Color::Rgb(48, 16, 12);
const LOGO_TOP: Color = Color::Rgb(255, 220, 150);
const LOGO_BOTTOM: Color = Color::Rgb(214, 112, 52);
const FLAME_HALF_WIDTH: i32 = 2;
const FLAME_HEIGHT: f32 = 7.0;
/// Sparks that appear on their own; clicks may take the total up to `MAX_SPARKS`.
const IDLE_SPARKS: usize = 22;
const MAX_SPARKS: usize = 64;
const BURST: usize = 20;
const STOKE_SECONDS: f64 = 1.5;
const STOKE_RAMP: f64 = 0.5;
const STOKE_LIGHT: f32 = 0.3;
const SPAWN_CHANCE: f32 = 0.1;

pub fn embers() -> Box<dyn Backdrop> {
    Box::<Campfire>::default()
}

struct Spark {
    /// Where it left the fire, in pixels relative to the area.
    origin: (f32, f32),
    /// Pixels per second, upwards.
    rise: f32,
    sway: f32,
    phase: f32,
    born: f64,
    lifetime: f32,
}

impl Spark {
    fn age(&self, clock: f64) -> f32 {
        (clock - self.born) as f32 / self.lifetime
    }

    fn position(&self, clock: f64) -> (i32, i32) {
        let seconds = (clock - self.born) as f32;
        (
            (self.origin.0 + self.sway * (self.phase + 3.0 * seconds).sin()).round() as i32,
            (self.origin.1 - self.rise * seconds).round() as i32,
        )
    }
}

/// Yellow, then red, then dark, as `age` goes from 0 to 1.
fn spark_color(age: f32) -> Color {
    if age < 0.5 {
        quantised(SPARK_HOT, SPARK_RED, age * 2.0, 4)
    } else {
        quantised(SPARK_RED, SPARK_DEAD, (age - 0.5) * 2.0, 3)
    }
}

#[derive(Default)]
struct Campfire {
    random: Rng,
    steps: Steps,
    /// The last painted area, which mouse events are relative to and the fire was built for.
    area: Rect,
    sparks: Vec<Spark>,
    /// Simulated seconds; f64 so a fire left open for days keeps its precision.
    clock: f64,
    stoked_until: f64,
}

impl Campfire {
    fn centre(&self) -> (f32, f32) {
        (
            f32::from(self.area.width) / 2.0,
            f32::from(self.area.height) * 2.0 - GROUND_ROWS - 1.0 - FLAME_HEIGHT,
        )
    }

    fn spark(&mut self, rise: f32) {
        let (x, y) = self.centre();
        self.sparks.push(Spark {
            origin: (x + (self.random.f32() - 0.5) * 4.0, y),
            rise: rise * (0.7 + 0.6 * self.random.f32()),
            sway: 1.0 + 2.0 * self.random.f32(),
            phase: self.random.f32() * std::f32::consts::TAU,
            born: self.clock,
            lifetime: 2.0 + 2.0 * self.random.f32(),
        });
    }

    fn advance(&mut self, elapsed: Duration, area: Rect) {
        if area != self.area {
            self.area = area;
            self.sparks.clear();
        }
        for _ in 0..self.steps.due(elapsed, STEP) {
            self.clock += f64::from(STEP);
            let now = self.clock;
            self.sparks.retain(|spark| spark.age(now) < 1.0);
            if self.sparks.len() < IDLE_SPARKS && self.random.f32() < SPAWN_CHANCE {
                self.spark(14.0);
            }
        }
    }

    fn stoke(&self) -> f32 {
        let left = (self.stoked_until - self.clock).clamp(0.0, STOKE_RAMP) / STOKE_RAMP;
        STOKE_LIGHT * left as f32
    }

    fn draw_glow(&self, pixels: &mut Pixels) {
        let (width, rows) = (f32::from(pixels.width()), f32::from(pixels.height()));
        let ground = rows - GROUND_ROWS;
        let (centre_x, centre_y) = (width / 2.0, ground - 1.0);
        let (radius_x, radius_y) = ((width * 0.12).max(8.0), rows * 0.35);
        let flicker = 0.8 + 0.2 * value_noise(noise_time(self.clock, 0.8), 0.0, 5) + self.stoke();
        let (left, right) = (
            (centre_x - radius_x).floor() as i32,
            (centre_x + radius_x).ceil() as i32,
        );
        for y in (centre_y - radius_y).floor() as i32..ground as i32 {
            for x in left..=right {
                let reach =
                    ((x as f32 - centre_x) / radius_x).hypot((y as f32 - centre_y) / radius_y);
                let rough = (value_noise(x as f32 * 0.3, y as f32 * 0.3, 3) - 0.5) * 0.15;
                let light = (1.0 - reach).max(0.0) * flicker + rough * (1.0 - reach).max(0.0);
                let color = quantised(NIGHT, EMBER_GLOW, light, GLOW_SHADES);
                if color != NIGHT {
                    pixels.set(x, y, color);
                }
            }
        }
    }

    fn draw_flame(&self, pixels: &mut Pixels) {
        let (centre_x, base) = (
            i32::from(pixels.width() / 2),
            i32::from(pixels.height()) - GROUND_ROWS as i32 - 1,
        );
        for across in -FLAME_HALF_WIDTH..=FLAME_HALF_WIDTH {
            let taper = 1.0 - across.abs() as f32 / (FLAME_HALF_WIDTH + 1) as f32;
            let wave = value_noise(across as f32 * 1.7, noise_time(self.clock, 4.0), 9);
            let height = (FLAME_HEIGHT * taper * (0.6 + 0.4 * wave)).round() as i32;
            for up in 0..height {
                let color = quantised(FLAME_HOT, FLAME_COOL, up as f32 / height as f32, 3);
                pixels.set(centre_x + across, base - up, color);
            }
        }
    }
}

impl Backdrop for Campfire {
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
        self.draw_glow(&mut pixels);
        self.draw_flame(&mut pixels);
        for spark in &self.sparks {
            let (x, y) = spark.position(self.clock);
            pixels.set(x, y, spark_color(spark.age(self.clock)));
        }
        pixels.render(area, text, NIGHT, TEXT_CEILING, buffer);
        draw_logo(logo, LOGO_TOP, LOGO_BOTTOM, buffer);
    }

    fn mouse(&mut self, event: MouseEvent) {
        let inside = pixel_of(self.area, event).is_some();
        if inside && event.kind == MouseEventKind::Down(MouseButton::Left) {
            self.stoked_until = self.clock + STOKE_SECONDS;
            for _ in 0..BURST.min(MAX_SPARKS.saturating_sub(self.sparks.len())) {
                self.spark(26.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyModifiers;
    use ratatui::style::Style;
    use tx_home::backdrop::layout;

    use super::*;
    use crate::common::MAX_GAP;
    use crate::common::tests::{
        assert_no_boxes_under_text, assert_nothing_bright_under_text, assert_within_budget,
    };

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn seeded() -> Campfire {
        Campfire {
            random: Rng::with_seed(7),
            ..Campfire::default()
        }
    }

    fn paint(fire: &mut Campfire, elapsed: Duration, area: Rect) {
        let (logo, _) = layout(area, fire.logo_size());
        let blank = Buffer::empty(area);
        fire.paint(elapsed, area, logo, &blank, &mut Buffer::empty(area));
    }

    #[test]
    fn paints_any_area_without_panicking() {
        let mut fire = Campfire::default();
        let (logo_width, logo_height) = fire.logo_size();
        for (width, height) in [(0, 0), (1, 1), (80, 24), (250, 70)] {
            let area = Rect::new(0, 0, width, height);
            let boxes = [
                None,
                (width >= logo_width && height >= logo_height)
                    .then(|| Rect::new(0, 0, logo_width, logo_height)),
            ];
            for logo in boxes {
                let mut buffer = Buffer::empty(area);
                fire.paint(
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
        let mut fire = Campfire::default();
        for (second, (width, height)) in [(250, 70), (10, 3), (0, 0), (80, 24)]
            .into_iter()
            .enumerate()
        {
            let area = Rect::new(0, 0, width, height);
            let (logo, content) = layout(area, fire.logo_size());
            let mut text = Buffer::empty(area);
            text.set_style(content, Style::new().bg(Color::Red));
            fire.mouse(click(width / 2, height / 2));
            let mut buffer = Buffer::empty(area);
            fire.paint(
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
        let mut fire = Campfire::default();
        let area = Rect::new(0, 0, 80, 24);
        paint(&mut fire, Duration::ZERO, area);
        paint(&mut fire, Duration::from_secs(3600), area);
        assert!(fire.steps.taken <= (MAX_GAP / STEP).ceil() as usize);
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
    fn a_click_stokes_a_burst() {
        let mut fire = seeded();
        let area = Rect::new(0, 0, 80, 24);
        paint(&mut fire, Duration::ZERO, area);
        fire.sparks.clear();
        fire.mouse(click(10, 5));
        assert_eq!(fire.sparks.len(), BURST);
        assert!(fire.stoked_until > fire.clock);
        assert!(fire.stoke() > 0.0);
        for tick in 1..=40 {
            paint(&mut fire, Duration::from_millis(tick * 50), area);
        }
        assert_eq!(fire.stoke(), 0.0, "it settles after 1.5 s");
    }

    #[test]
    fn clicks_are_capped() {
        let mut fire = seeded();
        paint(&mut fire, Duration::ZERO, Rect::new(0, 0, 80, 24));
        for _ in 0..200 {
            fire.mouse(click(10, 5));
        }
        assert!(fire.sparks.len() <= MAX_SPARKS, "{}", fire.sparks.len());
    }

    #[test]
    fn events_outside_the_fire_are_ignored() {
        let mut fire = seeded();
        paint(&mut fire, Duration::ZERO, Rect::new(0, 0, 80, 24));
        fire.sparks.clear();
        fire.mouse(click(500, 500));
        assert!(fire.sparks.is_empty());
    }

    #[test]
    fn sparks_cool_and_die() {
        assert_eq!(spark_color(0.0), SPARK_HOT);
        assert_eq!(spark_color(0.5), SPARK_RED);
        assert_eq!(spark_color(1.0), SPARK_DEAD);
        let mut fire = seeded();
        let area = Rect::new(0, 0, 80, 24);
        paint(&mut fire, Duration::ZERO, area);
        fire.sparks.clear();
        fire.spark(14.0);
        let (_, start_y) = fire.sparks[0].position(fire.clock);
        let lifetime = fire.sparks[0].lifetime;
        fire.sparks[0].born = fire.clock;
        let mut elapsed = Duration::ZERO;
        while elapsed.as_secs_f32() < lifetime / 2.0 {
            elapsed += Duration::from_millis(100);
            fire.advance(elapsed, area);
        }
        assert!(fire.sparks[0].position(fire.clock).1 < start_y, "it rises");
        while elapsed.as_secs_f32() < lifetime + 0.3 {
            elapsed += Duration::from_millis(100);
            fire.advance(elapsed, area);
            fire.sparks.retain(|spark| spark.born == 0.0);
        }
        assert!(fire.sparks.is_empty(), "it dies");
    }
}
