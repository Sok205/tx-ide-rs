//! Winter pines: a dark blue-grey sky in three bands, a few pine silhouettes along the bottom,
//! and snow that sways down, settles on the ground and the branches, and melts away oldest first
//! so the drifts stay low. The pointer is a wind; a click shakes the snow off the nearest pine.

use std::collections::VecDeque;
use std::time::Duration;

use fastrand::Rng;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_home::backdrop::{Backdrop, Pixels};

use crate::common::{
    Pointer, STEP, Steps, angle, draw_logo, fill, logo_size, pixel_of, quantised, value_noise,
};

const SKY_TOP: Color = Color::Rgb(9, 13, 24);
const SKY_HORIZON: Color = Color::Rgb(30, 40, 58);
const SKY_SHADES: u8 = 3;
/// The lightest sky: text cells show the sky as it is, and snow under them no brighter.
const TEXT_CEILING: Color = SKY_HORIZON;
const PINE_LIT: Color = Color::Rgb(17, 38, 44);
const PINE_SHADED: Color = Color::Rgb(10, 25, 31);
const GROUND: Color = Color::Rgb(112, 130, 160);
const SNOW: Color = Color::Rgb(232, 240, 252);
const FLAKE_NEAR: Color = Color::Rgb(246, 250, 255);
const FLAKE_FAR: Color = Color::Rgb(150, 168, 198);
const LOGO_TOP: Color = Color::Rgb(244, 248, 255);
const LOGO_BOTTOM: Color = Color::Rgb(146, 172, 210);
const CELLS_PER_FLAKE: usize = 200;
const MIN_PINES: usize = 3;
const MAX_PINES: usize = 6;
/// Pixels of bare ground along the bottom, which drifts pile on.
const GROUND_ROWS: i32 = 2;
/// Snow a column of ground holds, in pixels: three cell rows.
const MAX_DRIFT: u8 = 6;
const MAX_BRANCH_SNOW: u8 = 2;
/// Settled flakes kept per column of the area; past that the oldest melt.
const SETTLED_PER_COLUMN: usize = 2;
const WIND_RANGE: f32 = 30.0;
const WIND_SPEED: f32 = 60.0;
/// Flakes a shaken pine drops, and how many shaken flakes may be falling at once.
const SHAKE_FLAKES: usize = 14;
const MAX_SHAKEN: usize = 64;
/// How far past a pine's branches a click still counts as on it.
const NEAR_PINE: i32 = 4;

pub fn pines() -> Box<dyn Backdrop> {
    Box::<Pines>::default()
}

/// In pixels relative to the area.
struct Pine {
    x: i32,
    height: i32,
    /// Rows of branches; the trunk is the rest.
    body: i32,
    widest: i32,
}

struct Flake {
    x: f32,
    y: f32,
    /// Pixels per second.
    fall: f32,
    sway_amplitude: f32,
    sway_rate: f32,
    sway_phase: f32,
    /// Pixels per second sideways, for shaken flakes.
    drift: f32,
    shaken: bool,
    near: bool,
}

#[derive(Default)]
struct Pines {
    random: Rng,
    steps: Steps,
    /// The last painted area, which mouse events are relative to and the scene was built for.
    area: Rect,
    /// Sky, ground and trees: one colour per pixel, row by row.
    scene: Vec<Color>,
    trees: Vec<Pine>,
    /// The y of the highest solid pixel in each column.
    surface: Vec<i32>,
    /// Pixels of snow lying on `surface`, per column.
    depth: Vec<u8>,
    /// Columns of settled flakes, oldest first.
    settled: VecDeque<usize>,
    flakes: Vec<Flake>,
    /// Simulated seconds; f64 so a scene open for days keeps its precision.
    clock: f64,
    pointer: Pointer,
}

impl Pine {
    fn half_width(&self, row: i32) -> i32 {
        if row >= self.body {
            return 1;
        }
        let tier_rows = (self.body / 3).max(1);
        let tier = (row / tier_rows).min(2);
        let into = (row - tier * tier_rows) as f32 / tier_rows as f32;
        let fraction = ((tier as f32 + 0.35 + 0.65 * into) / 3.0).min(1.0);
        (self.widest as f32 * fraction).round() as i32
    }
}

impl Pines {
    fn width(&self) -> usize {
        usize::from(self.area.width)
    }

    fn rows(&self) -> i32 {
        i32::from(self.area.height) * 2
    }

    fn ground_top(&self) -> i32 {
        (self.rows() - GROUND_ROWS).max(0)
    }

