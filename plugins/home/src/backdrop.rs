//! The extension point a themes plugin uses to paint an animated background and logo under the
//! home TUI. Home draws its text on top of whatever the backdrop painted.

use std::sync::OnceLock;
use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::MouseEvent;
use ratatui::layout::Rect;
use ratatui::style::Color;

pub trait Backdrop {
    fn frames_per_second(&self) -> u64;
    /// (width, height) of this theme's logo in cells.
    fn logo_size(&self) -> (u16, u16);
    /// `elapsed`: time since the backdrop was built, less the time the pane spent unfocused.
    /// `logo`: the box home reserved, if any. `text`: the region home will write text into.
    fn paint(
        &mut self,
        elapsed: Duration,
        area: Rect,
        logo: Option<Rect>,
        text: Rect,
        buffer: &mut Buffer,
    );
    fn mouse(&mut self, event: MouseEvent);
}

pub type Constructor = fn() -> Box<dyn Backdrop>;

static CHOSEN: OnceLock<Constructor> = OnceLock::new();

/// The first install wins: one `tx` process runs `register` twice (`preview_verbs`, then `open`).
pub fn install(constructor: Constructor) {
    let _ = CHOSEN.set(constructor);
}

pub fn chosen() -> Option<Constructor> {
    CHOSEN.get().copied()
}

pub const MINIMUM_LIST_ROWS: u16 = 10;

const TEXT_DIM: f32 = 0.35;

/// Plain "tx-ide" block letters.
pub const LOGO: &[&str] = &[
    "███ █ █     ███   █ ███",
    " █  █ █      █    █ █  ",
    " █   █  ███  █  ███ ███",
    " █  █ █      █  █ █ █  ",
    " █  █ █     ███ ███ ███",
];

/// (logo box, content area). The logo shows when `area.width >= logo width` and
/// `area.height >= logo height + 1 (gap) + MINIMUM_LIST_ROWS + 2 (header, footer)`.
pub fn layout(area: Rect, logo_size: (u16, u16)) -> (Option<Rect>, Rect) {
    let (logo_width, logo_height) = logo_size;
    if area.width < logo_width || area.height < logo_height + 1 + MINIMUM_LIST_ROWS + 2 {
        return (None, area);
    }
    let logo = Rect::new(
        area.x + (area.width - logo_width) / 2,
        area.y,
        logo_width,
        logo_height,
    );
    let content = Rect::new(
        area.x,
        area.y + logo_height + 1,
        area.width,
        area.height - logo_height - 1,
    );
    (Some(logo), content)
}

/// `color`, or its 256-colour palette entry when `TX_HOME_COLORS=256`.
pub fn palette(color: Color) -> Color {
    static INDEXED: OnceLock<bool> = OnceLock::new();
    let indexed_palette =
        INDEXED.get_or_init(|| std::env::var("TX_HOME_COLORS").is_ok_and(|value| value == "256"));
    if *indexed_palette {
        indexed(color)
    } else {
        color
    }
}

/// The nearest entry of the 6x6x6 colour cube.
fn indexed(color: Color) -> Color {
    let Color::Rgb(red, green, blue) = color else {
        return color;
    };
    let level = |channel: u8| (u16::from(channel) * 5 + 127) / 255;
    let index = 16 + 36 * level(red) + 6 * level(green) + level(blue);
    Color::Indexed(u8::try_from(index).expect("cube indices are at most 231"))
}

/// Scales the channels of an `Rgb` colour; other colours pass through.
pub fn dim(color: Color, factor: f32) -> Color {
    let Color::Rgb(red, green, blue) = color else {
        return color;
    };
    let scale = |channel: u8| (f32::from(channel) * factor).round() as u8;
    Color::Rgb(scale(red), scale(green), scale(blue))
}

/// The mean of two `Rgb` colours; for anything else, the first.
fn mean(first: Color, second: Color) -> Color {
    match (first, second) {
        (Color::Rgb(red_a, green_a, blue_a), Color::Rgb(red_b, green_b, blue_b)) => {
            let average = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
            Color::Rgb(
                average(red_a, red_b),
                average(green_a, green_b),
                average(blue_a, blue_b),
            )
        }
        _ => first,
    }
}

/// A grid of half-block pixels: `width` cells by `2 * height` rows.
pub struct Pixels {
    width: u16,
    height: u16,
    cells: Vec<Option<Color>>,
}

