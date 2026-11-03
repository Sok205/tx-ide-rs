//! The koi pond: a dark blue-violet water with a slow shimmer, koi swimming in it, ripples and
//! food on its surface, and the logo floating on top.

mod fish;
mod water;

use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, Pixels, palette};

use crate::common::{Pointer, STEP, Steps, angle, distance, draw_logo, logo_size, pixel_of};
use fish::{Attractions, Fish};
use water::{Pellet, Ripple, Wake};

const WATER_DEEP: (f32, f32, f32) = (14.0, 16.0, 38.0);
const WATER_LIGHT: (f32, f32, f32) = (30.0, 40.0, 82.0);
/// Shades per step from `WATER_DEEP` to `WATER_LIGHT`.
const WATER_SHADES: f32 = 3.0;
const LOGO_WHITE: Color = Color::Rgb(255, 244, 226);
/// The brightest water: text cells show the water as it is, and fish under them no brighter.
const TEXT_CEILING: Color = Color::Rgb(30, 40, 82);
/// How close a head must come to a pellet to eat it.
const EAT_RANGE: f32 = 1.5;
const EAT_RIPPLE: f32 = 0.4;
const BURST_RIPPLE: f32 = 0.25;
const CLICK_SCATTER: f32 = 3.0;
const WAKE_DEPOSIT: f32 = 0.5;
/// How far a full wake moves the water towards (and past) `WATER_LIGHT`.
const WAKE_LIGHT: f32 = 0.6;
/// Live ripples at most: each repaints its whole ring every frame, so rapid clicks would flood
/// tmux.
const MAX_RIPPLES: usize = 12;
const MAX_PELLETS: usize = 64;

pub fn pond() -> Box<dyn Backdrop> {
    Box::<Pond>::default()
}

#[derive(Default)]
struct Pond {
    fish: Vec<Fish>,
    random: Rng,
    steps: Steps,
    /// The area, in cells, the fish were spawned for.
    cells: usize,
    /// The last painted area, which mouse events are relative to.
    area: Rect,
    /// Simulated seconds; f64 so a pond open for days keeps its precision.
    clock: f64,
    ripples: Vec<Ripple>,
    pellets: Vec<Pellet>,
    pointer: Pointer,
    wake: Wake,
}

/// `wake`: the wake's energy here, lightening the water. The shimmer drifts over minutes.
fn water(x: i32, y: i32, seconds: f64, wake: f32) -> Color {
    let (x, y) = (x as f32, y as f32);
    let wave = (x * 0.21 + y * 0.13 + angle(seconds, 0.02)).sin()
        + (x * 0.07 - y * 0.19 - angle(seconds, 0.013)).sin();
    let mix = (wave + 2.0) / 4.0 + WAKE_LIGHT * wake.min(1.0);
    // Few shades and a slow drift: a cell changes only when its shade steps, which keeps the bytes
    // sent to tmux each frame to the fish and ripples.
    let mix = (mix * WATER_SHADES).round() / WATER_SHADES;
    let channel = |deep: f32, light: f32| (deep + (light - deep) * mix).round() as u8;
    Color::Rgb(
        channel(WATER_DEEP.0, WATER_LIGHT.0),
        channel(WATER_DEEP.1, WATER_LIGHT.1),
        channel(WATER_DEEP.2, WATER_LIGHT.2),
    )
}

/// Adds `ripple`, dropping the oldest when `MAX_RIPPLES` are live.
fn add_ripple(ripples: &mut Vec<Ripple>, ripple: Ripple) {
    if ripples.len() == MAX_RIPPLES {
        ripples.remove(0);
    }
    ripples.push(ripple);
}

