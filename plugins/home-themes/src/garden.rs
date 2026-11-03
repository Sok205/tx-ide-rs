//! A moonless zen garden: night sand raked in alternating shades, straight except where it bends
//! round a few stones, and a leaf that drifts across now and then. A click sets a stone, a drag
//! rakes a line.

use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, Pixels};

use crate::common::{STEP, Steps, between, draw_logo, fill, logo_size, pixel_of};

const RIDGE: Color = Color::Rgb(44, 48, 64);
const FURROW: Color = Color::Rgb(34, 38, 52);
const GROOVE: Color = Color::Rgb(26, 29, 40);
const STONE_LIT: Color = Color::Rgb(92, 98, 118);
const STONE_SHADED: Color = Color::Rgb(58, 62, 80);
const LEAF: Color = Color::Rgb(118, 146, 92);
/// The lightest sand: text cells show the sand as it is.
const TEXT_CEILING: Color = RIDGE;
const LOGO_TOP: Color = Color::Rgb(222, 214, 190);
const LOGO_BOTTOM: Color = Color::Rgb(150, 148, 150);
/// Pixel rows per raked line.
const LINE_HEIGHT: i32 = 3;
/// Pixels between rings, and how far past a stone they reach: a click only repaints this disc.
const RING_SPACING: f32 = 3.0;
const RING_REACH: f32 = 11.0;
const STONE_RADII: (i32, i32) = (3, 5);
const START_STONES: (usize, usize) = (2, 3);
const MAX_STONES: usize = 6;
const MAX_STROKES: usize = 64;
const LEAF_EVERY_SECONDS: (f64, f64) = (10.0, 20.0);
const LEAF_CROSSING_SECONDS: (f64, f64) = (8.0, 10.0);

pub fn sand() -> Box<dyn Backdrop> {
    Box::<Garden>::default()
}

struct Stone {
    x: i32,
    y: i32,
    radius: i32,
}

struct Leaf {
    born: f64,
    row: f32,
    seconds: f32,
}

#[derive(Default)]
struct Garden {
    random: Rng,
    steps: Steps,
    /// The last painted area, which mouse events are relative to and the sand was raked for.
    area: Rect,
    /// One colour per pixel, row by row.
    grid: Vec<Color>,
    /// Oldest first.
    stones: Vec<Stone>,
    /// Segments a drag raked, oldest first.
    strokes: Vec<((i32, i32), (i32, i32))>,
    drag_from: Option<(i32, i32)>,
    /// The stones changed: rake the sand again before the next paint.
    dirty: bool,
    /// Simulated seconds; f64 so a garden open for days keeps its precision.
    clock: f64,
    leaf: Option<Leaf>,
    next_leaf: f64,
    #[cfg(test)]
    reflows: usize,
}

impl Garden {
    fn respawn(&mut self, area: Rect) {
        self.area = area;
        self.strokes.clear();
        self.drag_from = None;
        self.leaf = None;
        self.next_leaf = self.clock + between(&mut self.random, LEAF_EVERY_SECONDS);
        self.stones.clear();
        if !area.is_empty() {
            let count = self.random.usize(START_STONES.0..=START_STONES.1);
            for _ in 0..count {
                let point = (
                    self.random.i32(0..i32::from(area.width)),
                    self.random.i32(0..i32::from(area.height) * 2),
                );
                self.place(point);
            }
        }
        self.dirty = true;
    }

    fn place(&mut self, (x, y): (i32, i32)) {
        if self.stones.len() == MAX_STONES {
            self.stones.remove(0);
        }
        let radius = self.random.i32(STONE_RADII.0..=STONE_RADII.1);
        self.stones.push(Stone { x, y, radius });
        self.dirty = true;
    }

    /// Rakes the whole sand: straight lines, rings round each stone, then the drags on top.
    fn reflow(&mut self) {
        let (width, rows) = (i32::from(self.area.width), i32::from(self.area.height) * 2);
        self.grid.clear();
        self.grid.extend((0..rows).flat_map(|y| {
            let shade = if (y / LINE_HEIGHT) % 2 == 0 {
                RIDGE
            } else {
                FURROW
            };
            (0..width).map(move |_| shade)
        }));
        for stone in &self.stones {
            let reach = stone.radius + RING_REACH as i32;
            for y in (stone.y - reach).max(0)..(stone.y + reach + 1).min(rows) {
                for x in (stone.x - reach).max(0)..(stone.x + reach + 1).min(width) {
                    let (across, down) = ((x - stone.x) as f32, (y - stone.y) as f32);
                    let distance = across.hypot(down);
                    let color = if distance < stone.radius as f32 {
                        if across + down < 0.0 {
                            STONE_LIT
                        } else {
                            STONE_SHADED
                        }
                    } else if distance < reach as f32 {
                        let ring = ((distance - stone.radius as f32) / RING_SPACING) as i32;
                        if ring % 2 == 0 { FURROW } else { RIDGE }
                    } else {
                        continue;
                    };
                    self.grid[(y * width + x) as usize] = color;
                }
            }
        }
        for &(from, to) in &self.strokes {
            Self::groove(&mut self.grid, width, rows, from, to);
        }
        self.dirty = false;
        #[cfg(test)]
        {
            self.reflows += 1;
        }
    }

