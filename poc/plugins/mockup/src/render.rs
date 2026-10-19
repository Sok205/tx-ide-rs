//! Pure rendering: a [`Screen`] plus a [`ViewState`] into a ratatui [`Buffer`]. No terminal.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Direction, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, List, ListState, Paragraph, Row, StatefulWidget, Table, Widget};

use crate::markup::lines;
use crate::model::{Choice, Kind, Node, Screen, Size};

/// `tx_ui::render::ACCENT_ANSI`.
pub const ACCENT: Color = Color::Rgb(122, 162, 247);
/// `tx_ui::render::WARN_ANSI`.
pub const WARN: Color = Color::Rgb(224, 175, 104);

pub const HINT: &str = "1-9 pick · ←/→ move · tab layout · enter confirm · esc back";
pub const WAITING: &str = "continuing in terminal…";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layout {
    /// Options side by side.
    #[default]
    All,
    /// Only `options[focus]`, fullscreen.
    One,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewState {
    pub focus: usize,
    pub selected: Option<usize>,
    pub layout: Layout,
    pub error: Option<String>,
    pub waiting: bool,
}

pub fn draw(screen: &Screen, state: &ViewState, elapsed: Duration, area: Rect, buf: &mut Buffer) {
    if state.waiting {
        let top = area.y + area.height / 2;
        let line = Rect::new(area.x, top, area.width, area.height.min(1));
        Paragraph::new(Line::styled(
            WAITING,
            Style::new().add_modifier(Modifier::DIM),
        ))
        .alignment(Alignment::Center)
        .render(line, buf);
        return;
    }

    let mut header = Vec::new();
    if let Some(title) = &screen.title {
        header.push(Line::styled(
            title.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(sub) = &screen.sub {
        header.push(Line::styled(
            sub.clone(),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    let [header_area, middle, bottom] = ratatui::layout::Layout::vertical([
        Constraint::Length(header.len() as u16),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    Paragraph::new(Text::from(header)).render(header_area, buf);

    let bottom_line = match &state.error {
        Some(error) => Line::styled(error.clone(), Style::new().fg(WARN)),
        None => Line::raw(HINT),
    };
    Paragraph::new(bottom_line).render(bottom, buf);

    let context = Context {
        screen_fps: screen.fps,
        elapsed,
    };
    if screen.options.is_empty() {
        if let Some(body) = &screen.body {
            context.node(body, middle, buf);
        }
        return;
    }
    match state.layout {
        Layout::All => {
            let constraints = vec![Constraint::Fill(1); screen.options.len()];
            let areas = ratatui::layout::Layout::horizontal(constraints).split(middle);
            for (index, (choice, slot)) in screen.options.iter().zip(areas.iter()).enumerate() {
                context.option(state, index, choice, *slot, buf);
            }
        }
        Layout::One => {
            let index = state.focus.min(screen.options.len() - 1);
            context.option(state, index, &screen.options[index], middle, buf);
        }
    }
}

struct Context {
    screen_fps: u32,
    elapsed: Duration,
}

impl Context {
    fn option(
        &self,
        state: &ViewState,
        index: usize,
        choice: &Choice,
        area: Rect,
        buf: &mut Buffer,
    ) {
        let letter = (b'A' + (index % 26) as u8) as char;
        let selected = state.selected == Some(index);
        let mut title = format!("{letter} · {}", choice.title);
        if selected {
            title.push_str(" ✓");
        }
        let mut border = Style::new();
        if selected {
            border = border.fg(ACCENT);
        }
        if state.focus == index {
            border = border.add_modifier(Modifier::BOLD);
        }
        let block = Block::bordered().title(title).border_style(border);
        let inner = block.inner(area);
        block.render(area, buf);
        match &choice.note {
            Some(note) => {
                let [body_area, note_area] =
                    ratatui::layout::Layout::vertical([Constraint::Fill(1), Constraint::Length(1)])
                        .areas(inner);
                self.node(&choice.body, body_area, buf);
                Paragraph::new(Line::styled(
                    note.clone(),
                    Style::new().add_modifier(Modifier::DIM),
                ))
                .render(note_area, buf);
            }
            None => self.node(&choice.body, inner, buf),
        }
    }

    fn node(&self, node: &Node, area: Rect, buf: &mut Buffer) {
        match &node.kind {
            Kind::Row(children) => self.split(Direction::Horizontal, children, area, buf),
            Kind::Col(children) => self.split(Direction::Vertical, children, area, buf),
            Kind::Block { title, body } => {
                let block = Block::bordered().title(title.clone());
                let inner = block.inner(area);
                block.render(area, buf);
                self.node(body, inner, buf);
            }
            Kind::Text(text) => Paragraph::new(Text::from(lines(text))).render(area, buf),
            Kind::List { items, sel } => {
                let list = List::new(items.iter().map(|item| Text::from(lines(item))))
                    .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
                let mut list_state = ListState::default().with_selected(*sel);
                StatefulWidget::render(list, area, buf, &mut list_state);
            }
            Kind::Table { head, rows } => {
                let columns = head
                    .iter()
                    .chain(rows.iter())
                    .map(Vec::len)
                    .max()
                    .unwrap_or(0);
                let mut table = Table::new(
                    rows.iter()
                        .map(|cells| Row::new(cells.iter().map(|cell| Text::from(lines(cell))))),
                    vec![Constraint::Fill(1); columns],
                );
                if let Some(head) = head {
                    table = table.header(
                        Row::new(head.iter().map(|cell| Text::from(lines(cell))))
                            .style(Style::new().add_modifier(Modifier::BOLD)),
                    );
                }
                Widget::render(table, area, buf);
            }
            Kind::Frames { frames, fps } => {
                if frames.is_empty() {
                    return;
                }
                let fps = fps.unwrap_or(self.screen_fps);
                let index =
                    (self.elapsed.as_millis() * u128::from(fps) / 1000) % frames.len() as u128;
                Paragraph::new(Text::from(lines(&frames[index as usize]))).render(area, buf);
            }
        }
    }

    fn split(&self, direction: Direction, children: &[Node], area: Rect, buf: &mut Buffer) {
        let constraints: Vec<Constraint> = children
            .iter()
            .map(|child| constraint(child.size))
            .collect();
        let areas = ratatui::layout::Layout::new(direction, constraints).split(area);
        for (child, slot) in children.iter().zip(areas.iter()) {
            self.node(child, *slot, buf);
        }
    }
}

fn constraint(size: Size) -> Constraint {
    match size {
        Size::Len(n) => Constraint::Length(n),
        Size::Pct(n) => Constraint::Percentage(n),
        Size::Fill => Constraint::Fill(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::parse;
    use ratatui::style::{Color, Modifier};

    const WARN: Color = Color::Rgb(224, 175, 104);
    const HINT: &str = "1-9 pick";

    fn rows(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect()
            })
            .collect()
    }

    fn text(buf: &Buffer) -> String {
        rows(buf).join("\n")
    }

    /// (x, y) of the first cell of `needle` in the buffer.
    fn find(buf: &Buffer, needle: &str) -> (u16, u16) {
        for (y, row) in rows(buf).iter().enumerate() {
            if let Some(index) = row.find(needle) {
                let x = row[..index].chars().count();
                return (x as u16, y as u16);
            }
        }
        panic!("{needle:?} not found in:\n{}", text(buf));
    }

    fn render(json: &str, state: &ViewState, elapsed_ms: u64, w: u16, h: u16) -> Buffer {
        let screen = parse(json).unwrap();
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        draw(
            &screen,
            state,
            Duration::from_millis(elapsed_ms),
            area,
            &mut buf,
        );
        buf
    }

    const THREE_OPTIONS: &str = r#"{"title":"Pick one","sub":"closest wins","options":[
        {"id":"a","title":"Alpha","note":"first","body":{"text":"body a"}},
        {"id":"b","title":"Beta","body":{"text":"body b"}},
        {"id":"c","title":"Gamma","body":{"text":"body c"}}]}"#;

    #[test]
    fn title_and_hint_rendered() {
        let buf = render(THREE_OPTIONS, &ViewState::default(), 0, 80, 24);
        let out = text(&buf);
        assert!(out.contains("Pick one"), "{out}");
        assert!(out.contains("closest wins"), "{out}");
        assert!(rows(&buf)[23].contains(HINT), "{out}");
        let (x, y) = find(&buf, "Pick one");
        assert!(buf.cell((x, y)).unwrap().modifier.contains(Modifier::BOLD));
        let (x, y) = find(&buf, "closest wins");
        assert!(buf.cell((x, y)).unwrap().modifier.contains(Modifier::DIM));
    }

    #[test]
    fn error_replaces_hint() {
        let state = ViewState {
            error: Some("line 3 col 9: boom".into()),
            ..ViewState::default()
        };
        let buf = render(THREE_OPTIONS, &state, 0, 80, 24);
        let last = &rows(&buf)[23];
        assert!(last.contains("line 3 col 9: boom"), "{last}");
        assert!(!last.contains(HINT), "{last}");
        let (x, y) = find(&buf, "line 3 col 9");
        assert_eq!(buf.cell((x, y)).unwrap().fg, WARN);
    }

    #[test]
    fn all_layout_shows_every_option_title() {
        let buf = render(THREE_OPTIONS, &ViewState::default(), 0, 80, 24);
        let out = text(&buf);
        for title in ["A · Alpha", "B · Beta", "C · Gamma"] {
            assert!(out.contains(title), "{title} missing in:\n{out}");
        }
        assert!(out.contains("body a") && out.contains("body b") && out.contains("body c"));
        let (x, y) = find(&buf, "first");
        assert!(buf.cell((x, y)).unwrap().modifier.contains(Modifier::DIM));
    }

    #[test]
    fn one_layout_shows_only_focused() {
        let state = ViewState {
            focus: 1,
            layout: Layout::One,
            ..ViewState::default()
        };
        let buf = render(THREE_OPTIONS, &state, 0, 80, 24);
        let out = text(&buf);
        assert!(out.contains("B · Beta"), "{out}");
        assert!(!out.contains("Alpha"), "{out}");
        assert!(!out.contains("Gamma"), "{out}");
    }

    #[test]
    fn selected_option_has_check() {
        let state = ViewState {
            selected: Some(2),
            ..ViewState::default()
        };
        let buf = render(THREE_OPTIONS, &state, 0, 80, 24);
        let out = text(&buf);
        assert!(out.contains("C · Gamma ✓"), "{out}");
        assert!(!out.contains("Alpha ✓"), "{out}");
    }

    #[test]
    fn frames_advance_with_elapsed() {
        let json = r#"{"body":{"frames":["AA","BB"],"fps":2}}"#;
        let state = ViewState::default();
        let at_0 = text(&render(json, &state, 0, 80, 24));
        assert!(at_0.contains("AA") && !at_0.contains("BB"), "{at_0}");
        let at_600 = text(&render(json, &state, 600, 80, 24));
        assert!(at_600.contains("BB") && !at_600.contains("AA"), "{at_600}");
    }

    #[test]
    fn list_sel_is_reversed() {
        let json = r#"{"body":{"list":["one","two","three"],"sel":1}}"#;
        let buf = render(json, &ViewState::default(), 0, 80, 24);
        let (x, y) = find(&buf, "two");
        assert!(
            buf.cell((x, y))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
        let (x, y) = find(&buf, "one");
        assert!(
            !buf.cell((x, y))
                .unwrap()
                .modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn waiting_screen_text() {
        let state = ViewState {
            waiting: true,
            ..ViewState::default()
        };
        let buf = render(THREE_OPTIONS, &state, 0, 80, 24);
        let out = text(&buf);
        assert!(out.contains("continuing in terminal…"), "{out}");
        assert!(!out.contains("Alpha"), "{out}");
        assert!(!out.contains(HINT), "{out}");
        let (x, y) = find(&buf, "continuing");
        assert!(buf.cell((x, y)).unwrap().modifier.contains(Modifier::DIM));
        assert!(x > 20, "not centered: x={x}");
    }

    #[test]
    fn tiny_area_does_not_panic() {
        render(THREE_OPTIONS, &ViewState::default(), 0, 20, 5);
        render(THREE_OPTIONS, &ViewState::default(), 0, 0, 0);
        let one = ViewState {
            layout: Layout::One,
            ..ViewState::default()
        };
        render(THREE_OPTIONS, &one, 0, 20, 5);
        render(THREE_OPTIONS, &one, 0, 0, 0);
    }
}