impl Pond {
    fn swim(&mut self, elapsed: Duration, area: Rect) {
        let cells = usize::from(area.width) * usize::from(area.height);
        let bounds = (f32::from(area.width), f32::from(area.height) * 2.0);
        if cells * 2 < self.cells || cells > self.cells * 2 {
            self.cells = cells;
            self.fish = (0..(cells / 600).clamp(3, 6))
                .map(|_| Fish::spawn(&mut self.random, bounds))
                .collect();
            self.fish
                .sort_by(|first, second| first.depth.total_cmp(&second.depth));
        }
        if area != self.area {
            self.area = area;
            self.wake = Wake::new(usize::from(area.width), usize::from(area.height) * 2);
        }
        for _ in 0..self.steps.due(elapsed, STEP) {
            self.clock += f64::from(STEP);
            let now = self.clock;
            // All fish steer off the same moment.
            let neighbours = self.fish.clone();
            let pointer = self.pointer.noticed(self.clock);
            for fish in &mut self.fish {
                let head = fish.head();
                let attractions = Attractions {
                    food: self.pellets.iter().map(|pellet| pellet.position).min_by(
                        |first, second| distance(head, *first).total_cmp(&distance(head, *second)),
                    ),
                    pointer,
                };
                let was_bursting = fish.bursting;
                fish.swim(STEP, &neighbours, bounds, attractions, &mut self.random);
                let head = fish.head();
                if fish.bursting && !was_bursting {
                    add_ripple(
                        &mut self.ripples,
                        Ripple {
                            center: head,
                            born: now,
                            strength: BURST_RIPPLE,
                        },
                    );
                }
                if let Some(eaten) = self
                    .pellets
                    .iter()
                    .position(|pellet| distance(head, pellet.position) <= EAT_RANGE)
                {
                    // `remove`, not `swap_remove`: the oldest stay first, for the cap to drop.
                    self.pellets.remove(eaten);
                    add_ripple(
                        &mut self.ripples,
                        Ripple {
                            center: head,
                            born: now,
                            strength: EAT_RIPPLE,
                        },
                    );
                }
                self.wake.deposit(head, WAKE_DEPOSIT);
            }
            self.wake.step();
            self.ripples.retain(|ripple| ripple.alive(now));
            self.pellets.retain(|pellet| pellet.alive(now));
        }
    }
}

