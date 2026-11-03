//! A night meadow: a still sky, grass and tree silhouettes, and fireflies that wander and pulse
//! on slow timers of their own. The pointer draws them in to circle it; a click scatters them.

use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, Pixels};

use crate::common::{
    Pointer, STEP, Steps, distance, draw_logo, fill, glow, logo_size, pixel_of, quantised,
    value_noise,
};

const SKY_TOP: Color = Color::Rgb(6, 9, 24);
const SKY_HORIZON: Color = Color::Rgb(18, 30, 52);
const SKY_SHADES: u8 = 3;
/// The lightest sky: text cells show the meadow as it is, and fireflies under them no brighter.
const TEXT_CEILING: Color = SKY_HORIZON;
const GRASS: Color = Color::Rgb(8, 22, 16);
const TREE: Color = Color::Rgb(5, 14, 12);
const GLOW: Color = Color::Rgb(206, 238, 84);
/// Steps from the scene to a light: few, so a pulse changes colour rarely.
const GLOW_SHADES: u8 = 4;
const LOGO_TOP: Color = Color::Rgb(214, 238, 120);
const LOGO_BOTTOM: Color = Color::Rgb(112, 164, 82);
const CELLS_PER_FIREFLY: usize = 200;
const FIREFLIES: (usize, usize) = (20, 40);
/// Pixels per second.
const SPEED: (f32, f32) = (6.0, 12.0);
/// Radians per second the wander may turn at, and the most any steering may.
const WANDER_TURN: f32 = 1.5;
const TURN_CAP: f32 = 3.5;
const PULSE_SECONDS: (f32, f32) = (3.0, 7.0);
const PULL_RANGE: f32 = 25.0;
/// Inside this the pull bends into a circle round the pointer.
const ORBIT_RADIUS: f32 = 8.0;
const SCATTER_RANGE: f32 = 15.0;
const BURST_SPEED: f32 = 45.0;
/// Seconds for a burst to die down to a third.
const BURST_DECAY: f32 = 0.4;

pub fn meadow() -> Box<dyn Backdrop> {
    Box::<Meadow>::default()
}

/// Positions are in pixels relative to the meadow's area.
struct Firefly {
    position: (f32, f32),
    heading: f32,
    speed: f32,
    /// Extra speed from a scatter, dying down.
    burst: f32,
    period: f32,
    offset: f32,
}

#[derive(Default)]
struct Meadow {
    random: Rng,
    steps: Steps,
    /// The last painted area, which mouse events are relative to and the meadow was built for.
    area: Rect,
    /// One colour per pixel, row by row.
    scene: Vec<Color>,
    fireflies: Vec<Firefly>,
    /// Simulated seconds; f64 so a meadow open for days keeps its precision.
    clock: f64,
    pointer: Pointer,
}

fn angle_to(from: (f32, f32), to: (f32, f32)) -> f32 {
    (to.1 - from.1).atan2(to.0 - from.0)
}

/// Turns `heading` towards `desired` by at most `max_turn`, the short way round.
fn steer(heading: f32, desired: f32, max_turn: f32) -> f32 {
    let difference = (desired - heading + PI).rem_euclid(TAU) - PI;
    heading + difference.clamp(-max_turn, max_turn)
}

fn between(random: &mut Rng, (low, high): (f32, f32)) -> f32 {
    low + (high - low) * random.f32()
}

fn paint_scene(width: u16, rows: u16) -> Vec<Color> {
    let mut scene: Vec<Color> = (0..rows)
        .flat_map(|y| {
            (0..width).map(move |x| {
                let wobble = (value_noise(f32::from(x) * 0.04, f32::from(y) * 0.08, 1) - 0.5) * 0.3;
                quantised(
                    SKY_TOP,
                    SKY_HORIZON,
                    f32::from(y) / f32::from(rows) + wobble,
                    SKY_SHADES,
                )
            })
        })
        .collect();
    let mut fill = |x: i32, y: i32, color: Color| {
        if (0..i32::from(width)).contains(&x) && (0..i32::from(rows)).contains(&y) {
            scene[y as usize * usize::from(width) + x as usize] = color;
        }
    };
    let (width_pixels, rows_pixels) = (f32::from(width), f32::from(rows));
    for (fraction, height) in [(0.14, 0.5), (0.8, 0.62)] {
        let trunk_x = (width_pixels * fraction) as i32;
        let canopy_radius = rows_pixels * height * 0.22;
        let canopy_y = rows_pixels * (1.0 - height * 0.6);
        for y in (canopy_y as i32)..i32::from(rows) {
            fill(trunk_x - 1, y, TREE);
            fill(trunk_x, y, TREE);
            fill(trunk_x + 1, y, TREE);
        }
        let reach = canopy_radius.ceil() as i32;
        for dy in -reach..=reach {
            for dx in -reach..=reach {
                if ((dx * dx + dy * dy) as f32).sqrt() <= canopy_radius {
                    fill(trunk_x + dx, canopy_y as i32 + dy, TREE);
                }
            }
        }
    }
    for x in 0..width {
        let grass = 0.15 + 0.1 * value_noise(f32::from(x) * 0.05, 0.0, 3);
        let top = (rows_pixels * (1.0 - grass)) as i32;
        for y in top..i32::from(rows) {
            fill(i32::from(x), y, GRASS);
        }
    }
    scene
}

