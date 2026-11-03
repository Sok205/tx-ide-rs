//! A slow cellular-life board, one cell per half-block pixel on a torus: dim teal cells on
//! near-black, newborn ones brighter for a step. It steps at 4 Hz while painting at 15 FPS, tops
//! itself up with a small patch of soup whenever it thins out or settles, and a click stamps a
//! glider that flies away from the screen centre.

use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, Pixels};

use crate::common::{Steps, draw_logo, logo_size, pixel_of};

const VOID: Color = Color::Rgb(5, 9, 12);
const LIVE: Color = Color::Rgb(22, 92, 94);
const BORN: Color = Color::Rgb(96, 214, 196);
/// Text cells show a live cell at most as bright as this.
const TEXT_CEILING: Color = LIVE;
const LOGO_TOP: Color = Color::Rgb(176, 236, 224);
const LOGO_BOTTOM: Color = Color::Rgb(84, 160, 158);
/// Seconds per generation.
const STEP: f32 = 0.25;
/// Share of the opening board that starts alive: sparse, so a step flips few cells.
const SOUP_DENSITY: f32 = 0.05;
/// A reseed drops a patch this big, half alive, somewhere on the board.
const PATCH: (usize, usize) = (20, 12);
const PATCH_DENSITY: f32 = 0.4;
const MIN_POPULATION: f32 = 0.02;
/// Flips per step at or under which the board counts as having stopped changing.
const STILL_FLIPS: usize = 6;
const STALL_STEPS: u32 = 12;
/// A glider heading down and to the right, as (column, row) offsets.
const GLIDER: [(i32, i32); 5] = [(1, 0), (2, 1), (0, 2), (1, 2), (2, 2)];

pub fn cells() -> Box<dyn Backdrop> {
    Box::<Life>::default()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Dead,
    Alive,
    Newborn,
}

#[derive(Default)]
struct Life {
    random: Rng,
    steps: Steps,
    /// The last painted area, which mouse events are relative to and the board was built for.
    area: Rect,
    width: usize,
    height: usize,
    cells: Vec<Cell>,
    scratch: Vec<Cell>,
    still_steps: u32,
}

impl Life {
    fn index(&self, column: i64, row: i64) -> usize {
        let column = column.rem_euclid(self.width as i64) as usize;
        let row = row.rem_euclid(self.height as i64) as usize;
        row * self.width + column
    }

    fn population(&self) -> usize {
        self.cells
            .iter()
            .filter(|cell| **cell != Cell::Dead)
            .count()
    }

    fn respawn(&mut self, area: Rect) {
        self.area = area;
        self.width = usize::from(area.width);
        self.height = usize::from(area.height) * 2;
        self.cells = (0..self.width * self.height)
            .map(|_| {
                if self.random.f32() < SOUP_DENSITY {
                    Cell::Alive
                } else {
                    Cell::Dead
                }
            })
            .collect();
        self.scratch = self.cells.clone();
        self.still_steps = 0;
    }

    fn sprinkle(&mut self) {
        let corner = (
            self.random.usize(..self.width) as i64,
            self.random.usize(..self.height) as i64,
        );
        for row in 0..PATCH.1 as i64 {
            for column in 0..PATCH.0 as i64 {
                if self.random.f32() < PATCH_DENSITY {
                    let index = self.index(corner.0 + column, corner.1 + row);
                    self.cells[index] = Cell::Alive;
                }
            }
        }
    }

    fn step(&mut self) {
        if self.cells.is_empty() {
            return;
        }
        let mut flips = 0;
        for row in 0..self.height as i64 {
            for column in 0..self.width as i64 {
                let neighbours = (-1..=1)
                    .flat_map(|down| (-1..=1).map(move |across| (across, down)))
                    .filter(|offset| *offset != (0, 0))
                    .filter(|(across, down)| {
                        self.cells[self.index(column + across, row + down)] != Cell::Dead
                    })
                    .count();
                let index = self.index(column, row);
                let was_alive = self.cells[index] != Cell::Dead;
                self.scratch[index] = match (was_alive, neighbours) {
                    (true, 2 | 3) => Cell::Alive,
                    (false, 3) => Cell::Newborn,
                    _ => Cell::Dead,
                };
                flips += usize::from(was_alive != (self.scratch[index] != Cell::Dead));
            }
        }
        std::mem::swap(&mut self.cells, &mut self.scratch);
        self.still_steps = if flips <= STILL_FLIPS {
            self.still_steps + 1
        } else {
            0
        };
        let thin = (self.population() as f32) < MIN_POPULATION * self.cells.len() as f32;
        if thin || self.still_steps >= STALL_STEPS {
            self.sprinkle();
            self.still_steps = 0;
        }
    }

    fn advance(&mut self, elapsed: Duration, area: Rect) {
        if area != self.area {
            self.respawn(area);
        }
        for _ in 0..self.steps.due(elapsed, STEP) {
            self.step();
        }
    }

    fn stamp_glider(&mut self, (column, row): (i32, i32), (right, down): (i32, i32)) {
        for (across, along) in GLIDER {
            let index = self.index(
                i64::from(column + right * (across - 1)),
                i64::from(row + down * (along - 1)),
            );
            self.cells[index] = Cell::Alive;
        }
    }
}

