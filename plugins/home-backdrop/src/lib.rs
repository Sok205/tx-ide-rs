//! The extension point a themes plugin uses to paint an animated background and logo under the
//! home TUI. Home draws its text on top of whatever the backdrop painted.

use std::sync::OnceLock;
use std::time::Duration;

use ratatui::buffer::{Buffer, Cell};
use ratatui::crossterm::event::MouseEvent;
use ratatui::layout::Rect;
use ratatui::style::Color;
use tx_client::palette::palette;

pub trait Backdrop {
    fn frames_per_second(&self) -> u64;
    /// (width, height) of this theme's logo in cells.
    fn logo_size(&self) -> (u16, u16);
    /// `elapsed`: time since the backdrop was built, less the time the pane spent unfocused.
    /// `logo`: the box home reserved, if any. `text`: what home will draw over `area`; its
    /// [`is_text`] cells take home's text on top.
    fn paint(
        &mut self,
        elapsed: Duration,
        area: Rect,
        logo: Option<Rect>,
        text: &Buffer,
        buffer: &mut Buffer,
    );
    fn mouse(&mut self, event: MouseEvent);
}

pub type Constructor = fn() -> Box<dyn Backdrop>;
pub type Theme = (&'static str, Constructor);
/// Called with the chosen theme's name, `None` for off.
pub type Remember = Box<dyn Fn(Option<&str>) + Send + Sync>;

pub struct Catalogue {
    pub themes: &'static [Theme],
    /// The theme home starts with; `None` is off.
    pub start: Option<usize>,
    pub remember: Remember,
}

static CATALOGUE: OnceLock<Catalogue> = OnceLock::new();

/// The first install wins: one `tx` process runs `register` twice (`preview_verbs`, then `open`).
pub fn install(catalogue: Catalogue) {
    let _ = CATALOGUE.set(catalogue);
}

pub fn catalogue() -> Option<&'static Catalogue> {
    CATALOGUE.get()
}

/// The next position after `current`: 0, 1, …, len-1, then `None` (off), then 0.
pub fn next(current: Option<usize>, len: usize) -> Option<usize> {
    let following = current.map_or(0, |index| index + 1);
    (following < len).then_some(following)
}

/// Whether home drew this cell: a glyph, or a background such as the cursor bar.
pub fn is_text(cell: &Cell) -> bool {
    cell.symbol() != " " || cell.bg != Color::Reset
}

pub const MINIMUM_LIST_ROWS: u16 = 10;

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

/// Scales the channels of an `Rgb` colour; other colours pass through.
pub fn dim(color: Color, factor: f32) -> Color {
    let Color::Rgb(red, green, blue) = color else {
        return color;
    };
    let scale = |channel: u8| (f32::from(channel) * factor).round() as u8;
    Color::Rgb(scale(red), scale(green), scale(blue))
}

/// `color` scaled down, if needed, to be no brighter than `ceiling`; non-`Rgb` colours pass
/// through.
pub fn no_brighter_than(color: Color, ceiling: Color) -> Color {
    let (lightness, limit) = (luminance(color), luminance(ceiling));
    if lightness > limit {
        dim(color, limit / lightness)
    } else {
        color
    }
}

