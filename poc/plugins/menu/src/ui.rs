//! Drawing: the title screen (operator's look "A": the shimmering `TX` logo, the menu, the
//! recent box) and the history / plugins / keys sub-lists, rendered into a [`Buffer`].
//! `frame` picks the logo's shimmer frame; the loop advances it at [`FPS`].

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};
use tx_client::render::reltime;

use crate::model::{Context, Item, Menu, View};

pub const ACCENT: Color = Color::Rgb(122, 162, 247);
pub const INFO: Color = Color::Rgb(125, 207, 255);
pub const WARN: Color = Color::Rgb(224, 175, 104);
pub const FG: Color = Color::Rgb(192, 202, 245);
pub const DIM_FG: Color = Color::Rgb(169, 177, 214);

/// The shimmer's frame rate (the mockup's `fps`).
pub const FPS: u64 = 6;

/// The block-letter logo, verbatim from the mockup.
pub const LOGO: [&str; 6] = [
    "  ████████╗██╗  ██╗",
    "  ╚══██╔══╝╚██╗██╔╝",
    "     ██║    ╚███╔╝ ",
    "     ██║    ██╔██╗ ",
    "     ██║   ██╔╝ ██╗",
    "     ╚═╝   ╚═╝  ╚═╝",
];
const IDE: &str = "ide";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shade {
    Dim,
    Info,
    Accent,
}
use Shade::{Accent as A, Dim as D, Info as I};

/// Per frame, the shade of each logo line: the mockup's five frames, a lit line with a fading
/// trail running down the logo.
const FRAMES: [[Shade; 6]; 5] = [
    [A, A, D, D, D, D],
    [D, I, A, D, D, D],
    [D, D, I, A, D, D],
    [D, D, D, I, A, D],
    [D, D, D, D, I, A],
];
/// The frame in which `ide` lights up with the last line.
const IDE_FRAME: usize = 4;

/// The logo's rows plus the blank row under it (the mockup's `size: 7`).
const LOGO_ROWS: u16 = 7;
/// The title screen's column width.
const WIDTH: u16 = 44;
/// Label column of a menu line, after the two-space indent.
const LABEL_W: usize = 22;

fn dim() -> Style {
    Style::new().fg(DIM_FG).add_modifier(Modifier::DIM)
}

fn shade(shade: Shade) -> Style {
    match shade {
        Shade::Dim => dim(),
        Shade::Info => Style::new().fg(INFO),
        Shade::Accent => Style::new().fg(ACCENT),
    }
}

/// Logo line `row` in shimmer frame `frame` (looping).
pub fn logo_line(row: usize, frame: usize) -> Line<'static> {
    let frame = frame % FRAMES.len();
    let mut spans = vec![Span::styled(LOGO[row], shade(FRAMES[frame][row]))];
    if row == LOGO.len() - 1 {
        let ide = if frame == IDE_FRAME {
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            dim()
        };
        spans.push(Span::raw("  "));
        spans.push(Span::styled(IDE, ide));
    }
    Line::from(spans)
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

pub fn draw(menu: &Menu, frame: usize, area: Rect, buf: &mut Buffer) {
    match menu.view() {
        View::Main => draw_main(menu, frame, area, buf),
        View::History(row) => draw_history(menu, row, area, buf),
        View::Plugins(row) => draw_plugins(menu, row, area, buf),
        View::Keys(row) => draw_keys(menu, row, area, buf),
    }
}

/// The menu's lines: a dim header before each section, then its items with the key at the right.
fn menu_lines(menu: &Menu) -> Vec<(Line<'static>, bool)> {
    let mut lines = Vec::new();
    let mut section = "";
    for item in menu.items() {
        if item.section() != section {
            section = item.section();
            let mut header = vec![Span::styled(section, dim())];
            if section == Item::Attach.section() {
                header.push(Span::styled(
                    format!("  {}", running_counts(&menu.ctx)),
                    Style::new().fg(DIM_FG),
                ));
            }
            lines.push((Line::from(header), false));
        }
        let label = format!("  {:<LABEL_W$}", item.label());
        let selected = *item == menu.selected();
        lines.push((
            Line::from(vec![
                Span::styled(label, Style::new().fg(FG)),
                Span::styled(item.key().to_string(), dim()),
            ]),
            selected,
        ));
    }
    lines
}