impl Meadow {
    fn respawn(&mut self, area: Rect) {
        self.area = area;
        let (width, rows) = (f32::from(area.width), f32::from(area.height * 2));
        self.scene = paint_scene(area.width, area.height * 2);
        let cells = usize::from(area.width) * usize::from(area.height);
        let count = (cells / CELLS_PER_FIREFLY).clamp(FIREFLIES.0, FIREFLIES.1);
        self.fireflies = (0..count)
            .map(|_| Firefly {
                position: (self.random.f32() * width, self.random.f32() * rows),
                heading: self.random.f32() * TAU,
                speed: between(&mut self.random, SPEED),
                burst: 0.0,
                period: between(&mut self.random, PULSE_SECONDS),
                offset: self.random.f32(),
            })
            .collect();
    }

    fn step_firefly(
        firefly: &mut Firefly,
        random: &mut Rng,
        pointer: Option<(f32, f32)>,
        bounds: (f32, f32),
    ) {
        let wander = (random.f32() * 2.0 - 1.0) * WANDER_TURN * STEP;
        firefly.heading += wander;
        let inside = (0.0..=bounds.0).contains(&firefly.position.0)
            && (0.0..=bounds.1).contains(&firefly.position.1);
        let near = pointer.filter(|pointer| distance(*pointer, firefly.position) <= PULL_RANGE);
        if !inside {
            let centre = (bounds.0 / 2.0, bounds.1 / 2.0);
            firefly.heading = steer(
                firefly.heading,
                angle_to(firefly.position, centre),
                TURN_CAP * STEP,
            );
        } else if let Some(pointer) = near {
            // Straight at the pointer from afar, bending to sideways as it closes in.
            let closeness = 1.0 - (distance(pointer, firefly.position) / ORBIT_RADIUS).min(1.0);
            let desired = angle_to(firefly.position, pointer) + FRAC_PI_2 * closeness;
            firefly.heading = steer(firefly.heading, desired, TURN_CAP * STEP);
        }
        let speed = firefly.speed + firefly.burst;
        firefly.position.0 += firefly.heading.cos() * speed * STEP;
        firefly.position.1 += firefly.heading.sin() * speed * STEP;
        firefly.burst *= (-STEP / BURST_DECAY).exp();
    }

    fn advance(&mut self, elapsed: Duration, area: Rect) {
        if area != self.area {
            self.respawn(area);
        }
        let bounds = (f32::from(area.width), f32::from(area.height * 2));
        for _ in 0..self.steps.due(elapsed, STEP) {
            self.clock += f64::from(STEP);
            let pointer = self.pointer.noticed(self.clock);
            for firefly in &mut self.fireflies {
                Self::step_firefly(firefly, &mut self.random, pointer, bounds);
            }
        }
    }

    /// How lit a firefly is: off for part of every period, then swelling and fading.
    fn brightness(&self, firefly: &Firefly) -> f32 {
        let phase = (self.clock as f32 / firefly.period + firefly.offset).fract();
        ((0.5 - 0.5 * (phase * TAU).cos() - 0.3) / 0.7).max(0.0)
    }

    fn draw_firefly(&self, firefly: &Firefly, pixels: &mut Pixels) {
        let brightness = self.brightness(firefly);
        if brightness == 0.0 {
            return;
        }
        let (x, y) = (
            firefly.position.0.round() as i32,
            firefly.position.1.round() as i32,
        );
        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
            let point = (x + dx, y + dy);
            glow(
                pixels,
                &self.scene,
                point,
                GLOW,
                brightness * 0.4,
                GLOW_SHADES,
            );
        }
        glow(pixels, &self.scene, (x, y), GLOW, brightness, GLOW_SHADES);
    }
}

