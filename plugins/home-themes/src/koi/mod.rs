//! The koi pond: a dark blue-violet water with a slow shimmer, and the logo floating on it.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::MouseEvent;
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, LOGO, Pixels, palette};

const WATER_DEEP: (f32, f32, f32) = (14.0, 16.0, 38.0);
const WATER_LIGHT: (f32, f32, f32) = (30.0, 40.0, 82.0);
const LOGO_WHITE: Color = Color::Rgb(255, 244, 226);

pub fn pond() -> Box<dyn Backdrop> {
    Box::new(Pond)
}

struct Pond;

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
        let seconds = elapsed.as_secs_f32();
        let mut pixels = Pixels::new(area.width, area.height);
        for y in 0..i32::from(pixels.height()) {
            for x in 0..i32::from(pixels.width()) {
                pixels.set(x, y, water(x, y, seconds));
            }
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
    use super::*;

    #[test]
    fn the_pond_paints_any_area_without_panicking() {
        let mut pond = Pond;
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
}