impl Backdrop for Pond {
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
        self.swim(elapsed, area);
        let seconds = elapsed.as_secs_f64();
        let mut pixels = Pixels::new(area.width, area.height);
        for y in 0..i32::from(pixels.height()) {
            for x in 0..i32::from(pixels.width()) {
                let wake = self.wake.energy(x as usize, y as usize);
                pixels.set(x, y, water(x, y, seconds, wake));
            }
        }
        for fish in &self.fish {
            fish.draw(seconds, &mut pixels);
        }
        pixels.render(
            area,
            text,
            palette(Color::Rgb(0, 0, 0)),
            TEXT_CEILING,
            buffer,
        );
        for ripple in &self.ripples {
            ripple.draw(self.clock, area, text, buffer);
        }
        for pellet in &self.pellets {
            pellet.draw(self.clock, area, text, buffer);
        }
        draw_logo(logo, LOGO_WHITE, LOGO_WHITE, buffer);
    }

    fn mouse(&mut self, event: MouseEvent) {
        let Some(point) = pixel_of(self.area, event) else {
            return;
        };
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let count = self.random.usize(3..=5);
                for _ in 0..count {
                    let angle = self.random.f32() * std::f32::consts::TAU;
                    let reach = self.random.f32() * CLICK_SCATTER;
                    self.pellets.push(Pellet {
                        position: (point.0 + reach * angle.cos(), point.1 + reach * angle.sin()),
                        born: self.clock,
                    });
                }
                let excess = self.pellets.len().saturating_sub(MAX_PELLETS);
                self.pellets.drain(..excess);
                add_ripple(
                    &mut self.ripples,
                    Ripple {
                        center: point,
                        born: self.clock,
                        strength: 1.0,
                    },
                );
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

    fn seeded() -> Pond {
        Pond {
            random: Rng::with_seed(7),
            ..Pond::default()
        }
    }

    /// A seeded pond painted once at 80x24, with its school spawned but not yet moved.
    fn painted() -> (Pond, Rect) {
        let mut pond = seeded();
        let area = Rect::new(0, 0, 80, 24);
        let (logo, _) = layout(area, pond.logo_size());
        let blank = Buffer::empty(area);
        pond.paint(Duration::ZERO, area, logo, &blank, &mut Buffer::empty(area));
        (pond, area)
    }

    /// Paints every 50 ms of `seconds`.
    fn run(pond: &mut Pond, area: Rect, seconds: u64) {
        let (logo, _) = layout(area, pond.logo_size());
        let blank = Buffer::empty(area);
        let mut buffer = Buffer::empty(area);
        for tick in 1..=seconds * 20 {
            pond.paint(
                Duration::from_millis(tick * 50),
                area,
                logo,
                &blank,
                &mut buffer,
            );
        }
    }

    #[test]
    fn a_click_drops_food_and_a_ripple() {
        let (mut pond, _) = painted();
        pond.mouse(click(10, 5));
        assert!(
            (3..=5).contains(&pond.pellets.len()),
            "{}",
            pond.pellets.len()
        );
        for pellet in &pond.pellets {
            assert!(distance(pellet.position, (10.0, 11.0)) <= 3.0);
        }
        assert_eq!(pond.ripples.len(), 1);
    }

    #[test]
    fn ripples_are_capped_dropping_the_oldest() {
        let (mut pond, _) = painted();
        for column in 0..50 {
            pond.mouse(click(column, 5));
        }
        assert_eq!(pond.ripples.len(), MAX_RIPPLES);
        assert_eq!(pond.ripples[MAX_RIPPLES - 1].center, (49.0, 11.0));
        assert_eq!(
            pond.ripples[0].center,
            (f32::from(50 - MAX_RIPPLES as u16), 11.0)
        );
    }

    #[test]
    fn clicks_are_capped() {
        let (mut pond, _) = painted();
        for _ in 0..30 {
            pond.mouse(click(10, 5));
        }
        pond.mouse(click(60, 5));
        assert_eq!(pond.pellets.len(), MAX_PELLETS);
        assert!(distance(pond.pellets[MAX_PELLETS - 1].position, (60.0, 11.0)) <= 3.0);
        assert!(distance(pond.pellets[0].position, (10.0, 11.0)) <= 3.0);
    }

    #[test]
    fn food_gets_eaten() {
        let (mut pond, area) = painted();
        pond.fish.truncate(1);
        let head = pond.fish[0].head();
        let (centre_x, centre_y) = (40.0 - head.0, 24.0 - head.1);
        let towards_centre = 10.0 / centre_x.hypot(centre_y);
        pond.pellets.push(Pellet {
            position: (
                head.0 + centre_x * towards_centre,
                head.1 + centre_y * towards_centre,
            ),
            born: 0.0,
        });
        run(&mut pond, area, 10);
        assert!(pond.pellets.is_empty());
    }

    #[test]
    fn a_curious_fish_circles_the_pointer() {
        let (mut pond, area) = painted();
        pond.fish.truncate(1);
        let head = pond.fish[0].head();
        let pointer = ((head.0 + 40.0) / 2.0, (head.1 + 24.0) / 2.0);
        let (logo, _) = layout(area, pond.logo_size());
        let blank = Buffer::empty(area);
        let mut farthest: f32 = 0.0;
        for tick in 1..=200 {
            pond.pointer.moved(pointer, pond.clock);
            pond.paint(
                Duration::from_millis(tick * 50),
                area,
                logo,
                &blank,
                &mut Buffer::empty(area),
            );
            if tick > 100 {
                farthest = farthest.max(distance(pond.fish[0].head(), pointer));
            }
        }
        assert!(farthest < 14.0, "{farthest}");
    }

    #[test]
    fn curiosity_fades_after_the_pointer_goes_quiet() {
        let (mut pond, area) = painted();
        let moved = MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(10, 5)
        };
        pond.mouse(moved);
        assert_eq!(pond.pointer.noticed(pond.clock), Some((10.0, 11.0)));
        run(&mut pond, area, 4);
        assert_eq!(pond.pointer.noticed(pond.clock), None);
        pond.mouse(moved);
        assert_eq!(pond.pointer.noticed(pond.clock), Some((10.0, 11.0)));
    }

    #[test]
    fn events_outside_the_pond_are_ignored() {
        let (mut pond, _) = painted();
        pond.mouse(click(500, 500));
        pond.mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            ..click(80, 0)
        });
        assert!(pond.pellets.is_empty());
        assert!(pond.ripples.is_empty());
        assert_eq!(pond.pointer.noticed(pond.clock), None);
    }

    #[test]
    fn ripples_and_pellets_expire() {
        let (mut pond, area) = painted();
        pond.fish.clear();
        pond.mouse(click(10, 5));
        run(&mut pond, area, 20);
        assert!(pond.pellets.is_empty());
        assert!(pond.ripples.is_empty());
    }

    #[test]
    fn glyphs_skip_text_cells_and_show_between_them() {
        let (mut pond, area) = painted();
        let (logo, content) = layout(area, pond.logo_size());
        // Home's text: every other row of the content area is a full line.
        let mut text = Buffer::empty(area);
        for row in (content.top()..content.bottom()).step_by(2) {
            text.set_string(
                content.x,
                row,
                "x".repeat(usize::from(content.width)),
                Style::new(),
            );
        }
        let centre = (
            f32::from(content.x + content.width / 2),
            2.0 * f32::from(content.y + content.height / 2),
        );
        pond.ripples.push(Ripple {
            center: centre,
            born: -2.5,
            strength: 1.0,
        });
        pond.pellets.push(Pellet {
            position: centre,
            born: 0.0,
        });
        let mut buffer = Buffer::empty(area);
        pond.paint(Duration::ZERO, area, logo, &text, &mut buffer);
        let glyphs_on = |row: u16| {
            (content.left()..content.right())
                .filter(|&column| {
                    ["-", "|", "/", "\\", "·"].contains(&buffer[(column, row)].symbol())
                })
                .count()
        };
        for row in (content.top()..content.bottom()).step_by(2) {
            assert!(
                (content.left()..content.right())
                    .all(|column| buffer[(column, row)].symbol() == " "),
                "row {row}"
            );
        }
        let between: usize = (content.top() + 1..content.bottom())
            .step_by(2)
            .map(glyphs_on)
            .sum();
        assert!(
            between > 0,
            "the ring shows on the blank rows below the logo"
        );
    }

    #[test]
    fn the_pond_paints_any_area_without_panicking() {
        let mut pond = Pond::default();
        let (logo_width, logo_height) = pond.logo_size();
        for (width, height) in [(0, 0), (1, 1), (80, 24), (250, 70)] {
            let area = Rect::new(0, 0, width, height);
            let boxes = [
                None,
                (width >= logo_width && height >= logo_height)
                    .then(|| Rect::new(0, 0, logo_width, logo_height)),
            ];
            for logo in boxes {
                let mut buffer = Buffer::empty(area);
                pond.paint(
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
    fn stays_within_the_budget() {
        assert_within_budget(&mut seeded());
    }

    #[test]
    fn nothing_bright_under_text() {
        assert_nothing_bright_under_text(&mut seeded(), TEXT_CEILING);
    }

    #[test]
    fn no_boxes_under_text() {
        assert_no_boxes_under_text(&mut seeded());
    }

    /// The shimmer as first written, worked out wholly in f64 so it is exact at any age.
    fn original_water(x: i32, y: i32, seconds: f64) -> Color {
        let (x, y) = (f64::from(x), f64::from(y));
        let wave = (x * 0.21 + y * 0.13 + seconds * 0.02).sin()
            + (x * 0.07 - y * 0.19 - seconds * 0.013).sin();
        let shades = f64::from(WATER_SHADES);
        let mix = (((wave + 2.0) / 4.0 * shades).round() / shades) as f32;
        let channel = |deep: f32, light: f32| (deep + (light - deep) * mix).round() as u8;
        Color::Rgb(
            channel(WATER_DEEP.0, WATER_LIGHT.0),
            channel(WATER_DEEP.1, WATER_LIGHT.1),
            channel(WATER_DEEP.2, WATER_LIGHT.2),
        )
    }

    #[test]
    fn the_shimmer_keeps_its_original_look_at_any_age() {
        let month = 30.0 * 86_400.0;
        for seconds in [0.0, 3.0, 1000.0, month, month + 3.0] {
            let differ = (0..140)
                .flat_map(|y| (0..250).map(move |x| (x, y)))
                .filter(|&(x, y)| water(x, y, seconds, 0.0) != original_water(x, y, seconds))
                .count();
            // A pixel right on a shade boundary may round either way; a changed pattern moves
            // thousands.
            assert!(
                differ <= 35,
                "{differ} of 35000 pixels differ at {seconds} s"
            );
        }
    }

    #[test]
    fn a_long_gap_is_clamped() {
        let mut pond = Pond::default();
        let area = Rect::new(0, 0, 80, 24);
        let blank = Buffer::empty(area);
        let mut buffer = Buffer::empty(area);
        pond.paint(Duration::ZERO, area, None, &blank, &mut buffer);
        pond.paint(Duration::from_secs(3600), area, None, &blank, &mut buffer);
        assert!(pond.steps.taken <= (MAX_GAP / STEP).ceil() as usize);
    }

    #[test]
    fn a_shrinking_area_never_panics() {
        let mut pond = Pond::default();
        for (second, (width, height)) in [(250, 70), (10, 3), (0, 0), (80, 24)]
            .into_iter()
            .enumerate()
        {
            let area = Rect::new(0, 0, width, height);
            let (logo, content) = layout(area, pond.logo_size());
            let mut text = Buffer::empty(area);
            text.set_style(content, Style::new().bg(Color::Red));
            let mut buffer = Buffer::empty(area);
            pond.paint(
                Duration::from_secs(second as u64),
                area,
                logo,
                &text,
                &mut buffer,
            );
        }
    }
}