fn luminance(color: Color) -> f32 {
    let Color::Rgb(red, green, blue) = color else {
        return 0.0;
    };
    0.2126 * f32::from(red) + 0.7152 * f32::from(green) + 0.0722 * f32::from(blue)
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

    /// Half-blocks (a space where both halves match), except background-only spaces under home's
    /// `text` cells, no brighter than `text_ceiling` so the text stays readable over bright pixels.
    pub fn render(
        &self,
        area: Rect,
        text: &Buffer,
        background: Color,
        text_ceiling: Color,
        buffer: &mut Buffer,
    ) {
        for row in 0..area.height {
            for column in 0..area.width {
                let (x, y) = (area.x + column, area.y + row);
                let top = self.get(i32::from(column), i32::from(row) * 2, background);
                let bottom = self.get(i32::from(column), i32::from(row) * 2 + 1, background);
                let cell = &mut buffer[(x, y)];
                if is_text(&text[(x, y)]) {
                    cell.set_char(' ')
                        .set_bg(palette(no_brighter_than(mean(top, bottom), text_ceiling)));
                } else if top == bottom {
                    // Looks the same as a half-block, and spares the terminal a colour.
                    cell.set_char(' ').set_bg(palette(bottom));
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
    const CEILING: Color = Color::Rgb(40, 40, 40);

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
        static FIRST: [Theme; 1] = [("a", a)];
        static SECOND: [Theme; 1] = [("b", b)];
        let catalogue = |themes| Catalogue {
            themes,
            start: Some(0),
            remember: Box::new(|_| {}),
        };
        install(catalogue(&FIRST));
        install(catalogue(&SECOND));
        assert_eq!(super::catalogue().unwrap().themes[0].0, "a");
    }

    #[test]
    fn next_cycles_through_every_theme_then_off() {
        let mut position = None;
        let mut sequence = Vec::new();
        for _ in 0..5 {
            position = next(position, 3);
            sequence.push(position);
        }
        assert_eq!(sequence, [Some(0), Some(1), Some(2), None, Some(0)]);
        assert_eq!(next(None, 0), None);
        assert_eq!(next(Some(0), 0), None);
    }

    #[test]
    fn pixels_outside_text_are_half_blocks() {
        let (pixels, area, mut buffer) = two_pixels();
        pixels.render(area, &Buffer::empty(area), BACKGROUND, CEILING, &mut buffer);
        let cell = &buffer[(0, 0)];
        assert_eq!((cell.symbol(), cell.fg, cell.bg), ("▀", RED, BLUE));
        let even = &buffer[(1, 0)];
        assert_eq!((even.symbol(), even.bg), (" ", BACKGROUND));
    }

    #[test]
    fn pixels_under_text_cells_are_spaces_capped_at_the_ceiling() {
        let (pixels, area, mut buffer) = two_pixels();
        let mut text = Buffer::empty(area);
        text[(0, 0)].set_char('a');
        text[(1, 0)].set_char('b');
        pixels.render(area, &text, BACKGROUND, CEILING, &mut buffer);
        let bright = &buffer[(0, 0)];
        assert_eq!(bright.symbol(), " ");
        assert_eq!(
            bright.bg,
            no_brighter_than(mean(RED, BLUE), CEILING),
            "brighter than the ceiling: scaled down"
        );
        assert!(luminance(bright.bg) <= luminance(CEILING) + 1.0);
        assert_eq!(buffer[(1, 0)].bg, BACKGROUND, "darker water stays as it is");
    }

    #[test]
    fn a_glyph_or_a_background_is_text() {
        assert!(!is_text(&Cell::EMPTY));
        assert!(is_text(Cell::EMPTY.clone().set_char('a')));
        assert!(is_text(Cell::EMPTY.clone().set_bg(Color::Red)));
        assert!(!is_text(Cell::EMPTY.clone().set_fg(Color::Red)));
    }

    #[test]
    fn pixels_ignore_out_of_range_and_tiny_areas() {
        let mut pixels = Pixels::new(2, 1);
        pixels.set(-1, 0, RED);
        pixels.set(99, 99, RED);
        let mut empty = Pixels::new(0, 0);
        empty.set(0, 0, RED);
        let nothing = Buffer::empty(Rect::default());
        empty.render(
            Rect::default(),
            &nothing,
            BACKGROUND,
            CEILING,
            &mut Buffer::empty(Rect::default()),
        );
        let area = Rect::new(0, 0, 3, 2);
        let mut buffer = Buffer::empty(area);
        pixels.render(area, &Buffer::empty(area), BACKGROUND, CEILING, &mut buffer);
    }
}