/// `2 agents · 1 view`: what the picker (Attach to running) lists, agents and views apart.
fn running_counts(ctx: &Context) -> String {
    let plural = |count: usize, noun: &str| {
        let s = if count == 1 { "" } else { "s" };
        format!("{count} {noun}{s}")
    };
    format!(
        "{} · {}",
        plural(ctx.agents, "agent"),
        plural(ctx.views, "view")
    )
}

fn recent_lines(menu: &Menu) -> Vec<Line<'static>> {
    let recent = menu.recent();
    if recent.is_empty() {
        return vec![Line::styled("(no past sessions)", dim())];
    }
    let namew = recent
        .iter()
        .map(|past| past.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(24);
    recent
        .iter()
        .map(|past| {
            let age = reltime(Some(past.at), menu.ctx.now);
            let meta = if past.tag.is_empty() {
                age
            } else {
                format!("{} · {age}", past.tag)
            };
            Line::from(vec![
                Span::styled(
                    format!("{:<namew$}", truncate(&past.name, namew)),
                    Style::new().fg(WARN),
                ),
                Span::raw("  "),
                Span::styled(meta, dim()),
            ])
        })
        .collect()
}

fn draw_main(menu: &Menu, frame: usize, area: Rect, buf: &mut Buffer) {
    let width = WIDTH.min(area.width);
    let x = area.x + (area.width - width) / 2;
    let items = menu_lines(menu);
    let resumable = menu.items().contains(&Item::History);
    let recent = resumable.then(|| recent_lines(menu));
    let recent_rows = recent.as_ref().map_or(0, |lines| lines.len() as u16 + 2);
    let total = LOGO_ROWS + items.len() as u16 + 1 + recent_rows + 1 + 1;
    let mut y = area.y + area.height.saturating_sub(total) / 2;

    for row in 0..LOGO.len() {
        put(buf, area, x, y, &logo_line(row, frame), width);
        y += 1;
    }
    y += LOGO_ROWS - LOGO.len() as u16;

    for (line, selected) in &items {
        put(buf, area, x, y, line, width);
        if *selected && y < area.bottom() {
            buf.set_style(
                Rect::new(x, y, width, 1),
                Style::new().add_modifier(Modifier::REVERSED),
            );
        }
        y += 1;
    }
    y += 1;

    if let Some(lines) = &recent {
        let block_area = Rect::new(x, y, width, recent_rows).intersection(area);
        let block = Block::bordered()
            .title("recent")
            .border_style(Style::new().fg(DIM_FG));
        let inner = block.inner(block_area);
        block.render(block_area, buf);
        for (offset, line) in lines.iter().enumerate() {
            put(
                buf,
                inner,
                inner.x,
                inner.y + offset as u16,
                line,
                inner.width,
            );
        }
        y += recent_rows;
    }
    y += 1;

    let hint = if resumable && !menu.recent().is_empty() {
        "j/k move · enter run · 1-3 recent · q quit"
    } else {
        "j/k move · enter run · q quit"
    };
    put(buf, area, x, y, &Line::styled(hint, dim()), width);
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

const NAME_W: usize = 24;
const TAG_W: usize = 14;

fn draw_history(menu: &Menu, row: usize, area: Rect, buf: &mut Buffer) {
    let header = Line::styled(
        format!(
            "{:<NAME_W$} {:<TAG_W$} {:>5} {:>5}  cwd",
            "name", "tag", "ended", "chats"
        ),
        dim(),
    );
    let rows: Vec<Line<'static>> = menu
        .ctx
        .history
        .iter()
        .map(|past| {
            Line::from(vec![
                Span::styled(
                    format!("{:<NAME_W$}", truncate(&past.name, NAME_W)),
                    Style::new().fg(WARN),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<TAG_W$}", truncate(&past.tag, TAG_W)),
                    Style::new().fg(FG),
                ),
                Span::styled(
                    format!(
                        " {:>5} {:>4}c  {}",
                        reltime(Some(past.at), menu.ctx.now),
                        past.chats,
                        past.cwd
                    ),
                    dim(),
                ),
            ])
        })
        .collect();
    let list = List {
        title: "history",
        header: Some(header),
        rows,
        row,
        highlight: true,
        hint: "j/k move · enter resume · esc back",
    };
    draw_list(&list, area, buf);
}