impl Backdrop for Meadow {
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
        fill(&mut pixels, &self.scene);
        for firefly in &self.fireflies {
            self.draw_firefly(firefly, &mut pixels);
        }
        pixels.render(area, text, SKY_TOP, TEXT_CEILING, buffer);
        draw_logo(logo, LOGO_TOP, LOGO_BOTTOM, buffer);
    }

    fn mouse(&mut self, event: MouseEvent) {
        let Some(point) = pixel_of(self.area, event) else {
            return;
        };
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                for firefly in &mut self.fireflies {
                    if distance(point, firefly.position) <= SCATTER_RANGE {
                        firefly.heading = angle_to(point, firefly.position);
                        firefly.burst = BURST_SPEED;
                    }
                }
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
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
    use ratatui::style::Style;
    use tx_home::backdrop::layout;

    use super::*;
    use crate::common::MAX_GAP;
    use crate::common::tests::{assert_nothing_bright_under_text, assert_within_budget};

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn seeded() -> Meadow {
        Meadow {
            random: Rng::with_seed(7),
            ..Meadow::default()
        }
    }

    fn paint(meadow: &mut Meadow, elapsed: Duration, area: Rect) {
        let (logo, _) = layout(area, meadow.logo_size());
        let blank = Buffer::empty(area);
        meadow.paint(elapsed, area, logo, &blank, &mut Buffer::empty(area));
    }

    /// A seeded meadow painted once at 80x24, with one firefly at `position` flying `heading`.
    fn with_one_firefly(position: (f32, f32), heading: f32) -> Meadow {
        let mut meadow = seeded();
        paint(&mut meadow, Duration::ZERO, Rect::new(0, 0, 80, 24));
        meadow.fireflies = vec![Firefly {
            position,
            heading,
            speed: 10.0,
            burst: 0.0,
            period: 5.0,
            offset: 0.0,
        }];
        meadow
    }

    #[test]
    fn paints_any_area_without_panicking() {
        let mut meadow = Meadow::default();
        let (logo_width, logo_height) = meadow.logo_size();
        for (width, height) in [(0, 0), (1, 1), (80, 24), (250, 70)] {
            let area = Rect::new(0, 0, width, height);
            let boxes = [
                None,
                (width >= logo_width && height >= logo_height)
                    .then(|| Rect::new(0, 0, logo_width, logo_height)),
            ];
            for logo in boxes {
                let mut buffer = Buffer::empty(area);
                meadow.paint(
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
        let mut meadow = Meadow::default();
        for (second, (width, height)) in [(250, 70), (10, 3), (0, 0), (80, 24)]
            .into_iter()
            .enumerate()
        {
            let area = Rect::new(0, 0, width, height);
            let (logo, content) = layout(area, meadow.logo_size());
            let mut text = Buffer::empty(area);
            text.set_style(content, Style::new().bg(Color::Red));
            meadow.mouse(click(width / 2, height / 2));
            let mut buffer = Buffer::empty(area);
            meadow.paint(
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
        let mut meadow = Meadow::default();
        let area = Rect::new(0, 0, 80, 24);
        paint(&mut meadow, Duration::ZERO, area);
        paint(&mut meadow, Duration::from_secs(3600), area);
        assert!(meadow.steps.taken <= (MAX_GAP / STEP).ceil() as usize);
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
    fn there_are_twenty_to_forty_fireflies() {
        for (width, height) in [(80, 24), (250, 70)] {
            let mut meadow = seeded();
            paint(&mut meadow, Duration::ZERO, Rect::new(0, 0, width, height));
            assert!((20..=40).contains(&meadow.fireflies.len()));
        }
    }

    #[test]
    fn fireflies_are_drawn_to_the_pointer() {
        // Flying straight away from a pointer 20 px off, on row 15 (pixel y 31).
        let mut meadow = with_one_firefly((60.0, 31.0), 0.0);
        let pointer = (40.0, 31.0);
        let area = Rect::new(0, 0, 80, 24);
        let before = distance(pointer, meadow.fireflies[0].position);
        for tick in 1..=50 {
            meadow.mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                ..click(40, 15)
            });
            paint(&mut meadow, Duration::from_millis(tick * 50), area);
        }
        let after = distance(pointer, meadow.fireflies[0].position);
        assert!(after < before - 8.0, "{before} -> {after}");
    }

    #[test]
    fn a_click_scatters_nearby_fireflies() {
        let mut meadow = with_one_firefly((48.0, 31.0), 0.0);
        meadow.fireflies.push(Firefly {
            position: (70.0, 31.0),
            heading: 0.0,
            speed: 10.0,
            burst: 0.0,
            period: 5.0,
            offset: 0.0,
        });
        let point = (45.0, 31.0);
        let before = distance(point, meadow.fireflies[0].position);
        meadow.mouse(click(45, 15));
        assert_eq!(meadow.fireflies[1].burst, 0.0, "too far to be scattered");
        paint(
            &mut meadow,
            Duration::from_millis(200),
            Rect::new(0, 0, 80, 24),
        );
        assert!(distance(point, meadow.fireflies[0].position) > before + 5.0);
    }

    #[test]
    fn events_outside_the_meadow_are_ignored() {
        let mut meadow = with_one_firefly((48.0, 31.0), 0.0);
        meadow.mouse(click(500, 500));
        meadow.mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(80, 0)
        });
        assert_eq!(meadow.fireflies[0].burst, 0.0);
        assert_eq!(meadow.pointer.noticed(meadow.clock), None);
    }

    #[test]
    fn the_pointer_is_forgotten_after_three_seconds() {
        let mut meadow = with_one_firefly((48.0, 31.0), 0.0);
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(10, 5)
        };
        meadow.mouse(moved);
        assert_eq!(meadow.pointer.noticed(meadow.clock), Some((10.0, 11.0)));
        let area = Rect::new(0, 0, 80, 24);
        for tick in 1..=80 {
            paint(&mut meadow, Duration::from_millis(tick * 50), area);
        }
        assert_eq!(meadow.pointer.noticed(meadow.clock), None);
        meadow.mouse(moved);
        assert_eq!(meadow.pointer.noticed(meadow.clock), Some((10.0, 11.0)));
    }
}