impl Pixels {
    pub fn new(width: u16, height: u16) -> Self {
        let rows = height * 2;
        Self {
            width,
            height: rows,
            cells: vec![None; usize::from(width) * usize::from(rows)],
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn set(&mut self, x: i32, y: i32, color: Color) {
        if let Some(index) = self.index(x, y) {
            self.cells[index] = Some(color);
        }
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let in_range =
            (0..i32::from(self.width)).contains(&x) && (0..i32::from(self.height)).contains(&y);
        in_range.then(|| y as usize * usize::from(self.width) + x as usize)
    }

    fn get(&self, x: i32, y: i32, background: Color) -> Color {
        self.index(x, y)
            .and_then(|index| self.cells[index])
            .unwrap_or(background)
    }

    pub fn render(&self, area: Rect, text: Rect, background: Color, buffer: &mut Buffer) {
        for row in 0..area.height {
            for column in 0..area.width {
                let (x, y) = (area.x + column, area.y + row);
                let top = self.get(i32::from(column), i32::from(row) * 2, background);
                let bottom = self.get(i32::from(column), i32::from(row) * 2 + 1, background);
                let cell = &mut buffer[(x, y)];
                if text.contains((x, y).into()) {
                    cell.set_char(' ')
                        .set_bg(palette(dim(mean(top, bottom), TEXT_DIM)));
                } else {
                    cell.set_char('▀')
                        .set_fg(palette(top))
                        .set_bg(palette(bottom));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Color = Color::Rgb(255, 0, 0);
    const BLUE: Color = Color::Rgb(0, 0, 255);
    const BACKGROUND: Color = Color::Rgb(10, 20, 30);

    fn two_pixels() -> (Pixels, Rect, Buffer) {
        let mut pixels = Pixels::new(2, 1);
        pixels.set(0, 0, RED);
        pixels.set(0, 1, BLUE);
        let area = Rect::new(0, 0, 2, 1);
        (pixels, area, Buffer::empty(area))
    }

    #[test]
    fn the_logo_shows_above_the_list_when_it_fits() {
        let (logo, content) = layout(Rect::new(0, 0, 80, 30), (40, 6));
        assert_eq!(logo, Some(Rect::new(20, 0, 40, 6)));
        assert_eq!(content, Rect::new(0, 7, 80, 23));
    }

    #[test]
    fn the_logo_hides_when_the_list_would_get_fewer_than_ten_rows() {
        let short = Rect::new(0, 0, 80, 6 + 1 + 10 + 2 - 1);
        assert_eq!(layout(short, (40, 6)), (None, short));
        assert!(layout(Rect::new(0, 0, 80, 19), (40, 6)).0.is_some());
    }

    #[test]
    fn the_logo_hides_when_too_narrow() {
        let narrow = Rect::new(0, 0, 39, 30);
        assert_eq!(layout(narrow, (40, 6)), (None, narrow));
    }

    #[test]
    fn the_logo_art_is_within_bounds_with_equal_width_rows() {
        assert!(LOGO.len() <= 7);
        let width = LOGO[0].chars().count();
        assert!(width <= 48);
        assert!(LOGO.iter().all(|row| row.chars().count() == width));
    }

    #[test]
    fn the_first_install_wins() {
        fn a() -> Box<dyn Backdrop> {
            unreachable!()
        }
        fn b() -> Box<dyn Backdrop> {
            unreachable!()
        }
        install(a);
        install(b);
        assert!(std::ptr::fn_addr_eq(chosen().unwrap(), a as Constructor));
    }

    #[test]
    fn pixels_outside_text_are_half_blocks() {
        let (pixels, area, mut buffer) = two_pixels();
        pixels.render(area, Rect::default(), BACKGROUND, &mut buffer);
        let cell = &buffer[(0, 0)];
        assert_eq!((cell.symbol(), cell.fg, cell.bg), ("▀", RED, BLUE));
        let empty = &buffer[(1, 0)];
        assert_eq!((empty.fg, empty.bg), (BACKGROUND, BACKGROUND));
    }

    #[test]
    fn pixels_inside_text_are_dimmed_spaces() {
        let (pixels, area, mut buffer) = two_pixels();
        pixels.render(area, area, BACKGROUND, &mut buffer);
        let cell = &buffer[(0, 0)];
        assert_eq!(cell.symbol(), " ");
        assert_eq!(cell.bg, dim(mean(RED, BLUE), TEXT_DIM));
    }

    #[test]
    fn pixels_ignore_out_of_range_and_tiny_areas() {
        let mut pixels = Pixels::new(2, 1);
        pixels.set(-1, 0, RED);
        pixels.set(99, 99, RED);
        let mut empty = Pixels::new(0, 0);
        empty.set(0, 0, RED);
        empty.render(
            Rect::default(),
            Rect::default(),
            BACKGROUND,
            &mut Buffer::empty(Rect::default()),
        );
        let area = Rect::new(0, 0, 3, 2);
        let mut buffer = Buffer::empty(area);
        pixels.render(area, Rect::default(), BACKGROUND, &mut buffer);
    }

    #[test]
    fn the_256_colour_palette_maps_rgb_into_the_cube_and_leaves_others() {
        assert_eq!(indexed(Color::Rgb(0, 0, 0)), Color::Indexed(16));
        assert_eq!(indexed(Color::Rgb(255, 255, 255)), Color::Indexed(231));
        assert_eq!(indexed(Color::Rgb(255, 0, 0)), Color::Indexed(196));
        assert_eq!(indexed(Color::Red), Color::Red);
    }
}