impl Backdrop for Life {
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
        for (index, cell) in self.cells.iter().enumerate() {
            let color = match cell {
                Cell::Dead => VOID,
                Cell::Alive => LIVE,
                Cell::Newborn => BORN,
            };
            pixels.set(
                (index % self.width) as i32,
                (index / self.width) as i32,
                color,
            );
        }
        pixels.render(area, text, VOID, TEXT_CEILING, buffer);
        draw_logo(logo, LOGO_TOP, LOGO_BOTTOM, buffer);
    }

    fn mouse(&mut self, event: MouseEvent) {
        let Some((x, y)) = pixel_of(self.area, event) else {
            return;
        };
        if event.kind == MouseEventKind::Down(MouseButton::Left) {
            let point = (x as i32, y as i32);
            let away = (
                if 2 * point.0 >= self.width as i32 {
                    1
                } else {
                    -1
                },
                if 2 * point.1 >= self.height as i32 {
                    1
                } else {
                    -1
                },
            );
            self.stamp_glider(point, away);
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
    use crate::common::tests::{assert_nothing_bright_under_text, assert_within_budget};

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn seeded() -> Life {
        Life {
            random: Rng::with_seed(7),
            ..Life::default()
        }
    }

    /// A seeded 40x10 board (40x20 pixels) with nothing alive.
    fn empty_board() -> Life {
        let mut life = seeded();
        life.respawn(Rect::new(0, 0, 40, 10));
        life.cells.fill(Cell::Dead);
        life
    }

    /// Still-life blocks across the lower rows, so the board isn't thin enough to reseed.
    fn fill_with_blocks(life: &mut Life, rows: std::ops::Range<i64>) {
        for row in rows.step_by(4) {
            for column in (0..40).step_by(4) {
                for (across, down) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let index = life.index(column + across, row + down);
                    life.cells[index] = Cell::Alive;
                }
            }
        }
    }

    /// Live cells above the filler blocks.
    fn alive(life: &Life) -> Vec<(usize, usize)> {
        let mut found: Vec<_> = life
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| **cell != Cell::Dead)
            .map(|(index, _)| (index % life.width, index / life.width))
            .filter(|(_, row)| *row < 12)
            .collect();
        found.sort();
        found
    }

    #[test]
    fn paints_any_area_without_panicking() {
        let mut life = Life::default();
        let (logo_width, logo_height) = life.logo_size();
        for (width, height) in [(0, 0), (1, 1), (80, 24), (250, 70)] {
            let area = Rect::new(0, 0, width, height);
            let boxes = [
                None,
                (width >= logo_width && height >= logo_height)
                    .then(|| Rect::new(0, 0, logo_width, logo_height)),
            ];
            for logo in boxes {
                let mut buffer = Buffer::empty(area);
                life.paint(
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
        let mut life = Life::default();
        for (second, (width, height)) in [(250, 70), (10, 3), (0, 0), (80, 24)]
            .into_iter()
            .enumerate()
        {
            let area = Rect::new(0, 0, width, height);
            let (logo, content) = layout(area, life.logo_size());
            let mut text = Buffer::empty(area);
            text.set_style(content, Style::new().bg(Color::Red));
            life.mouse(click(width / 2, height / 2));
            let mut buffer = Buffer::empty(area);
            life.paint(
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
        let mut life = Life::default();
        let area = Rect::new(0, 0, 80, 24);
        let blank = Buffer::empty(area);
        life.paint(Duration::ZERO, area, None, &blank, &mut blank.clone());
        life.paint(
            Duration::from_secs(3600),
            area,
            None,
            &blank,
            &mut blank.clone(),
        );
        assert!(life.steps.taken <= (MAX_GAP / STEP).ceil() as usize);
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
    fn a_blinker_oscillates() {
        let mut life = empty_board();
        fill_with_blocks(&mut life, 12..20);
        for column in 10..13 {
            let index = life.index(column, 8);
            life.cells[index] = Cell::Alive;
        }
        let flat = alive(&life);
        life.step();
        let upright = alive(&life);
        assert_eq!(upright, [(11, 7), (11, 8), (11, 9)]);
        life.step();
        assert_eq!(alive(&life), flat);
    }

    #[test]
    fn a_glider_moves() {
        let mut life = empty_board();
        fill_with_blocks(&mut life, 12..20);
        life.stamp_glider((10, 5), (1, 1));
        let before = alive(&life);
        for _ in 0..4 {
            life.step();
        }
        let shifted: Vec<_> = before.iter().map(|(x, y)| (x + 1, y + 1)).collect();
        assert_eq!(alive(&life), shifted);
    }

    #[test]
    fn a_click_stamps_a_glider_away_from_the_centre() {
        let mut life = empty_board();
        life.mouse(click(3, 1));
        assert_eq!(alive(&life), [(2, 2), (2, 3), (3, 2), (3, 4), (4, 2)]);
        let mut life = empty_board();
        life.mouse(click(36, 4));
        assert_eq!(life.population(), 5);
    }

    #[test]
    fn a_stalled_board_reseeds() {
        let mut life = empty_board();
        fill_with_blocks(&mut life, 0..20);
        let blocks = life.population();
        for _ in 0..STALL_STEPS - 1 {
            life.step();
        }
        assert_eq!(life.population(), blocks, "not stalled long enough yet");
        life.step();
        assert!(life.population() > blocks);
    }

    #[test]
    fn a_thin_board_reseeds() {
        let mut life = empty_board();
        life.step();
        assert!(life.population() > 0);
    }

    #[test]
    fn events_outside_the_board_are_ignored() {
        let mut life = seeded();
        life.respawn(Rect::new(0, 0, 40, 10));
        life.cells.fill(Cell::Dead);
        life.mouse(click(500, 500));
        assert_eq!(life.population(), 0);
    }
}
