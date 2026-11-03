//! The water's surface: ripples, food pellets and the wake the fish leave. Positions are in
//! pixels relative to the pond's area; ripples and pellets are drawn as glyph cells over it.

use std::f32::consts::{FRAC_PI_8, PI, TAU};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_home::backdrop::{dim, is_text, palette};

/// Pixels per second a ripple's ring grows.
const RIPPLE_SPEED: f32 = 9.0;
const PELLET_LIFETIME: f32 = 12.0;
const WAKE_DECAY: f32 = 0.98;

const RIPPLE_LIGHT: Color = Color::Rgb(112, 130, 196);
const PELLET: Color = Color::Rgb(222, 158, 82);

pub struct Ripple {
    pub centre: (f32, f32),
    pub born: f64,
    pub strength: f32,
}

pub struct Pellet {
    pub position: (f32, f32),
    pub born: f64,
}

/// The cell over pixel `point`, unless it is outside `area` or one of home's text cells.
fn surface_cell(area: Rect, text: &Buffer, point: (f32, f32)) -> Option<(u16, u16)> {
    let (x, y) = (point.0.floor(), (point.1 / 2.0).floor());
    if x < 0.0 || y < 0.0 || x >= f32::from(area.width) || y >= f32::from(area.height) {
        return None;
    }
    let cell = (area.x + x as u16, area.y + y as u16);
    (!is_text(&text[cell])).then_some(cell)
}

/// The glyph along a ring's tangent at `angle` around its centre (y grows downwards).
fn tangent(angle: f32) -> char {
    let direction = (angle + PI / 2.0).rem_euclid(PI);
    match direction {
        direction if direction < FRAC_PI_8 => '-',
        direction if direction < 3.0 * FRAC_PI_8 => '\\',
        direction if direction < 5.0 * FRAC_PI_8 => '|',
        direction if direction < 7.0 * FRAC_PI_8 => '/',
        _ => '-',
    }
}

impl Ripple {
    /// A weak ring dies small: a ring repaints its whole circumference every frame.
    fn lifetime(&self) -> f32 {
        0.6 + 2.6 * self.strength
    }

    pub fn alive(&self, now: f64) -> bool {
        ((now - self.born) as f32) < self.lifetime()
    }

    pub fn draw(&self, now: f64, area: Rect, text: &Buffer, buffer: &mut Buffer) {
        let age = (now - self.born) as f32;
        let radius = RIPPLE_SPEED * age;
        let brightness = (0.4 + 0.6 * self.strength) * (1.0 - age / self.lifetime());
        let color = palette(dim(RIPPLE_LIGHT, brightness));
        // About one sample per pixel of circumference.
        let samples = (TAU * radius).ceil().max(8.0) as usize;
        for sample in 0..samples {
            let angle = TAU * sample as f32 / samples as f32;
            let point = (
                self.centre.0 + radius * angle.cos(),
                self.centre.1 + radius * angle.sin(),
            );
            if let Some(cell) = surface_cell(area, text, point) {
                buffer[cell].set_char(tangent(angle)).set_fg(color);
            }
        }
    }
}

impl Pellet {
    pub fn alive(&self, now: f64) -> bool {
        ((now - self.born) as f32) < PELLET_LIFETIME
    }

    /// Darkens as it sinks.
    pub fn draw(&self, now: f64, area: Rect, text: &Buffer, buffer: &mut Buffer) {
        if let Some(cell) = surface_cell(area, text, self.position) {
            let sunk = (now - self.born) as f32 / PELLET_LIFETIME;
            buffer[cell]
                .set_char('·')
                .set_fg(palette(dim(PELLET, 1.0 - 0.7 * sunk)));
        }
    }
}

/// Energy per pixel, deposited by the fish and spreading out as it fades.
#[derive(Default)]
pub struct Wake {
    width: usize,
    height: usize,
    energy: Vec<f32>,
    /// The next step's energy, kept to avoid allocating every step.
    scratch: Vec<f32>,
}

impl Wake {
    /// `width` by `height` pixels.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            energy: vec![0.0; width * height],
            scratch: Vec::with_capacity(width * height),
        }
    }

    pub fn deposit(&mut self, point: (f32, f32), amount: f32) {
        let (x, y) = (point.0.floor(), point.1.floor());
        if x >= 0.0 && y >= 0.0 && (x as usize) < self.width && (y as usize) < self.height {
            self.energy[y as usize * self.width + x as usize] += amount;
        }
    }

    pub fn energy(&self, x: usize, y: usize) -> f32 {
        self.energy[y * self.width + x]
    }

    /// A 4-neighbour blur and a decay.
    pub fn step(&mut self) {
        let (width, energy) = (self.width, &self.energy);
        self.scratch.clear();
        self.scratch.extend((0..energy.len()).map(|index| {
            let x = index % width;
            let left = if x > 0 { energy[index - 1] } else { 0.0 };
            let right = if x + 1 < width {
                energy[index + 1]
            } else {
                0.0
            };
            let up = index.checked_sub(width).map_or(0.0, |above| energy[above]);
            let down = energy.get(index + width).copied().unwrap_or(0.0);
            WAKE_DECAY * (energy[index] + left + right + up + down) / 5.0
        }));
        std::mem::swap(&mut self.energy, &mut self.scratch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ripple_glyphs_follow_the_ring() {
        assert_eq!(tangent(0.0), '|');
        assert_eq!(tangent(PI / 2.0), '-');
        assert_eq!(tangent(PI / 4.0), '/');
        assert_eq!(tangent(-PI / 4.0), '\\');
    }

    #[test]
    fn a_wake_keeps_half_its_energy_for_half_a_second() {
        let mut wake = Wake::new(41, 41);
        wake.deposit((20.5, 20.5), 1.0);
        for _ in 0..30 {
            wake.step();
        }
        let left: f32 = wake.energy.iter().sum();
        assert!(left > 0.5, "{left}");
    }

    #[test]
    fn the_wake_spreads_and_fades() {
        let mut wake = Wake::new(5, 5);
        wake.deposit((2.5, 2.5), 1.0);
        wake.step();
        assert!(wake.energy(2, 1) > 0.0);
        assert!(wake.energy(2, 2) < 1.0);
        for _ in 0..100 {
            wake.step();
        }
        assert!(wake.energy(2, 2) < 0.001);
    }
}