    fn groove(grid: &mut [Color], width: i32, rows: i32, from: (i32, i32), to: (i32, i32)) {
        let steps = (to.0 - from.0).abs().max((to.1 - from.1).abs()).max(1);
        for step in 0..=steps {
            let along = step as f32 / steps as f32;
            let x = from.0 + ((to.0 - from.0) as f32 * along).round() as i32;
            let y = from.1 + ((to.1 - from.1) as f32 * along).round() as i32;
            for y in [y, y + 1] {
                if (0..width).contains(&x) && (0..rows).contains(&y) {
                    grid[(y * width + x) as usize] = GROOVE;
                }
            }
        }
    }

    fn advance(&mut self, elapsed: Duration, area: Rect) {
        if area != self.area {
            self.respawn(area);
        }
        if self.dirty {
            self.reflow();
        }
        for _ in 0..self.steps.due(elapsed, STEP) {
            self.clock += f64::from(STEP);
            let now = self.clock;
            if self
                .leaf
                .as_ref()
                .is_some_and(|leaf| (now - leaf.born) as f32 > leaf.seconds)
            {
                self.leaf = None;
            }
            if now >= self.next_leaf {
                self.leaf = Some(Leaf {
                    born: now,
                    row: self.random.f32() * f32::from(area.height) * 2.0,
                    seconds: between(&mut self.random, LEAF_CROSSING_SECONDS) as f32,
                });
                self.next_leaf = now + between(&mut self.random, LEAF_EVERY_SECONDS);
            }
        }
    }

    fn draw_leaf(&self, leaf: &Leaf, pixels: &mut Pixels) {
        let age = (self.clock - leaf.born) as f32;
        let span = f32::from(pixels.width()) + 4.0;
        let x = (age / leaf.seconds * span - 2.0).round() as i32;
        let y = (leaf.row + 3.0 * (age * 1.3).sin()).round() as i32;
        for (dx, dy) in [(0, 0), (1, 0), (1, 1)] {
            pixels.set(x + dx, y + dy, LEAF);
        }
    }
}

impl Backdrop for Garden {
    fn frames_per_second(&self) -> u64 {
        10
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
        fill(&mut pixels, &self.grid);
        if let Some(leaf) = &self.leaf {
            self.draw_leaf(leaf, &mut pixels);
        }
        pixels.render(area, text, FURROW, TEXT_CEILING, buffer);
        draw_logo(logo, LOGO_TOP, LOGO_BOTTOM, buffer);
    }

