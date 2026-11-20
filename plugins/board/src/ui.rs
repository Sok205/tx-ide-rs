//! Drawing the board: a header, the three columns, the detail drawer of the selected card and a
//! footer of keys (or the notice). Colours go through the tx palette.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use tx_client::palette::{self, palette};
use tx_core::LlmEngineState;

use super::app::App;
use super::github::{Issue, age};
use super::model::{Card, CardAgent, ColumnKind};

/// Each card takes two rows: the number and title, then labels and agents.
pub const CARD_ROWS: u16 = 2;
const CURSOR_BG: Color = Color::Indexed(236);
const KEYS: &str = "j/k h/l move  s start agent  Enter show agent  o browser  c comment  n new  x close  m assign  r refresh  ? keys  q quit";

fn accent() -> Style {
    Style::new().fg(palette(palette::ACCENT))
}

fn dim() -> Style {
    Style::new()
        .fg(palette(palette::DIM_FG))
        .add_modifier(Modifier::DIM)
}

fn text() -> Style {
    Style::new().fg(palette(palette::FG))
}

fn label_style() -> Style {
    Style::new().fg(palette(palette::WARN))
}

pub fn state_style(state: LlmEngineState) -> Style {
    match state {
        LlmEngineState::Working => Style::new().fg(palette(palette::GREEN)),
        LlmEngineState::Starting => Style::new().fg(palette(palette::WARN)),
        LlmEngineState::Idle => Style::new().fg(palette(palette::INFO)),
        LlmEngineState::Ended => dim(),
    }
}

fn state_mark(state: LlmEngineState) -> &'static str {
    match state {
        LlmEngineState::Working => "●",
        LlmEngineState::Starting => "◐",
        LlmEngineState::Idle => "○",
        LlmEngineState::Ended => "×",
    }
}

/// `text` cut to `width` cells, with `…` when it was longer.
pub fn truncate(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut cut: String = text.chars().take(width - 1).collect();
    cut.push('…');
    cut
}

pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    if area.height < 6 || area.width < 20 {
        return;
    }
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .areas(area);
    let drawer_rows = (body.height * 45 / 100)
        .max(5)
        .min(body.height.saturating_sub(4));
    let [columns, drawer] =
        Layout::vertical([Constraint::Min(4), Constraint::Length(drawer_rows)]).areas(body);
    let visible = usize::from(columns.height.saturating_sub(2) / CARD_ROWS);
    app.keep_cursor_visible(visible);

    frame.render_widget(header_line(app), header);
    let thirds = Layout::horizontal([Constraint::Ratio(1, 3); 3]).split(columns);
    for (index, kind) in ColumnKind::ALL.into_iter().enumerate() {
        draw_column(app, kind, thirds[index], frame);
    }
    draw_drawer(app, drawer, frame);
    frame.render_widget(footer_line(app, footer.width), footer);
}

fn header_line(app: &App) -> Line<'static> {
    let fetched = match (app.refreshing, app.fetched_at) {
        (true, _) => "refreshing…".to_owned(),
        (false, Some(at)) => format!("refreshed {}", ago(app.now - at)),
        (false, None) => "not fetched".to_owned(),
    };
    Line::from(vec![
        Span::styled(" tx-board ", accent().add_modifier(Modifier::BOLD)),
        Span::styled(app.board.repository.slug(), text()),
        Span::styled(format!("   {fetched}"), dim()),
    ])
}

fn ago(seconds: f64) -> String {
    let seconds = seconds.max(0.0) as u64;
    if seconds < 60 {
        format!("{seconds}s ago")
    } else {
        format!("{}m ago", seconds / 60)
    }
}