    fn new_flake(&mut self, y: f32) -> Flake {
        let near = self.random.bool();
        Flake {
            x: self.random.f32() * f32::from(self.area.width),
            y,
            fall: if near { 11.0 } else { 7.0 } + 4.0 * self.random.f32(),
            sway_amplitude: 1.0 + 2.0 * self.random.f32(),
            sway_rate: 0.8 + 1.4 * self.random.f32(),
            sway_phase: std::f32::consts::TAU * self.random.f32(),
            drift: 0.0,
            shaken: false,
            near,
        }
    }

    fn respawn(&mut self, area: Rect) {
        self.area = area;
        let (width, rows) = (self.width(), self.rows());
        let ground_top = self.ground_top();
        self.scene = (0..rows)
            .flat_map(|y| {
                (0..width).map(move |x| {
                    if y >= ground_top {
                        return GROUND;
                    }
                    let wobble = (value_noise(x as f32 * 0.03, y as f32 * 0.07, 3) - 0.5) * 0.25;
                    quantised(
                        SKY_TOP,
                        SKY_HORIZON,
                        y as f32 / rows as f32 + wobble,
                        SKY_SHADES,
                    )
                })
            })
            .collect();
        let count = self.random.usize(MIN_PINES..=MAX_PINES);
        self.trees = (0..count)
            .map(|index| {
                let height = ((rows as f32 * (0.28 + 0.27 * self.random.f32())) as i32).max(6);
                let slot = width as f32 / count as f32;
                let jitter = (self.random.f32() - 0.5) * slot * 0.5;
                Pine {
                    x: ((index as f32 + 0.5) * slot + jitter) as i32,
                    height,
                    body: height - 3,
                    widest: (height as f32 * (0.22 + 0.1 * self.random.f32())) as i32 + 1,
                }
            })
            .collect();
        self.surface = vec![ground_top; width];
        for pine in &self.trees {
            for row in 0..pine.height {
                let y = ground_top - pine.height + row;
                let half = pine.half_width(row);
                for x in pine.x - half..=pine.x + half {
                    if let Some(column) = usize::try_from(x).ok().filter(|column| *column < width) {
                        self.surface[column] = self.surface[column].min(y);
                        if let Ok(y) = usize::try_from(y) {
                            self.scene[y * width + column] =
                                if x < pine.x { PINE_LIT } else { PINE_SHADED };
                        }
                    }
                }
            }
        }
        self.depth = vec![0; width];
        self.settled.clear();
        let count = width * usize::from(area.height) / CELLS_PER_FLAKE;
        self.flakes = (0..count)
            .map(|_| {
                let y = self.random.f32() * rows as f32;
                self.new_flake(y)
            })
            .collect();
    }

    fn column(&self, flake: &Flake) -> usize {
        let swayed = flake.x
            + flake.sway_amplitude
                * (angle(self.clock, f64::from(flake.sway_rate)) + flake.sway_phase).sin();
        (swayed.round() as i32).rem_euclid(self.width() as i32) as usize
    }

    fn settle(&mut self, column: usize) {
        let limit = if self.surface[column] < self.ground_top() {
            MAX_BRANCH_SNOW
        } else {
            MAX_DRIFT
        };
        if self.depth[column] >= limit {
            return;
        }
        self.depth[column] += 1;
        self.settled.push_back(column);
        if self.settled.len() > self.width() * SETTLED_PER_COLUMN {
            let oldest = self.settled.pop_front().expect("the queue is not empty");
            self.depth[oldest] -= 1;
        }
    }

    fn shake(&mut self, point: (f32, f32)) {
        let ground_top = self.ground_top();
        let reach = |pine: &&Pine| {
            (pine.x - point.0 as i32).abs() <= pine.widest + NEAR_PINE
                && point.1 as i32 >= ground_top - pine.height - NEAR_PINE
        };
        let Some(pine) = self
            .trees
            .iter()
            .filter(reach)
            .min_by_key(|pine| (pine.x - point.0 as i32).abs())
        else {
            return;
        };
        let (left, right) = (pine.x - pine.widest, pine.x + pine.widest);
        let columns: Vec<usize> = (0..self.width())
            .filter(|&column| {
                (left..=right).contains(&(column as i32))
                    && self.surface[column] < ground_top
                    && self.depth[column] > 0
            })
            .collect();
        if columns.is_empty() {
            return;
        }
        for &column in &columns {
            self.depth[column] = 0;
        }
        self.settled.retain(|column| !columns.contains(column));
        let shaken = self.flakes.iter().filter(|flake| flake.shaken).count();
        for _ in 0..SHAKE_FLAKES.min(MAX_SHAKEN.saturating_sub(shaken)) {
            let column = columns[self.random.usize(..columns.len())];
            let mut flake = self.new_flake(0.0);
            flake.x = column as f32;
            flake.y = (self.surface[column] - 1 - self.random.i32(0..6)) as f32;
            flake.fall = 18.0 + 14.0 * self.random.f32();
            flake.drift = (self.random.f32() - 0.5) * 24.0;
            flake.shaken = true;
            self.flakes.push(flake);
        }
    }

