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
/// Non-`Rgb` inputs return `dark`.
pub fn quantised(dark: Color, light: Color, amount: f32, shades: u8) -> Color {
    assert!(
        shades >= 2,
        "quantised needs two shades at least, got {shades}"
    );
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
            panic!("{color:?} is not an Rgb colour");
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

    /// Paints `backdrop` frame by frame at its fps on 250x70 (logo box from `layout`, text layer =
    /// a home-like layer: 8 rows of text) and diffs each shown frame with the one before. After a
    /// 10 s warm-up, so periodic events are due: the mean over 2 s is ≤ 400 changed cells; after a
    /// left click + a hover, every frame of the next 1 s is ≤ 1200 and their mean ≤ 400; through
    /// 10 rapid clicks at different points and the 1 s after, every frame is ≤ 1200.
    pub fn assert_within_budget(backdrop: &mut dyn Backdrop) {
        let area = Rect::new(0, 0, 250, 70);
        let (logo, content) = layout(area, backdrop.logo_size());
        let text = home_like_text(area, content);
        let fps = backdrop.frames_per_second();
        let mut frame = 0;
        let mut previous = Buffer::empty(area);
        let mut changed = |backdrop: &mut dyn Backdrop| {
            let mut buffer = Buffer::empty(area);
            backdrop.paint(
                Duration::from_millis(frame * 1000 / fps),
                area,
                logo,
                &text,
                &mut buffer,
            );
            frame += 1;
            let shown = composite(&buffer, &text);
            let count = previous.diff(&shown).len();
            previous = shown;
            count
        };
        let mean = |counts: &[usize]| counts.iter().sum::<usize>() / counts.len();
        for _ in 0..=10 * fps {
            changed(backdrop);
        }
        let calm: Vec<usize> = (0..2 * fps).map(|_| changed(backdrop)).collect();
        assert!(
            mean(&calm) <= MEAN_BUDGET,
            "{} changed cells per frame",
            mean(&calm)
        );
        backdrop.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 60, 30));
        backdrop.mouse(mouse(MouseEventKind::Moved, 64, 33));
        let busy: Vec<usize> = (0..fps).map(|_| changed(backdrop)).collect();
        assert!(
            busy.iter().all(|&count| count <= BURST_BUDGET),
            "{busy:?} changed cells after a click and a hover"
        );
        assert!(
            mean(&busy) <= MEAN_BUDGET,
            "{} changed cells per frame in the second after a click and a hover",
            mean(&busy)
        );
        let mut clicks = Vec::new();
        for index in 0..10 {
            backdrop.mouse(mouse(
                MouseEventKind::Down(MouseButton::Left),
                20 + 21 * index,
                20 + 4 * index,
            ));
            clicks.push(changed(backdrop));
        }
        clicks.extend((0..fps).map(|_| changed(backdrop)));
        assert!(
            clicks.iter().all(|&count| count <= BURST_BUDGET),
            "{clicks:?} changed cells through rapid clicks"
        );
    }

    /// Paints with the same home-like text layer for 15 s at its fps, so periodic events show,
    /// clicking and hovering on the text every second; asserts every text cell's bg is `Rgb` with
    /// luminance ≤ `ceiling`'s.
    pub fn assert_nothing_bright_under_text(backdrop: &mut dyn Backdrop, ceiling: Color) {
        let area = Rect::new(0, 0, 250, 70);
        let (logo, content) = layout(area, backdrop.logo_size());
        let text = home_like_text(area, content);
        let fps = backdrop.frames_per_second();
        for frame in 0..15 * fps {
            if frame % fps == 1 {
                // On the text itself, so whatever a click or a hover lights up lands under it.
                let column = content.x + 10 + 7 * (frame / fps) as u16;
                backdrop.mouse(mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    column,
                    content.y + 2,
                ));
                backdrop.mouse(mouse(MouseEventKind::Moved, column + 5, content.y + 4));
            }
            let mut buffer = Buffer::empty(area);
            backdrop.paint(
                Duration::from_millis(frame * 1000 / fps),
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
                    matches!(background, Color::Rgb(..))
                        && luminance(background) <= luminance(ceiling) + 1.0,
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
            let (mut lowest, mut highest) = (f32::MAX, f32::MIN);
            for step in 0..4000 {
                let x = step as f32 * 0.01 - 20.0;
                let y = x * 0.37 + 3.0;
                let here = value_noise(x, y, seed);
                let next = value_noise(x + 0.01, y, seed);
                assert!((0.0..=1.0).contains(&here), "{here}");
                assert!((here - next).abs() < 0.05, "{here} then {next} at {x}");
                lowest = lowest.min(here);
                highest = highest.max(here);
            }
            assert!(
                highest - lowest > 0.3,
                "{lowest}..{highest} for seed {seed}"
            );
        }
    }

    #[test]
    #[should_panic(expected = "two shades")]
    fn quantised_needs_two_shades() {
        quantised(Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255), 0.5, 1);
    }

    /// When a probe floods the whole area with a bright background.
    enum Cue {
        AfterAClick,
        EveryFourSeconds,
        TwoFramesAfterAClick,
    }

    /// A theme that misbehaves on its cue, to prove the helpers catch it.
    struct Probe {
        cue: Cue,
        area: Rect,
        frames: u64,
        clicked: Option<u64>,
    }

    fn probe(cue: Cue) -> Probe {
        Probe {
            cue,
            area: Rect::default(),
            frames: 0,
            clicked: None,
        }
    }

    impl Backdrop for Probe {
        fn frames_per_second(&self) -> u64 {
            15
        }

        fn logo_size(&self) -> (u16, u16) {
            (23, 5)
        }

        fn paint(
            &mut self,
            _: Duration,
            area: Rect,
            _: Option<Rect>,
            _: &Buffer,
            buffer: &mut Buffer,
        ) {
            self.frames += 1;
            self.area = area;
            let flood = match self.cue {
                Cue::AfterAClick => self.clicked.is_some(),
                Cue::EveryFourSeconds => self.frames.is_multiple_of(60),
                Cue::TwoFramesAfterAClick => {
                    self.clicked.map(|clicked| clicked + 2) == Some(self.frames)
                }
            };
            let background = if flood {
                Color::Rgb(255, 255, 255)
            } else {
                Color::Rgb(0, 0, 0)
            };
            buffer.set_style(area, Style::new().bg(background));
        }

        fn mouse(&mut self, event: MouseEvent) {
            let inside = self
                .area
                .contains(ratatui::layout::Position::new(event.column, event.row));
            if inside && event.kind == MouseEventKind::Down(MouseButton::Left) {
                self.clicked = Some(self.frames);
            }
        }
    }

    #[test]
    #[should_panic(expected = "under the text")]
    fn the_ceiling_check_sees_what_a_click_lights_up() {
        assert_nothing_bright_under_text(&mut probe(Cue::AfterAClick), Color::Rgb(30, 30, 30));
    }

    #[test]
    #[should_panic(expected = "changed cells")]
    fn the_budget_sees_periodic_events() {
        assert_within_budget(&mut probe(Cue::EveryFourSeconds));
    }

    #[test]
    #[should_panic(expected = "changed cells")]
    fn the_budget_sees_every_frame_after_a_click() {
        assert_within_budget(&mut probe(Cue::TwoFramesAfterAClick));
    }
}