fn draw_plugins(menu: &Menu, row: usize, area: Rect, buf: &mut Buffer) {
    let idw = menu
        .ctx
        .plugins
        .iter()
        .map(|(id, _)| id.len())
        .max()
        .unwrap_or(0);
    let rows: Vec<Line<'static>> = menu
        .ctx
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

fn draw_keys(menu: &Menu, row: usize, area: Rect, buf: &mut Buffer) {
    let keys: Vec<String> = menu
        .ctx
        .keys
        .iter()
        .map(|hint| hint.keys.join(" "))
        .collect();
    let keyw = keys.iter().map(String::len).max().unwrap_or(0);
    let idw = menu
        .ctx
        .keys
        .iter()
        .map(|hint| hint.plugin.len())
        .max()
        .unwrap_or(0);
    let rows: Vec<Line<'static>> = menu
        .ctx
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

    fn render(menu: &Menu, frame: usize) -> Buffer {
        let area = Rect::new(0, 0, W, H);
        let mut buf = Buffer::empty(area);
        draw(menu, frame, area, &mut buf);
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

    /// The fg of the first non-blank cell on `row`.
    fn fg_at(buf: &Buffer, row: usize) -> Color {
        let y = row as u16;
        (0..buf.area.width)
            .map(|x| &buf[(x, y)])
            .find(|cell| cell.symbol() != " ")
            .map(|cell| cell.fg)
            .unwrap()
    }

    #[test]
    fn title_screen_snapshot() {
        let menu = Menu::new(context());
        let lines = text(&render(&menu, 0));
        let expected = [
            "  ████████╗██╗  ██╗",
            "  ╚══██╔══╝╚██╗██╔╝",
            "     ██║    ╚███╔╝",
            "     ██║    ██╔██╗",
            "     ██║   ██╔╝ ██╗",
            "     ╚═╝   ╚═╝  ╚═╝  ide",
            "",
            "sessions  2 agents · 1 view",
            "  New tx session        n",
            "  Attach to running     t",
            "  Resume from history   h",
            "  Fork this session     f",
            "look around",
            "  Timeline              T",
            "  Artifacts             a",
            "setup",
            "  Plugins               p",
            "  Keys cheat sheet      K",
            "learn",
            "  Tutor                 ?",
            "  Assistant             /",
            "  Quit                  q",
            "",
            "┌recent────────────────────────────────────┐",
            "│mockup-t5-verbs  tx-mockup · 11m          │",
            "│fixer            agent · 57m              │",
            "│tx-demo-ai       tx-demo · 1h             │",
            "└──────────────────────────────────────────┘",
            "",
            "j/k move · enter run · 1-3 recent · q quit",
        ];
        // 60 columns, 44 wide, centred: an 8-column margin; the 30 rows are filled exactly.
        let drawn: Vec<&str> = lines
            .iter()
            .map(|line| line.get(8..).unwrap_or(""))
            .collect();
        assert_eq!(drawn, expected, "\n{}", lines.join("\n"));
        assert!(
            lines
                .iter()
                .all(|line| line.get(..8).is_none_or(|m| m.trim().is_empty()))
        );
    }

    #[test]
    fn the_shimmer_moves_between_frames() {
        let menu = Menu::new(context());
        let (zero, one) = (render(&menu, 0), render(&menu, 1));
        let top = row_of(&text(&zero), "████████╗");
        let shades =
            |buf: &Buffer| -> Vec<Color> { (0..6).map(|row| fg_at(buf, top + row)).collect() };
        assert_eq!(
            shades(&zero),
            [ACCENT, ACCENT, DIM_FG, DIM_FG, DIM_FG, DIM_FG]
        );
        assert_eq!(shades(&one), [DIM_FG, INFO, ACCENT, DIM_FG, DIM_FG, DIM_FG]);
        assert_eq!(
            shades(&render(&menu, 4)),
            [DIM_FG, DIM_FG, DIM_FG, DIM_FG, INFO, ACCENT]
        );
        assert_eq!(shades(&render(&menu, 5)), shades(&zero), "it loops");
    }

    #[test]
    fn ide_lights_up_with_the_last_line_only() {
        let ide = |frame| {
            let line = logo_line(5, frame);
            line.spans.last().unwrap().style
        };
        assert_eq!(ide(4).fg, Some(ACCENT));
        assert!(ide(4).add_modifier.contains(Modifier::BOLD));
        assert_eq!(ide(0).fg, Some(DIM_FG));
    }

    #[test]
    fn the_selected_line_is_reversed() {
        let mut menu = Menu::new(context());
        menu.key(KeyCode::Down);
        let buf = render(&menu, 0);
        let lines = text(&buf);
        let reversed = |row: usize| {
            buf[(lines[row].find('A').unwrap() as u16, row as u16)]
                .modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed(row_of(&lines, "Attach to running")));
        let new = row_of(&lines, "New tx session");
        assert!(!buf[(20, new as u16)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn hidden_items_and_the_recent_box_disappear() {
        let mut ctx = context();
        ctx.active.remove("chat");
        let lines = text(&render(&Menu::new(ctx), 0));
        let joined = lines.join("\n");
        assert!(!joined.contains("Resume from history"), "{joined}");
        assert!(!joined.contains("Fork this session"), "{joined}");
        assert!(!joined.contains("recent"), "{joined}");
        assert!(!joined.contains("1-3"), "{joined}");
        assert!(joined.contains("New tx session"), "{joined}");
    }

    #[test]
    fn empty_history_says_so_in_the_recent_box() {
        let mut ctx = context();
        ctx.history.clear();
        let joined = text(&render(&Menu::new(ctx), 0)).join("\n");
        assert!(joined.contains("(no past sessions)"), "{joined}");
        assert!(!joined.contains("1-3"), "{joined}");
    }

    #[test]
    fn history_view_lists_every_past_session() {
        let mut menu = Menu::new(context());
        menu.key(KeyCode::Char('h'));
        menu.key(KeyCode::Char('j'));
        let buf = render(&menu, 0);
        let lines = text(&buf);
        assert!(lines[0].contains(" history "), "{}", lines[0]);
        row_of(&lines, "name");
        let fixer = row_of(&lines, "fixer");
        assert!(lines[fixer].contains("agent"), "{}", lines[fixer]);
        assert!(lines[fixer].contains("57m"), "{}", lines[fixer]);
        assert!(buf[(1, fixer as u16)].modifier.contains(Modifier::REVERSED));
        row_of(&lines, "old");
        row_of(&lines, "enter resume");
    }

    #[test]
    fn plugins_view_marks_disabled_ones() {
        let mut menu = Menu::new(context());
        menu.key(KeyCode::Char('p'));
        let lines = text(&render(&menu, 0));
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
        let mut menu = Menu::new(context());
        menu.key(KeyCode::Char('K'));
        let lines = text(&render(&menu, 0));
        let row = row_of(&lines, "kill");
        assert!(lines[row].contains("X  kill  stop"), "{}", lines[row]);
    }

    #[test]
    fn a_tiny_area_never_panics() {
        let menu = Menu::new(context());
        for (w, h) in [(1, 1), (10, 3), (44, 8), (3, 40)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            draw(&menu, 0, area, &mut buf);
        }
        let mut menu = Menu::new(context());
        menu.key(KeyCode::Char('h'));
        for (w, h) in [(1, 1), (10, 3), (44, 4)] {
            let area = Rect::new(0, 0, w, h);
            let mut buf = Buffer::empty(area);
            draw(&menu, 0, area, &mut buf);
        }
    }

    #[test]
    fn the_sessions_header_counts_agents_and_views_apart() {
        let mut ctx = context();
        assert_eq!(running_counts(&ctx), "2 agents · 1 view");
        ctx.agents = 1;
        ctx.views = 0;
        assert_eq!(running_counts(&ctx), "1 agent · 0 views");
    }
}