    fn advance(&mut self, elapsed: Duration, area: Rect) {
        if area != self.area {
            self.respawn(area);
        }
        for _ in 0..self.steps.due(elapsed, STEP) {
            self.clock += f64::from(STEP);
            self.step();
        }
    }

    fn step(&mut self) {
        let wind = self.pointer.noticed(self.clock);
        let width = f32::from(self.area.width);
        for index in (0..self.flakes.len()).rev() {
            let flake = &mut self.flakes[index];
            flake.y += flake.fall * STEP;
            flake.x += flake.drift * STEP;
            if let Some(pointer) = wind {
                let reach = (flake.x - pointer.0).hypot(flake.y - pointer.1);
                if reach < WIND_RANGE {
                    let away = if flake.x < pointer.0 { -1.0 } else { 1.0 };
                    flake.x += away * WIND_SPEED * (1.0 - reach / WIND_RANGE) * STEP;
                }
            }
            flake.x = flake.x.rem_euclid(width);
            let column = self.column(&self.flakes[index]);
            let flake = &self.flakes[index];
            if flake.y as i32 >= self.surface[column] - i32::from(self.depth[column]) {
                self.settle(column);
                if self.flakes[index].shaken {
                    self.flakes.swap_remove(index);
                } else {
                    self.flakes[index] = self.new_flake(0.0);
                }
            }
        }
    }
}

