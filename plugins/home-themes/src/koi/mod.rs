//! The koi pond: a dark blue-violet water with a slow shimmer, koi swimming in it, and the logo
//! floating on top.

mod fish;

use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::MouseEvent;
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, LOGO, Pixels, palette};

use fish::Fish;

const WATER_DEEP: (f32, f32, f32) = (14.0, 16.0, 38.0);
const WATER_LIGHT: (f32, f32, f32) = (30.0, 40.0, 82.0);
const LOGO_WHITE: Color = Color::Rgb(255, 244, 226);
const STEP: f32 = 1.0 / 60.0;
/// The most simulated time one paint catches up on, so a long pause doesn't replay hours.
const MAX_GAP: f32 = 0.25;

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
    #[cfg(test)]
    steps: usize,
}

fn water(x: i32, y: i32, seconds: f32) -> Color {
    let (x, y) = (x as f32, y as f32);
    let wave =
        (x * 0.21 + y * 0.13 + seconds * 0.6).sin() + (x * 0.07 - y * 0.19 - seconds * 0.4).sin();
    let mix = (wave + 2.0) / 4.0;
    let channel = |deep: f32, light: f32| (deep + (light - deep) * mix).round() as u8;
    Color::Rgb(
        channel(WATER_DEEP.0, WATER_LIGHT.0),
        channel(WATER_DEEP.1, WATER_LIGHT.1),
        channel(WATER_DEEP.2, WATER_LIGHT.2),
    )
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
        let gap = self
            .last
            .map_or(Duration::ZERO, |last| elapsed.saturating_sub(last));
        self.last = Some(elapsed);
        self.remainder += gap.as_secs_f32().min(MAX_GAP);
        while self.remainder >= STEP {
            self.remainder -= STEP;
            // All fish steer off the same moment.
            let neighbours = self.fish.clone();
            for fish in &mut self.fish {
                fish.swim(STEP, &neighbours, bounds, &mut self.random);
            }
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
                pixels.set(x, y, water(x, y, seconds));
            }
        }
        for fish in &self.fish {
            fish.draw(seconds, &mut pixels);
        }
        pixels.render(area, text, palette(Color::Rgb(0, 0, 0)), buffer);
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

    fn mouse(&mut self, _event: MouseEvent) {}
}

#[cfg(test)]
mod tests {
    use tx_home::backdrop::layout;

    use super::*;

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