    fn mouse(&mut self, event: MouseEvent) {
        let Some((x, y)) = pixel_of(self.area, event) else {
            self.drag_from = None;
            return;
        };
        let point = (x as i32, y as i32);
        match event.kind {
            // A terminal can't tell a click from the start of a drag, so every press sets a stone.
            MouseEventKind::Down(MouseButton::Left) => {
                self.drag_from = None;
                self.place(point);
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(from) = self.drag_from {
                    let (width, rows) =
                        (i32::from(self.area.width), i32::from(self.area.height) * 2);
                    Self::groove(&mut self.grid, width, rows, from, point);
                    self.strokes.push((from, point));
                    if self.strokes.len() > MAX_STROKES {
                        // Half at a time, so a long drag re-flows once per 32 segments.
                        self.strokes.drain(..MAX_STROKES / 2);
                        self.dirty = true;
                    }
                }
                self.drag_from = Some(point);
            }
            _ => self.drag_from = None,
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

    const AREA: Rect = Rect::new(0, 0, 80, 24);

    fn event(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        event(MouseEventKind::Down(MouseButton::Left), column, row)
    }

    fn seeded() -> Garden {
        Garden {
            random: Rng::with_seed(7),
            ..Garden::default()
        }
    }

    fn paint(garden: &mut Garden, elapsed: Duration, area: Rect) {
        let (logo, _) = layout(area, garden.logo_size());
        let blank = Buffer::empty(area);
        garden.paint(elapsed, area, logo, &blank, &mut Buffer::empty(area));
    }

    fn painted() -> Garden {
        let mut garden = seeded();
        paint(&mut garden, Duration::ZERO, AREA);
        garden
    }

    #[test]
    fn paints_any_area_without_panicking() {
        let mut garden = Garden::default();
        let (logo_width, logo_height) = garden.logo_size();
        for (width, height) in [(0, 0), (1, 1), (80, 24), (250, 70)] {
            let area = Rect::new(0, 0, width, height);
            let boxes = [
                None,
                (width >= logo_width && height >= logo_height)
                    .then(|| Rect::new(0, 0, logo_width, logo_height)),
            ];
            for logo in boxes {
                let mut buffer = Buffer::empty(area);
                garden.paint(
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
        let mut garden = Garden::default();
        for (second, (width, height)) in [(250, 70), (10, 3), (0, 0), (80, 24)]
            .into_iter()
            .enumerate()
        {
            let area = Rect::new(0, 0, width, height);
            let (logo, content) = layout(area, garden.logo_size());
            let mut text = Buffer::empty(area);
            text.set_style(content, Style::new().bg(Color::Red));
            garden.mouse(click(width / 2, height / 2));
            garden.mouse(event(
                MouseEventKind::Drag(MouseButton::Left),
                width / 2,
                height / 2,
            ));
            let mut buffer = Buffer::empty(area);
            garden.paint(
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
        let mut garden = Garden::default();
        paint(&mut garden, Duration::ZERO, AREA);
        paint(&mut garden, Duration::from_secs(3600), AREA);
        assert!(garden.steps.taken <= (MAX_GAP / STEP).ceil() as usize);
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
    fn idle_frames_change_almost_nothing() {
        let mut garden = seeded();
        let mut previous = Buffer::empty(AREA);
        let mut total = 0;
        let frames = 30 * 10;
        for frame in 0..frames {
            let mut buffer = Buffer::empty(AREA);
            garden.paint(
                Duration::from_millis(frame * 100),
                AREA,
                None,
                &Buffer::empty(AREA),
                &mut buffer,
            );
            total += previous.diff(&buffer).len();
            previous = buffer;
        }
        // The first frame paints everything; the rest is a leaf.
        total -= usize::from(AREA.width) * usize::from(AREA.height);
        assert!(total / (frames as usize) <= 20, "{total} cells in 30 s");
    }

    #[test]
    fn a_click_places_a_stone_and_reflows() {
        let mut garden = painted();
        let stones = garden.stones.len();
        let before = garden.grid.clone();
        garden.mouse(click(10, 5));
        let newest = garden.stones.last().unwrap();
        assert_eq!((newest.x, newest.y), (10, 11));
        assert_eq!(garden.stones.len(), stones + 1);
        for tick in 1..=20 {
            paint(&mut garden, Duration::from_millis(tick * 50), AREA);
        }
        assert_ne!(garden.grid, before);
        assert_eq!(garden.reflows, 2, "once to rake, once for the click");
    }

    #[test]
    fn a_drag_rakes_a_line() {
        let mut garden = painted();
        garden.mouse(event(MouseEventKind::Drag(MouseButton::Left), 5, 20));
        garden.mouse(event(MouseEventKind::Drag(MouseButton::Left), 25, 20));
        let row = 41;
        assert!((5..=25).all(|x| garden.grid[row * 80 + x] == GROOVE));
    }

    #[test]
    fn stones_are_capped() {
        let mut garden = painted();
        for column in 0..50 {
            garden.mouse(click(column, 5));
        }
        assert_eq!(garden.stones.len(), MAX_STONES);
        for column in 0..200 {
            garden.mouse(event(
                MouseEventKind::Drag(MouseButton::Left),
                column % 80,
                3,
            ));
        }
        assert!(garden.strokes.len() <= MAX_STROKES);
    }

    #[test]
    fn a_long_drag_reflows_rarely() {
        let mut garden = painted();
        for tick in 0..200u16 {
            garden.mouse(event(
                MouseEventKind::Drag(MouseButton::Left),
                tick % 70,
                2 + tick % 15,
            ));
            paint(
                &mut garden,
                Duration::from_millis(u64::from(tick) * 100),
                AREA,
            );
        }
        assert!(garden.reflows <= 8, "{}", garden.reflows);
    }

    #[test]
    fn leaving_the_garden_ends_a_drag() {
        let mut garden = painted();
        garden.mouse(event(MouseEventKind::Drag(MouseButton::Left), 5, 5));
        garden.mouse(event(MouseEventKind::Drag(MouseButton::Left), 500, 5));
        garden.mouse(event(MouseEventKind::Drag(MouseButton::Left), 60, 5));
        assert!(garden.strokes.is_empty());
    }

    #[test]
    fn events_outside_the_garden_are_ignored() {
        let mut garden = painted();
        let stones = garden.stones.len();
        garden.mouse(click(500, 500));
        garden.mouse(click(80, 0));
        assert_eq!(garden.stones.len(), stones);
    }
}
