//! The koi pond: a dark blue-violet water with a slow shimmer, koi swimming in it, ripples and
//! food on its surface, and the logo floating on top.

mod fish;
mod water;

use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, LOGO, Pixels, palette};

use fish::{Attractions, Fish, distance};
use water::{Pellet, Ripple, Wake};

const WATER_DEEP: (f32, f32, f32) = (14.0, 16.0, 38.0);
const WATER_LIGHT: (f32, f32, f32) = (30.0, 40.0, 82.0);
const LOGO_WHITE: Color = Color::Rgb(255, 244, 226);
const STEP: f32 = 1.0 / 60.0;
/// The most simulated time one paint catches up on, so a long pause doesn't replay hours.
const MAX_GAP: f32 = 0.25;
/// How close a head must come to a pellet to eat it.
const EAT_RANGE: f32 = 1.5;
const EAT_RIPPLE: f32 = 0.4;
const BURST_RIPPLE: f32 = 0.25;
const CLICK_SCATTER: f32 = 3.0;
const WAKE_DEPOSIT: f32 = 0.5;
/// How far a full wake moves the water towards (and past) `WATER_LIGHT`.
const WAKE_LIGHT: f32 = 0.6;
/// Seconds a still pointer keeps the fish curious: a terminal never says the mouse has left.
const POINTER_MEMORY: f32 = 3.0;

pub fn pond() -> Box<dyn Backdrop> {
    Box::<Pond>::default()
}

#[derive(Default)]
struct Pond {
    fish: Vec<Fish>,
    random: Rng,
    last: Option<Duration>,
    remainder: f32,
    /// The area, in cells, the fish were spawned for.
    cells: usize,
    /// The last painted area, which mouse events are relative to.
    area: Rect,
    /// Simulated seconds.
    clock: f32,
    ripples: Vec<Ripple>,
    pellets: Vec<Pellet>,
    /// In pixels, and the `clock` it last moved at.
    pointer: Option<((f32, f32), f32)>,
    wake: Wake,
    #[cfg(test)]
    steps: usize,
}

/// `wake`: the wake's energy here, lightening the water.
fn water(x: i32, y: i32, seconds: f32, wake: f32) -> Color {
    let (x, y) = (x as f32, y as f32);
    let wave =
        (x * 0.21 + y * 0.13 + seconds * 0.6).sin() + (x * 0.07 - y * 0.19 - seconds * 0.4).sin();
    let mix = (wave + 2.0) / 4.0 + WAKE_LIGHT * wake.min(1.0);
    let channel = |deep: f32, light: f32| (deep + (light - deep) * mix).round() as u8;
    Color::Rgb(
        channel(WATER_DEEP.0, WATER_LIGHT.0),
        channel(WATER_DEEP.1, WATER_LIGHT.1),
        channel(WATER_DEEP.2, WATER_LIGHT.2),
    )
}

impl Pond {
    fn noticed_pointer(&self) -> Option<(f32, f32)> {
        self.pointer
            .filter(|(_, moved)| self.clock - moved <= POINTER_MEMORY)
            .map(|(point, _)| point)
    }

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
        let gap = self
            .last
            .map_or(Duration::ZERO, |last| elapsed.saturating_sub(last));
        self.last = Some(elapsed);
        self.remainder += gap.as_secs_f32().min(MAX_GAP);
        while self.remainder >= STEP {
            self.remainder -= STEP;
            self.clock += STEP;
            let now = self.clock;
            // All fish steer off the same moment.
            let neighbours = self.fish.clone();
            let pointer = self.noticed_pointer();
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
                    self.ripples.push(Ripple {
                        center: head,
                        born: now,
                        strength: BURST_RIPPLE,
                    });
                }
                if let Some(eaten) = self
                    .pellets
                    .iter()
                    .position(|pellet| distance(head, pellet.position) <= EAT_RANGE)
                {
                    self.pellets.swap_remove(eaten);
                    self.ripples.push(Ripple {
                        center: head,
                        born: now,
                        strength: EAT_RIPPLE,
                    });
                }
                self.wake.deposit(head, WAKE_DEPOSIT);
            }
            self.wake.step();
            self.ripples.retain(|ripple| ripple.alive(now));
            self.pellets.retain(|pellet| pellet.alive(now));
            #[cfg(test)]
            {
                self.steps += 1;
            }
        }
    }
}

impl Backdrop for Pond {
    fn frames_per_second(&self) -> u64 {
        20
    }

    fn logo_size(&self) -> (u16, u16) {
        (LOGO[0].chars().count() as u16, LOGO.len() as u16)
    }

