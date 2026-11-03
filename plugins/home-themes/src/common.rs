//! Helpers more than one theme uses.

use ratatui::style::Color;

/// A pseudo-random value in 0..1 for each lattice point.
fn lattice(x: i32, y: i32, seed: u32) -> f32 {
    let mut hash = (x as u32).wrapping_mul(0x27d4_eb2d)
        ^ (y as u32).wrapping_mul(0x1656_67b1)
        ^ seed.wrapping_mul(0x9e37_79b9);
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(0x2c1b_3c6d);
    hash ^= hash >> 13;
    (hash & 0xff_ffff) as f32 / 0xff_ffff as f32
}

/// Smooth noise in 0..1: lattice values blended with a smoothstep between them.
pub fn value_noise(x: f32, y: f32, seed: u32) -> f32 {
    let (left, top) = (x.floor(), y.floor());
    let smooth = |fraction: f32| fraction * fraction * (3.0 - 2.0 * fraction);
    let (across, down) = (smooth(x - left), smooth(y - top));
    let (column, row) = (left as i32, top as i32);
    let corner = |dx: i32, dy: i32| lattice(column.wrapping_add(dx), row.wrapping_add(dy), seed);
    let upper = corner(0, 0) + (corner(1, 0) - corner(0, 0)) * across;
    let lower = corner(0, 1) + (corner(1, 1) - corner(0, 1)) * across;
    upper + (lower - upper) * down
}

/// `shades` evenly spaced steps from `dark` to `light`; `amount` 0..1 is rounded to a step.
pub fn quantised(dark: Color, light: Color, amount: f32, shades: u8) -> Color {
    let (Color::Rgb(dark_red, dark_green, dark_blue), Color::Rgb(red, green, blue)) = (dark, light)
    else {
        return dark;
    };
    let steps = f32::from(shades - 1);
    let mix = (amount.clamp(0.0, 1.0) * steps).round() / steps;
    let channel = |from: u8, to: u8| {
        (f32::from(from) + (f32::from(to) - f32::from(from)) * mix).round() as u8
    };
    Color::Rgb(
        channel(dark_red, red),
        channel(dark_green, green),
        channel(dark_blue, blue),
    )
}

#[cfg(test)]
pub mod tests {
    use std::time::Duration;

    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use tx_home::backdrop::{Backdrop, is_text, layout};

    use super::*;

    const MEAN_BUDGET: usize = 400;
    const BURST_BUDGET: usize = 1200;
    const SELECTED: Color = Color::Rgb(90, 70, 160);