fn draw_column(app: &App, kind: ColumnKind, area: Rect, frame: &mut Frame) {
    let index = kind as usize;
    let cards = app.column_of(kind);
    let active = app.column == index;
    let border = if active {
        accent()
    } else {
        Style::new().fg(palette(palette::BORDER_DIM))
    };
    let title = format!(" {} {} ", kind.title(), cards.len());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Span::styled(
            title,
            if active {
                accent().add_modifier(Modifier::BOLD)
            } else {
                dim()
            },
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 4 {
        return;
    }
    let visible = usize::from(inner.height / CARD_ROWS);
    let first = app.scroll[index];
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (row, card) in cards.iter().enumerate().skip(first).take(visible) {
        let selected = active && row == app.cursor[index];
        let mut head = card_head(card, usize::from(inner.width));
        let mut tail = card_tail(card, usize::from(inner.width));
        if selected {
            for line in [&mut head, &mut tail] {
                line.style = Style::new().bg(CURSOR_BG);
            }
        }
        lines.push(head);
        lines.push(tail);
    }
    if cards.len() > first + visible {
        let more = cards.len() - first - visible;
        if let Some(last) = lines.last_mut() {
            *last = Line::from(Span::styled(format!("  … {more} more"), dim()));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn card_head(card: &Card, width: usize) -> Line<'static> {
    let number = format!("#{} ", card.issue.number);
    let title = truncate(&card.issue.title, width.saturating_sub(number.len() + 1));
    Line::from(vec![
        Span::styled(" ", text()),
        Span::styled(number, text().add_modifier(Modifier::BOLD)),
        Span::styled(title, text()),
    ])
}

fn card_tail(card: &Card, width: usize) -> Line<'static> {
    let mut spans = vec![Span::styled("   ", text())];
    let mut used = 3;
    for label in &card.issue.labels {
        let word = format!("{} ", label.name);
        if used + word.len() > width {
            break;
        }
        used += word.len();
        spans.push(Span::styled(word, label_style()));
    }
    if let Some(agent) = card.agents.first() {
        let word = format!("{} {}", state_mark(agent.state), agent.name);
        let word = truncate(&word, width.saturating_sub(used));
        spans.push(Span::styled(word, state_style(agent.state)));
    } else if let Some(assignee) = card.issue.assignees.first() {
        let word = truncate(&format!("@{}", assignee.login), width.saturating_sub(used));
        spans.push(Span::styled(word, dim()));
    }
    Line::from(spans)
}

fn draw_drawer(app: &App, area: Rect, frame: &mut Frame) {
    let border = Style::new().fg(palette(palette::BORDER_DIM));
    let (title, lines) = match (app.help, app.selected()) {
        (true, _) => (" keys ".to_owned(), help_lines()),
        (false, Some(card)) => (
            format!(" #{} {} ", card.issue.number, card.issue.title),
            detail_lines(card, app.now),
        ),
        (false, None) => (
            " board ".to_owned(),
            vec![Line::from(Span::styled(
                "no issue in this column (h/l to move, n files one)",
                dim(),
            ))],
        ),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Span::styled(
            truncate(&title, usize::from(area.width.saturating_sub(4))),
            text().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll, 0)),
        inner,
    );
}

fn detail_lines(card: &Card, now: f64) -> Vec<Line<'static>> {
    let issue: &Issue = &card.issue;
    let mut first = vec![Span::styled(" ", text())];
    for label in &issue.labels {
        first.push(Span::styled(format!("{} ", label.name), label_style()));
    }
    if let Some(milestone) = &issue.milestone {
        first.push(Span::styled(format!("◆ {} ", milestone.title), accent()));
    }
    let who = if issue.assignees.is_empty() {
        String::new()
    } else {
        format!(
            "  assigned {}",
            issue
                .assignees
                .iter()
                .map(|user| format!("@{}", user.login))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    first.push(Span::styled(
        format!(
            "  {} by @{}, opened {} ago, updated {} ago{who}",
            issue.state.to_lowercase(),
            issue.author.login,
            age(&issue.created_at, now),
            age(&issue.updated_at, now)
        ),
        dim(),
    ));
    let mut lines = vec![Line::from(first)];
    let mut agents = vec![Span::styled(" agents: ", dim())];
    if card.agents.is_empty() {
        agents.push(Span::styled("none (s starts one)", dim()));
    }
    for (index, agent) in card.agents.iter().enumerate() {
        if index > 0 {
            agents.push(Span::styled(", ", dim()));
        }
        agents.push(agent_span(agent));
    }
    lines.push(Line::from(agents));
    lines.push(Line::default());
    for line in issue.body.lines() {
        lines.push(Line::from(Span::styled(format!(" {line}"), text())));
    }
    lines
}

fn agent_span(agent: &CardAgent) -> Span<'static> {
    Span::styled(
        format!(
            "{} {} {}",
            state_mark(agent.state),
            agent.name,
            agent.state.as_str()
        ),
        state_style(agent.state),
    )
}

fn help_lines() -> Vec<Line<'static>> {
    [
        (
            "j/k  h/l",
            "move between cards and columns; g/G first and last card",
        ),
        ("J/K", "scroll the issue body"),
        (
            "s",
            "start a developer agent on the issue (tagged issue-<n>), in this checkout",
        ),
        (
            "Enter  a  p",
            "show the agent working on it: focus its pane, attach it into a pane, popup",
        ),
        ("o", "open the issue in the browser"),
        (
            "c  n",
            "comment on the issue; file a new issue (gh opens your editor)",
        ),
        ("x", "close the issue, or reopen a closed one"),
        ("m", "assign yourself, or unassign"),
        (
            "r",
            "fetch again now (every 60 s otherwise; agents every 3 s)",
        ),
        ("?  q", "this list; quit (the popup's resident stays warm)"),
    ]
    .into_iter()
    .map(|(keys, what)| {
        Line::from(vec![
            Span::styled(format!(" {keys:<12}"), accent()),
            Span::styled(what.to_owned(), text()),
        ])
    })
    .collect()
}

fn footer_line(app: &App, width: u16) -> Line<'static> {
    match &app.notice {
        Some(notice) => Line::from(Span::styled(
            truncate(&format!(" {notice}"), usize::from(width)),
            Style::new().fg(palette(palette::WARN)),
        )),
        None => Line::from(Span::styled(
            truncate(&format!(" {KEYS}"), usize::from(width)),
            dim(),
        )),
    }
}

/// The frame as plain text, trailing blanks trimmed: what `--snapshot` prints.
pub fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut lines = Vec::with_capacity(usize::from(area.height));
    for y in 0..area.height {
        let mut line = String::new();
        for x in 0..area.width {
            line.push_str(buffer[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_keeps_the_width() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "ab…");
        assert_eq!(truncate("abcd", 0), "");
    }
}