impl Backdrop for Pines {
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
        for (column, depth) in self.depth.iter().enumerate() {
            for lift in 1..=i32::from(*depth) {
                pixels.set(column as i32, self.surface[column] - lift, SNOW);
            }
        }
        for flake in &self.flakes {
            let color = if flake.near { FLAKE_NEAR } else { FLAKE_FAR };
            pixels.set(self.column(flake) as i32, flake.y as i32, color);
        }
        pixels.render(area, text, SKY_TOP, TEXT_CEILING, buffer);
        draw_logo(logo, LOGO_TOP, LOGO_BOTTOM, buffer);
    }

    fn mouse(&mut self, event: MouseEvent) {
        let Some(point) = pixel_of(self.area, event) else {
            return;
        };
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => self.shake(point),
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                self.pointer.moved(point, self.clock);
            }
            _ => {}
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

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        mouse(MouseEventKind::Down(MouseButton::Left), column, row)
    }

    fn seeded() -> Pines {
        Pines {
            random: Rng::with_seed(7),
            ..Pines::default()
        }
    }

    fn paint(pines: &mut Pines, elapsed: Duration, area: Rect) {
        let (logo, _) = layout(area, pines.logo_size());
        let blank = Buffer::empty(area);
        pines.paint(elapsed, area, logo, &blank, &mut Buffer::empty(area));
    }

    const AREA: Rect = Rect::new(0, 0, 80, 24);

    fn painted() -> Pines {
        let mut pines = seeded();
        paint(&mut pines, Duration::ZERO, AREA);
        pines
    }

    #[test]
    fn paints_any_area_without_panicking() {
        let mut pines = Pines::default();
        let (logo_width, logo_height) = pines.logo_size();
        for (width, height) in [(0, 0), (1, 1), (80, 24), (250, 70)] {
            let area = Rect::new(0, 0, width, height);
            let boxes = [
                None,
                (width >= logo_width && height >= logo_height)
                    .then(|| Rect::new(0, 0, logo_width, logo_height)),
            ];
            for logo in boxes {
                let mut buffer = Buffer::empty(area);
                pines.paint(
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
        let mut pines = Pines::default();
        for (second, (width, height)) in [(250, 70), (10, 3), (0, 0), (80, 24)]
            .into_iter()
            .enumerate()
        {
            let area = Rect::new(0, 0, width, height);
            let (logo, content) = layout(area, pines.logo_size());
            let mut text = Buffer::empty(area);
            text.set_style(content, Style::new().bg(Color::Red));
            pines.mouse(click(width / 2, height / 2));
            let mut buffer = Buffer::empty(area);
            pines.paint(
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
        let mut pines = Pines::default();
        paint(&mut pines, Duration::ZERO, AREA);
        paint(&mut pines, Duration::from_secs(3600), AREA);
        assert!(pines.steps.taken <= (MAX_GAP / STEP).ceil() as usize);
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
    fn the_snow_bank_stays_bounded() {
        let mut pines = painted();
        let flakes = pines.flakes.len();
        for tick in 1..=60 * 15 {
            paint(&mut pines, Duration::from_millis(tick * 66), AREA);
        }
        assert!(pines.settled.len() <= pines.width() * SETTLED_PER_COLUMN);
        assert!(!pines.settled.is_empty(), "snow does settle");
        assert!(pines.depth.iter().all(|depth| *depth <= MAX_DRIFT));
        assert_eq!(pines.flakes.len(), flakes);
    }

    #[test]
    fn the_pointer_pushes_flakes() {
        let mut pines = painted();
        pines.flakes.clear();
        let mut flake = pines.new_flake(0.0);
        (flake.x, flake.y, flake.fall) = (20.0, 11.0, 0.0);
        let mut left = pines.new_flake(0.0);
        (left.x, left.y, left.fall) = (10.0, 11.0, 0.0);
        pines.flakes.extend([flake, left]);
        pines.mouse(mouse(MouseEventKind::Moved, 15, 5));
        paint(&mut pines, Duration::from_millis(200), AREA);
        assert!(pines.flakes[0].x > 20.0, "{}", pines.flakes[0].x);
        assert!(pines.flakes[1].x < 10.0, "{}", pines.flakes[1].x);
    }

    #[test]
    fn a_click_shakes_a_pine() {
        let mut pines = painted();
        pines.flakes.clear();
        let pine = &pines.trees[0];
        let column = pine.x as usize;
        pines.depth[column] = 2;
        pines.settled.push_back(column);
        let row = (pines.surface[column] / 2) as u16;
        pines.mouse(click(column as u16, row));
        assert_eq!(pines.depth[column], 0);
        assert!(pines.settled.is_empty());
        assert_eq!(pines.flakes.len(), SHAKE_FLAKES);
    }

    #[test]
    fn a_click_high_above_a_pine_or_on_a_bare_one_shakes_nothing() {
        let mut pines = painted();
        pines.flakes.clear();
        let column = pines.trees[0].x as usize;
        pines.depth[column] = 2;
        pines.mouse(click(column as u16, 0));
        assert_eq!(
            pines.depth[column], 2,
            "the sky above the pine is not the pine"
        );
        pines.depth[column] = 0;
        let row = (pines.surface[column] / 2) as u16;
        pines.mouse(click(column as u16, row));
        assert!(pines.flakes.is_empty(), "no snow, no burst");
    }

    #[test]
    fn clicks_are_capped() {
        let mut pines = painted();
        let column = pines.trees[0].x as usize;
        let row = (pines.surface[column] / 2) as u16;
        let ground_top = pines.ground_top();
        for _ in 0..200 {
            for branch in 0..pines.width() {
                if pines.surface[branch] < ground_top {
                    pines.depth[branch] = MAX_BRANCH_SNOW;
                }
            }
            pines.mouse(click(column as u16, row));
        }
        let shaken = pines.flakes.iter().filter(|flake| flake.shaken).count();
        assert_eq!(shaken, MAX_SHAKEN);
    }

    #[test]
    fn events_outside_the_scene_are_ignored() {
        let mut pines = painted();
        let flakes = pines.flakes.len();
        pines.mouse(click(500, 500));
        pines.mouse(mouse(MouseEventKind::Moved, 80, 0));
        assert_eq!(pines.flakes.len(), flakes);
        assert_eq!(pines.pointer.noticed(pines.clock), None);
    }

    #[test]
    fn the_pointer_is_forgotten_after_three_seconds() {
        let mut pines = painted();
        let moved = mouse(MouseEventKind::Moved, 10, 5);
        pines.mouse(moved);
        assert_eq!(pines.pointer.noticed(pines.clock), Some((10.0, 11.0)));
        for tick in 1..=80 {
            paint(&mut pines, Duration::from_millis(tick * 50), AREA);
        }
        assert_eq!(pines.pointer.noticed(pines.clock), None);
        pines.mouse(moved);
        assert_eq!(pines.pointer.noticed(pines.clock), Some((10.0, 11.0)));
    }
}