    fn paint(
        &mut self,
        elapsed: Duration,
        area: Rect,
        logo: Option<Rect>,
        text: Rect,
        buffer: &mut Buffer,
    ) {
        self.swim(elapsed, area);
        let seconds = elapsed.as_secs_f32();
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
        pixels.render(area, text, palette(Color::Rgb(0, 0, 0)), buffer);
        for ripple in &self.ripples {
            ripple.draw(self.clock, area, text, buffer);
        }
        for pellet in &self.pellets {
            pellet.draw(self.clock, area, text, buffer);
        }
        if let Some(logo) = logo {
            for (row, line) in LOGO.iter().enumerate() {
                for (column, glyph) in line.chars().enumerate().filter(|(_, glyph)| *glyph != ' ') {
                    buffer[(logo.x + column as u16, logo.y + row as u16)]
                        .set_char(glyph)
                        .set_fg(palette(LOGO_WHITE));
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
                let count = self.random.usize(3..=5);
                for _ in 0..count {
                    let angle = self.random.f32() * std::f32::consts::TAU;
                    let reach = self.random.f32() * CLICK_SCATTER;
                    self.pellets.push(Pellet {
                        position: (point.0 + reach * angle.cos(), point.1 + reach * angle.sin()),
                        born: self.clock,
                    });
                }
                self.ripples.push(Ripple {
                    center: point,
                    born: self.clock,
                    strength: 1.0,
                });
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
    use tx_home::backdrop::layout;

    use super::*;

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A seeded pond painted once at 80x24, with its school spawned but not yet moved.
    fn painted() -> (Pond, Rect) {
        let mut pond = Pond {
            random: Rng::with_seed(7),
            ..Pond::default()
        };
        let area = Rect::new(0, 0, 80, 24);
        let (logo, text) = layout(area, pond.logo_size());
        pond.paint(Duration::ZERO, area, logo, text, &mut Buffer::empty(area));
        (pond, area)
    }

    /// Paints every 50 ms of `seconds`.
    fn run(pond: &mut Pond, area: Rect, seconds: u64) {
        let (logo, text) = layout(area, pond.logo_size());
        let mut buffer = Buffer::empty(area);
        for tick in 1..=seconds * 20 {
            pond.paint(
                Duration::from_millis(tick * 50),
                area,
                logo,
                text,
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
        let (logo, text) = layout(area, pond.logo_size());
        let mut farthest: f32 = 0.0;
        for tick in 1..=200 {
            pond.pointer = Some((pointer, pond.clock));
            pond.paint(
                Duration::from_millis(tick * 50),
                area,
                logo,
                text,
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
        assert_eq!(pond.noticed_pointer(), Some((10.0, 11.0)));
        run(&mut pond, area, 4);
        assert_eq!(pond.noticed_pointer(), None);
        pond.mouse(moved);
        assert_eq!(pond.noticed_pointer(), Some((10.0, 11.0)));
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
        assert_eq!(pond.pointer, None);
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
    fn nothing_is_drawn_as_glyphs_in_the_text_region() {
        let (mut pond, area) = painted();
        let (logo, text) = layout(area, pond.logo_size());
        let centre = (
            f32::from(text.x + text.width / 2),
            2.0 * f32::from(text.y + text.height / 2),
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
        pond.paint(Duration::ZERO, area, logo, text, &mut buffer);
        for row in text.top()..text.bottom() {
            for column in text.left()..text.right() {
                assert_eq!(buffer[(column, row)].symbol(), " ", "({column}, {row})");
            }
        }
        let ripple_glyphs = buffer
            .content()
            .iter()
            .filter(|cell| ["-", "|", "/", "\\"].contains(&cell.symbol()))
            .count();
        assert!(ripple_glyphs > 0, "the ring reaches outside the text");
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
                    Rect::default(),
                    &mut buffer,
                );
            }
        }
    }

    #[test]
    fn a_long_gap_is_clamped() {
        let mut pond = Pond::default();
        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::empty(area);
        pond.paint(Duration::ZERO, area, None, Rect::default(), &mut buffer);
        pond.paint(
            Duration::from_secs(3600),
            area,
            None,
            Rect::default(),
            &mut buffer,
        );
        assert!(pond.steps <= (MAX_GAP / STEP).ceil() as usize);
    }

    #[test]
    fn a_shrinking_area_never_panics() {
        let mut pond = Pond::default();
        for (second, (width, height)) in [(250, 70), (10, 3), (0, 0), (80, 24)]
            .into_iter()
            .enumerate()
        {
            let area = Rect::new(0, 0, width, height);
            let (logo, text) = layout(area, pond.logo_size());
            let mut buffer = Buffer::empty(area);
            pond.paint(
                Duration::from_secs(second as u64),
                area,
                logo,
                text,
                &mut buffer,
            );
        }
    }
}