    fn luminance(color: Color) -> f32 {
        let Color::Rgb(red, green, blue) = color else {
            return 0.0;
        };
        0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue)
    }

    /// What home draws below the logo: a header, list rows of varying length (one a solid
    /// selection bar) and a footer of keys.
    fn home_like_text(area: Rect, content: Rect) -> Buffer {
        let mut text = Buffer::empty(area);
        let width = usize::from(content.width);
        let mut line = |row: u16, words: &str, style: Style| {
            let mut words = words.to_owned();
            words.truncate(width);
            text.set_string(content.x, content.y + row, words, style);
        };
        line(0, " tx-ide · ~/projects · 3 running", Style::new());
        for entry in 0..6u16 {
            let words = format!(
                "  ● project-{entry}   {} agents   {}",
                entry + 1,
                "session notes ".repeat(usize::from(entry) + 1)
            );
            line(2 + entry, &words, Style::new());
        }
        line(
            content.height - 1,
            " enter open · n new · / find · b theme · q quit",
            Style::new(),
        );
        text.set_style(
            Rect::new(content.x, content.y + 4, content.width, 1),
            Style::new().bg(SELECTED),
        );
        text
    }

    /// The frame home shows: `text` over what the backdrop painted, keeping its background where
    /// home set none.
    fn composite(painted: &Buffer, text: &Buffer) -> Buffer {
        let mut shown = painted.clone();
        for position in shown.area.positions() {
            let drawn = &text[position];
            if is_text(drawn) {
                let background = shown[position].bg;
                shown[position] = drawn.clone();
                if drawn.bg == Color::Reset {
                    shown[position].bg = background;
                }
            }
        }
        shown
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Paints `backdrop` for 2 simulated s at its fps on 250x70 (logo box from `layout`, text layer
    /// = a home-like layer: 8 rows of text), asserts mean changed cells/frame ≤ 400; then a frame
    /// after a left click + a hover move asserts ≤ 1200.
    pub fn assert_within_budget(backdrop: &mut dyn Backdrop) {
        let area = Rect::new(0, 0, 250, 70);
        let (logo, content) = layout(area, backdrop.logo_size());
        let text = home_like_text(area, content);
        let frame_length = 1000 / backdrop.frames_per_second();
        let paint = |backdrop: &mut dyn Backdrop, frame: u64| {
            let mut buffer = Buffer::empty(area);
            backdrop.paint(
                Duration::from_millis(frame * frame_length),
                area,
                logo,
                &text,
                &mut buffer,
            );
            composite(&buffer, &text)
        };
        let mut previous = paint(backdrop, 0);
        let frames = 2 * backdrop.frames_per_second();
        let mut changed = 0;
        for frame in 1..=frames {
            let shown = paint(backdrop, frame);
            changed += previous.diff(&shown).len();
            previous = shown;
        }
        let mean = changed / frames as usize;
        assert!(mean <= MEAN_BUDGET, "{mean} changed cells per frame");
        backdrop.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 60, 30));
        backdrop.mouse(mouse(MouseEventKind::Moved, 64, 33));
        let shown = paint(backdrop, frames + 1);
        let burst = previous.diff(&shown).len();
        assert!(
            burst <= BURST_BUDGET,
            "{burst} changed cells after a click and a hover"
        );
    }

    /// Paints a frame with the same home-like text layer; asserts every text cell's bg
    /// luminance ≤ `ceiling`'s.
    pub fn assert_nothing_bright_under_text(backdrop: &mut dyn Backdrop, ceiling: Color) {
        let area = Rect::new(0, 0, 250, 70);
        let (logo, content) = layout(area, backdrop.logo_size());
        let text = home_like_text(area, content);
        // Clicks and a hover on the text itself, so whatever they light up lands under it.
        backdrop.mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            content.x + 10,
            content.y + 2,
        ));
        backdrop.mouse(mouse(MouseEventKind::Moved, content.x + 20, content.y + 4));
        for frame in 0..30 {
            let mut buffer = Buffer::empty(area);
            backdrop.paint(
                Duration::from_millis(frame * 100),
                area,
                logo,
                &text,
                &mut buffer,
            );
            for position in area
                .positions()
                .filter(|position| is_text(&text[*position]))
            {
                let background = buffer[position].bg;
                assert!(
                    luminance(background) <= luminance(ceiling) + 1.0,
                    "{background:?} under the text at {position:?}"
                );
            }
        }
    }

    #[test]
    fn quantised_snaps_to_steps() {
        let (black, white) = (Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255));
        assert_eq!(quantised(black, white, 0.1, 3), black);
        assert_eq!(quantised(black, white, 0.4, 3), Color::Rgb(128, 128, 128));
        assert_eq!(quantised(black, white, 0.5, 4), Color::Rgb(170, 170, 170));
        assert_eq!(quantised(black, white, 1.0, 4), white);
        assert_eq!(quantised(black, white, 1.5, 4), white);
        assert_eq!(quantised(black, white, -1.0, 4), black);
    }

    #[test]
    fn value_noise_is_smooth_and_bounded() {
        for seed in [0, 7, 99] {
            for step in 0..4000 {
                let x = step as f32 * 0.01 - 20.0;
                let y = x * 0.37 + 3.0;
                let here = value_noise(x, y, seed);
                let next = value_noise(x + 0.01, y, seed);
                assert!((0.0..=1.0).contains(&here), "{here}");
                assert!((here - next).abs() < 0.05, "{here} then {next} at {x}");
            }
        }
    }
}
