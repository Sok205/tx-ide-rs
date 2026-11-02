//! Drawing: the main list (a placeholder until Task 8) and the plugins / keys sub-lists,
//! rendered into a [`Buffer`].

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use crate::model::{Home, View};

pub const ACCENT: Color = Color::Rgb(122, 162, 247);
pub const INFO: Color = Color::Rgb(125, 207, 255);
pub const WARN: Color = Color::Rgb(224, 175, 104);
pub const FG: Color = Color::Rgb(192, 202, 245);
pub const DIM_FG: Color = Color::Rgb(169, 177, 214);

/// The loop's redraw rate.
pub const FPS: u64 = 6;

fn dim() -> Style {
    Style::new().fg(DIM_FG).add_modifier(Modifier::DIM)
}

/// Writes `line` at row `y` when the row is inside `area`.
fn put(buf: &mut Buffer, area: Rect, x: u16, y: u16, line: &Line<'_>, width: u16) {
    if y < area.bottom() && x < area.right() {
        buf.set_line(x, y, line, width.min(area.right() - x));
    }
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() > width {
        text.chars()
            .take(width.saturating_sub(1))
            .collect::<String>()
            + "…"
    } else {
        text.to_owned()
    }
}

pub fn draw(home: &Home, area: Rect, buf: &mut Buffer) {
    match home.view() {
        View::Main => draw_main(home, area, buf),
        View::Plugins(row) => draw_plugins(home, row, area, buf),
        View::Keys(row) => draw_keys(home, row, area, buf),
    }
}

fn draw_main(home: &Home, area: Rect, buf: &mut Buffer) {
    let width = usize::from(area.width);
    let placeholder = Line::styled(truncate("home: list comes in Task 8", width), dim());
    put(buf, area, area.x, area.y, &placeholder, area.width);
    let footer = Line::styled(truncate(&home.footer(), width), dim());
    let bottom = area.bottom().saturating_sub(1).max(area.y + 1);
    put(buf, area, area.x, bottom, &footer, area.width);
}

/// A bordered full-area list with an optional header row, a selected (reversed) or merely
/// scrolled-to row, and a hint at the bottom.
struct List<'a> {
    title: &'a str,
    header: Option<Line<'static>>,
    rows: Vec<Line<'static>>,
    row: usize,
    highlight: bool,
    hint: &'a str,
}

fn draw_list(list: &List<'_>, area: Rect, buf: &mut Buffer) {
    let List {
        title,
        header,
        rows,
        row,
        highlight,
        hint,
    } = list;
    let (row, highlight) = (*row, *highlight);
    let block = Block::bordered()
        .title(format!(" {title} "))
        .border_style(Style::new().fg(ACCENT));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 2 {
        return;
    }
    let mut y = inner.y;
    if let Some(header) = &header {
        put(buf, inner, inner.x, y, header, inner.width);
        y += 1;
    }
    let body = inner.bottom().saturating_sub(y + 1) as usize;
    if rows.is_empty() {
        put(
            buf,
            inner,
            inner.x,
            y,
            &Line::styled("(none)", dim()),
            inner.width,
        );
    }
    let first = if body == 0 {
        row
    } else {
        row.saturating_sub(body - 1)
    };
    for (index, line) in rows.iter().enumerate().skip(first).take(body) {
        put(buf, inner, inner.x, y, line, inner.width);
        if highlight && index == row {
            buf.set_style(
                Rect::new(inner.x, y, inner.width, 1),
                Style::new().add_modifier(Modifier::REVERSED),
            );
        }
        y += 1;
    }
    put(
        buf,
        inner,
        inner.x,
        inner.bottom() - 1,
        &Line::styled(hint.to_string(), dim()),
        inner.width,
    );
}

fn draw_plugins(home: &Home, row: usize, area: Rect, buf: &mut Buffer) {
    let idw = home
        .context
        .plugins
        .iter()
        .map(|(id, _)| id.len())
        .max()
        .unwrap_or(0);
    let rows: Vec<Line<'static>> = home
        .context
        .plugins
        .iter()
        .map(|(id, active)| {
            let (status, style) = if *active {
                ("active", Style::new().fg(ACCENT))
            } else {
                ("disabled", dim())
            };
            Line::from(vec![
                Span::styled(format!("{id:<idw$}  "), Style::new().fg(FG)),
                Span::styled(status, style),
            ])
        })
        .collect();
    let list = List {
        title: "plugins",
        header: None,
        rows,
        row,
        highlight: false,
        hint: "read-only · plugins.yaml switches them · esc back",
    };
    draw_list(&list, area, buf);
}

fn draw_keys(home: &Home, row: usize, area: Rect, buf: &mut Buffer) {
    let keys: Vec<String> = home
        .context
        .keys
        .iter()
        .map(|hint| hint.keys.join(" "))
        .collect();
    let keyw = keys.iter().map(String::len).max().unwrap_or(0);
    let idw = home
        .context
        .keys
        .iter()
        .map(|hint| hint.plugin.len())
        .max()
        .unwrap_or(0);
    let rows: Vec<Line<'static>> = home
        .context
        .keys
        .iter()
        .zip(&keys)
        .map(|(hint, keys)| {
            Line::from(vec![
                Span::styled(format!("{keys:<keyw$}  "), Style::new().fg(ACCENT)),
                Span::styled(format!("{:<idw$}  ", hint.plugin), Style::new().fg(WARN)),
                Span::styled(hint.text.clone(), Style::new().fg(FG)),
            ])
        })
        .collect();
    let list = List {
        title: "keys",
        header: None,
        rows,
        row,
        highlight: false,
        hint: "j/k scroll · esc back",
    };
    draw_list(&list, area, buf);
}

#[cfg(test)]
mod tests {
    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::model::tests::context;

    const W: u16 = 60;
    const H: u16 = 30;

    fn render(home: &Home) -> Buffer {
        let area = Rect::new(0, 0, W, H);
        let mut buf = Buffer::empty(area);
        draw(home, area, &mut buf);
        buf
    }

    fn text(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    fn row_of(lines: &[String], needle: &str) -> usize {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not drawn:\n{}", lines.join("\n")))
    }

    #[test]
    fn plugins_view_marks_disabled_ones() {
        let mut home = Home::new(context());
        home.key(KeyCode::Char('p'));
        let lines = text(&render(&home));
        let status = |id: &str| {
            lines[row_of(&lines, id)]
                .replace('│', "")
                .trim_end()
                .to_owned()
        };
        assert!(
            status("spawn").ends_with("spawn  active"),
            "{}",
            status("spawn")
        );
        assert!(
            status("kill").ends_with("kill   disabled"),
            "{}",
            status("kill")
        );
    }

    #[test]
    fn keys_view_shows_the_hints() {
        let mut home = Home::new(context());
        home.key(KeyCode::Char('K'));
        let lines = text(&render(&home));
        let row = row_of(&lines, "kill");
        assert!(lines[row].contains("X  kill  stop"), "{}", lines[row]);
    }

    #[test]
    fn a_tiny_area_never_panics() {
        let home = Home::new(context());
        for (w, h) in [(1, 1), (10, 3), (44, 8), (3, 40)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            draw(&home, area, &mut buf);
        }
        let mut home = Home::new(context());
        home.key(KeyCode::Char('p'));
        for (w, h) in [(1, 1), (10, 3), (44, 4)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            draw(&home, area, &mut buf);
        }
    }
}
