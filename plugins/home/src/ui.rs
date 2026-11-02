//! Drawing: the session list with its header and footer, and the plugins / keys sub-lists,
//! rendered into a [`Buffer`]. Colours go through [`palette`], so `TX_HOME_COLORS=256` swaps
//! the true-colour constants for the nearest entries of the 256-colour palette.

use std::sync::OnceLock;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};
use tx_client::render::reltime;
use tx_core::LlmEngineState;

use crate::model::{AgentRow, AssistantState, Context, Detail, Home, Row, View};

pub const ACCENT: Color = Color::Rgb(122, 162, 247);
pub const INFO: Color = Color::Rgb(125, 207, 255);
pub const WARN: Color = Color::Rgb(224, 175, 104);
pub const FG: Color = Color::Rgb(192, 202, 245);
pub const DIM_FG: Color = Color::Rgb(169, 177, 214);
pub const GREEN: Color = Color::Rgb(158, 206, 106);
pub const RED: Color = Color::Rgb(247, 118, 142);

/// The loop's redraw rate: the starting spinner is the only thing that moves.
pub const FPS: u64 = 4;

const NAME_W: usize = 24;
const SPINNER: [char; 4] = ['◐', '◓', '◑', '◒'];
const DAY: f64 = 86_400.0;
const DETAIL_INDENT: &str = "      ";

/// `color`, or its 256-colour palette entry when `TX_HOME_COLORS=256`.
fn palette(color: Color) -> Color {
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

fn fg(color: Color) -> Style {
    Style::new().fg(palette(color))
}

fn dim() -> Style {
    fg(DIM_FG).add_modifier(Modifier::DIM)
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

/// `frame` counts redraws; it only picks the spinner glyph of starting agents.
pub fn draw(home: &Home, frame: usize, area: Rect, buf: &mut Buffer) {
    match home.view() {
        View::Main => draw_main(home, frame, area, buf),
        View::Plugins(row) => draw_plugins(home, row, area, buf),
        View::Keys(row) => draw_keys(home, row, area, buf),
    }
}

fn draw_main(home: &Home, frame: usize, area: Rect, buf: &mut Buffer) {
    let header = header_line(home, usize::from(area.width));
    put(buf, area, area.x, area.y, &header, area.width);
    if area.height < 2 {
        return;
    }
    let footer = Line::styled(home.footer(), dim());
    put(buf, area, area.x, area.bottom() - 1, &footer, area.width);

    let body = usize::from(area.height - 2);
    let cursor = home.cursor();
    let mut lines = Vec::new();
    // The cursor row's first line and the end of its block: the detail and views rows below it.
    let mut block: Option<(usize, usize)> = None;
    for (index, row) in home.rows().iter().enumerate() {
        let start = lines.len();
        lines.extend(row_lines(row, &home.context, frame));
        if Some(index) == cursor {
            block = Some((start, lines.len()));
        } else if let Some((_, end)) = &mut block
            && *end == start
            && matches!(row, Row::Detail(..) | Row::Views(_))
        {
            *end = lines.len();
        }
    }
    let cursor_line = block.map(|(start, _)| start);
    let first = block.map_or(0, |(start, end)| {
        if end - start <= body {
            end.saturating_sub(body)
        } else {
            start
        }
    });
    let visible = lines.iter().enumerate().skip(first).take(body);
    for ((index, line), y) in visible.zip(area.y + 1..) {
        put(buf, area, area.x, y, line, area.width);
        if Some(index) == cursor_line {
            buf.set_style(
                Rect::new(area.x, y, area.width, 1).intersection(area),
                Style::new().add_modifier(Modifier::REVERSED),
            );
        }
    }
}

fn header_line(home: &Home, width: usize) -> Line<'static> {
    let context = &home.context;
    let running = context
        .agents
        .iter()
        .filter(|agent| !agent.is_assistant && agent.state != LlmEngineState::Ended)
        .count();
    let ended_today = context
        .agents
        .iter()
        .filter(|agent| {
            !agent.is_assistant
                && agent.state == LlmEngineState::Ended
                && context.now - agent.age_at <= DAY
        })
        .count();
    let counts = format!("  {running} running · {ended_today} ended today · assistant ");
    let (state, style) = match &context.assistant {
        AssistantState::Absent => ("○ none".to_owned(), dim()),
        AssistantState::Creating => ("◐ starting · creating".to_owned(), fg(WARN)),
        AssistantState::Starting => ("◐ starting · claude".to_owned(), fg(WARN)),
        AssistantState::Ready => ("● ready".to_owned(), fg(GREEN)),
        AssistantState::Stopped { .. } => ("○ stopped · r restart".to_owned(), dim()),
        AssistantState::Failed(reason) => {
            let prefix = "✗ failed: ";
            let used = "tx-ide".len() + counts.chars().count() + prefix.chars().count();
            let reason = truncate(reason, width.saturating_sub(used));
            (format!("{prefix}{reason}"), fg(RED))
        }
    };
    Line::from(vec![
        Span::styled("tx-ide", fg(ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(counts, dim()),
        Span::styled(state, style),
    ])
}

fn row_lines(row: &Row<'_>, context: &Context, frame: usize) -> Vec<Line<'static>> {
    match row {
        Row::Project(project) => vec![Line::from(vec![
            Span::styled(project.name.clone(), fg(FG).add_modifier(Modifier::BOLD)),
            Span::styled(format!("  {}", project.path.display()), dim()),
        ])],
        Row::Agent(agent) => vec![agent_line(agent, context.now, frame)],
        Row::Detail(agent, detail) => detail_lines(agent, *detail, context.now),
        Row::Views(views) => {
            let names: Vec<&str> = views.iter().map(|view| view.name.as_str()).collect();
            vec![Line::styled(format!("    ⌐ {}", names.join(" · ")), dim())]
        }
        Row::EndedFold { hidden, .. } => vec![Line::styled(
            format!("  … {hidden} more ended · Tab"),
            dim(),
        )],
        Row::FirstRun => first_run_lines(),
    }
}

fn agent_line(agent: &AgentRow, now: f64, frame: usize) -> Line<'static> {
    let (glyph, word, glyph_style) = match agent.state {
        LlmEngineState::Working => ('●', "working", fg(GREEN)),
        LlmEngineState::Starting => (SPINNER[frame % SPINNER.len()], "starting", fg(WARN)),
        LlmEngineState::Idle => ('○', "idle", dim()),
        LlmEngineState::Ended => ('○', "ended", dim()),
    };
    let body = if agent.state == LlmEngineState::Ended {
        dim()
    } else {
        fg(FG)
    };
    let age = reltime(Some(agent.age_at), now);
    let name = truncate(&agent.name, NAME_W);
    let mut spans = vec![
        Span::styled("  ", body),
        Span::styled(glyph.to_string(), glyph_style),
        Span::styled(format!(" {name:<NAME_W$} {word:<8}{age:>6}"), body),
    ];
    if let Some(branch) = &agent.branch {
        spans.push(Span::styled(format!("  {branch}"), dim()));
    }
    Line::from(spans)
}

/// `label value`, the label dim.
fn labelled(label: &str, value: String) -> Vec<Span<'static>> {
    vec![
        Span::styled(format!("{label} "), dim()),
        Span::styled(value, fg(FG)),
    ]
}

/// The parts one after another behind the detail indent, the separator dim.
fn detail_line(parts: Vec<Vec<Span<'static>>>, separator: &str) -> Line<'static> {
    let mut spans = vec![Span::raw(DETAIL_INDENT)];
    for (index, part) in parts.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(separator.to_owned(), dim()));
        }
        spans.extend(part);
    }
    Line::from(spans)
}

fn detail_lines(agent: &AgentRow, detail: Option<&Detail>, now: f64) -> Vec<Line<'static>> {
    let Some(detail) = detail else {
        return vec![Line::styled(format!("{DETAIL_INDENT}…"), dim())];
    };
    let mut lines = Vec::new();

    let mut facts = Vec::new();
    if let Some(branch) = &agent.branch {
        facts.push(labelled("branch", branch.clone()));
    }
    if !detail.roles.is_empty() {
        facts.push(labelled("roles", detail.roles.join(" ")));
    }
    let engine = match (&detail.model, &detail.effort) {
        (Some(model), Some(effort)) => Some(format!("{model}/{effort}")),
        (Some(only), None) | (None, Some(only)) => Some(only.clone()),
        (None, None) => None,
    };
    if let Some(engine) = engine {
        facts.push(vec![Span::styled(engine, fg(FG))]);
    }
    if let Some(parent) = &detail.parent_name {
        facts.push(labelled("parent", parent.clone()));
    }
    if detail.started_at != 0.0 {
        facts.push(labelled("started", reltime(Some(detail.started_at), now)));
    }
    if let Some(ended_at) = detail.ended_at {
        facts.push(labelled("ended", reltime(Some(ended_at), now)));
    }
    if !facts.is_empty() {
        lines.push(detail_line(facts, "   "));
    }

    if let Some(text) = &detail.last_user {
        lines.push(detail_line(vec![labelled("you ›", format!(" {text}"))], ""));
    }
    if let Some(text) = &detail.last_agent {
        let speaker = format!("{} ›", agent.name);
        lines.push(detail_line(
            vec![labelled(&speaker, format!(" {text}"))],
            "",
        ));
    }

    let mut spans = detail_line(Vec::new(), "").spans;
    if !detail.artifacts.is_empty() {
        let counts: Vec<String> = detail
            .artifacts
            .iter()
            .map(|(kind, count)| format!("{count} {kind}"))
            .collect();
        spans.extend(labelled("artifacts", counts.join(" · ")));
        spans.push(Span::raw("        "));
    }
    let first_verb = if agent.state == LlmEngineState::Ended {
        "r resume"
    } else {
        "x stop"
    };
    spans.push(Span::styled(
        format!("{first_verb}  a artifacts  t timeline"),
        dim(),
    ));
    lines.push(Line::from(spans));
    lines
}

fn first_run_lines() -> Vec<Line<'static>> {
    let verb = |key: &str, name: &str, explanation: &str| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(key.to_owned(), fg(WARN)),
            Span::raw(format!("  {name:<15}")),
            Span::styled(explanation.to_owned(), dim()),
        ])
    };
    vec![
        Line::raw("No agents yet. Three ways to start:"),
        Line::raw(""),
        verb(
            "n",
            "new agent",
            "pick a repo, an engine and a prompt — it gets its own worktree",
        ),
        verb(
            "/",
            "ask",
            "tell the assistant: \"start a developer in ~/w/wrangler on the retry bug\"",
        ),
        verb(
            "L",
            "tour",
            "ten minutes through the keys, in a scratch session",
        ),
        Line::raw(""),
        Line::styled(
            "Ended sessions will show up here under their project, greyed, with r to resume.",
            dim(),
        ),
    ]
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
        .border_style(fg(ACCENT));
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
                ("active", fg(ACCENT))
            } else {
                ("disabled", dim())
            };
            Line::from(vec![
                Span::styled(format!("{id:<idw$}  "), fg(FG)),
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
                Span::styled(format!("{keys:<keyw$}  "), fg(ACCENT)),
                Span::styled(format!("{:<idw$}  ", hint.plugin), fg(WARN)),
                Span::styled(hint.text.clone(), fg(FG)),
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
    use std::collections::BTreeMap;

    use ratatui::crossterm::event::KeyCode;

    use super::*;
    use crate::model::tests::{agent, context};
    use crate::model::{AgentRow, AssistantState, Detail};

    const W: u16 = 80;
    const H: u16 = 30;

    fn render_sized(home: &Home, frame: usize, width: u16, height: u16) -> Buffer {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        draw(home, frame, area, &mut buf);
        buf
    }

    fn render(home: &Home) -> Buffer {
        render_sized(home, 0, W, H)
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

    fn first_run_context() -> Context {
        let assistant = AgentRow {
            tag: "tx-system".into(),
            is_assistant: true,
            ..agent("s1", "tx-assistant", LlmEngineState::Idle, 990.0, 0)
        };
        Context {
            projects: vec![crate::model::tests::project("tx-system")],
            agents: vec![assistant],
            views: Vec::new(),
            ..context()
        }
    }

    fn detail() -> Detail {
        Detail {
            roles: vec!["COMMON".into(), "DEVELOPER".into()],
            model: Some("opus".into()),
            effort: Some("medium".into()),
            parent_name: Some("lead".into()),
            last_user: Some("fix the retry backoff".into()),
            last_agent: Some("Backoff fixed; tests pass.".into()),
            artifacts: vec![("doc".into(), 1), ("diff".into(), 2)],
            started_at: 100.0,
            ended_at: Some(940.0),
        }
    }

    #[test]
    fn header_shows_counts_and_assistant_state() {
        let mut home = Home::new(context());
        let lines = text(&render(&home));
        assert_eq!(
            lines[0],
            "tx-ide  2 running · 1 ended today · assistant ● ready"
        );
        for (state, tail) in [
            (AssistantState::Absent, "assistant ○ none"),
            (AssistantState::Creating, "assistant ◐ starting · creating"),
            (AssistantState::Starting, "assistant ◐ starting · claude"),
            (
                AssistantState::Stopped { id: "s1".into() },
                "assistant ○ stopped · r restart",
            ),
            (
                AssistantState::Failed("no claude".into()),
                "assistant ✗ failed: no claude",
            ),
        ] {
            home.context.assistant = state;
            let lines = text(&render(&home));
            assert!(lines[0].ends_with(tail), "{}", lines[0]);
        }
        home.context.assistant = AssistantState::Failed("x".repeat(100));
        let line = &text(&render(&home))[0];
        assert_eq!(line.chars().count(), usize::from(W), "{line}");
        assert!(line.ends_with('…'), "{line}");
    }

    #[test]
    fn a_project_row_is_bold_with_a_dim_path() {
        let home = Home::new(context());
        let buf = render(&home);
        let row = row_of(&text(&buf), "wrangler  /w/wrangler");
        let y = u16::try_from(row).unwrap();
        assert!(buf[(0, y)].modifier.contains(Modifier::BOLD));
        assert!(!buf[(12, y)].modifier.contains(Modifier::BOLD));
        assert!(buf[(12, y)].modifier.contains(Modifier::DIM));
    }

    #[test]
    fn agent_rows_show_state_age_and_branch_and_starting_ones_spin() {
        let mut home = Home::new(context());
        home.context
            .agents
            .push(agent("a3", "late", LlmEngineState::Starting, 990.0, 0));
        let lines = text(&render(&home));
        assert!(
            lines[row_of(&lines, "startup-ux")]
                .starts_with("  ● startup-ux               working     1m  feat/startup-ux"),
            "{}",
            lines[row_of(&lines, "startup-ux")]
        );
        assert!(lines[row_of(&lines, "docs-site")].starts_with("  ○ docs-site"));
        assert!(lines[row_of(&lines, "docs-site")].contains("ended       8m"));
        for (frame, glyph) in [(0, "◐"), (1, "◓"), (2, "◑"), (3, "◒"), (4, "◐")] {
            let lines = text(&render_sized(&home, frame, W, H));
            assert!(
                lines[row_of(&lines, "late")].starts_with(&format!("  {glyph} late")),
                "{frame}"
            );
        }
    }

    #[test]
    fn views_and_the_ended_fold_have_their_own_rows() {
        let mut context = context();
        for index in 0..4 {
            let id = format!("old-{index}");
            context.agents.push(agent(
                &id,
                &id,
                LlmEngineState::Ended,
                100.0 - f64::from(index),
                0,
            ));
        }
        let home = Home::new(context);
        let lines = text(&render(&home));
        assert_eq!(
            lines[row_of(&lines, "⌐ wrangler-p1-nvim")],
            "    ⌐ wrangler-p1-nvim · build-watch"
        );
        assert_eq!(
            lines[row_of(&lines, "more ended")],
            "  … 2 more ended · Tab"
        );
    }

    #[test]
    fn an_unfolded_agent_shows_its_detail_rows() {
        let mut home = Home::new(context());
        home.key(KeyCode::Right);
        let lines = text(&render(&home));
        let fixer = row_of(&lines, "fixer   ");
        assert_eq!(lines[fixer + 1], "      …");

        home.context.details = BTreeMap::from([("w1".to_owned(), detail())]);
        let lines = text(&render_sized(&home, 0, 120, H));
        let fixer = row_of(&lines, "fixer   ");
        assert_eq!(
            lines[fixer + 1],
            "      branch fix/retry-backoff   roles COMMON DEVELOPER   opus/medium   parent lead   started 15m   ended 1m"
        );
        assert_eq!(lines[fixer + 2], "      you ›  fix the retry backoff");
        assert_eq!(
            lines[fixer + 3],
            "      fixer ›  Backoff fixed; tests pass."
        );
        assert_eq!(
            lines[fixer + 4],
            "      artifacts 1 doc · 2 diff        x stop  a artifacts  t timeline"
        );
    }

    #[test]
    fn an_ended_agent_offers_resume_in_its_detail() {
        let mut home = Home::new(context());
        home.context.details = BTreeMap::from([("a2".to_owned(), detail())]);
        while home.cursor().is_none_or(
            |row| !matches!(&home.rows()[row], Row::Agent(agent) if agent.name == "docs-site"),
        ) {
            home.key(KeyCode::Char('j'));
        }
        home.key(KeyCode::Right);
        let lines = text(&render(&home));
        assert!(
            lines[row_of(&lines, "artifacts 1 doc")].contains("r resume  a artifacts"),
            "{lines:?}"
        );
    }

    #[test]
    fn the_cursor_row_is_reversed() {
        let home = Home::new(context());
        let buf = render(&home);
        let lines = text(&buf);
        let cursor = u16::try_from(row_of(&lines, "fixer   ")).unwrap();
        let other = u16::try_from(row_of(&lines, "startup-ux")).unwrap();
        assert!((0..W).all(|x| buf[(x, cursor)].modifier.contains(Modifier::REVERSED)));
        assert!((0..W).all(|x| !buf[(x, other)].modifier.contains(Modifier::REVERSED)));
    }

    #[test]
    fn the_footer_changes_when_finding_asking_and_confirming() {
        let last = |home: &Home| text(&render(home)).pop().unwrap();
        let mut home = Home::new(context());
        assert_eq!(
            last(&home),
            "enter attach · →/← unfold · n new here · / ask · f find · ? keys"
        );
        home.key(KeyCode::Char('f'));
        home.key(KeyCode::Char('w'));
        assert_eq!(last(&home), "find › w");
        home.key(KeyCode::Esc);
        home.key(KeyCode::Char('/'));
        home.key(KeyCode::Char('h'));
        home.key(KeyCode::Char('i'));
        assert_eq!(last(&home), "ask › hi");
        home.key(KeyCode::Esc);
        home.key(KeyCode::Char('x'));
        assert_eq!(last(&home), "stop fixer? y/n");
    }

    #[test]
    fn first_run_screen_lists_the_three_verbs() {
        let home = Home::new(first_run_context());
        let buf = render_sized(&home, 0, 100, H);
        let lines = text(&buf);
        let intro = row_of(&lines, "No agents yet. Three ways to start:");
        assert_eq!(lines[intro + 1], "");
        assert_eq!(
            lines[intro + 2],
            "  n  new agent      pick a repo, an engine and a prompt — it gets its own worktree"
        );
        assert_eq!(
            lines[intro + 3],
            "  /  ask            tell the assistant: \"start a developer in ~/w/wrangler on the retry bug\""
        );
        assert_eq!(
            lines[intro + 4],
            "  L  tour           ten minutes through the keys, in a scratch session"
        );
        assert_eq!(lines[intro + 5], "");
        assert_eq!(
            lines[intro + 6],
            "Ended sessions will show up here under their project, greyed, with r to resume."
        );
        assert!(lines[row_of(&lines, "tx-assistant")].starts_with("  ○ tx-assistant"));
        let mut starting = Home::new(first_run_context());
        starting.context.assistant = AssistantState::Starting;
        assert_eq!(
            text(&render(&starting))[0],
            "tx-ide  0 running · 0 ended today · assistant ◐ starting · claude"
        );
        let key_cell = &buf[(2, u16::try_from(intro + 2).unwrap())];
        assert_eq!(key_cell.fg, WARN);
        assert_eq!(
            lines.last().unwrap(),
            "enter attach · n new · / ask · L tour · ? keys"
        );
    }

    #[test]
    fn the_list_scrolls_to_keep_the_cursor_visible() {
        let mut context = context();
        for index in 0..40 {
            let name = format!("bulk-{index:02}");
            context.agents.push(agent(
                &name,
                &name,
                LlmEngineState::Working,
                800.0 - f64::from(index),
                0,
            ));
        }
        let mut home = Home::new(context);
        for _ in 0..30 {
            home.key(KeyCode::Char('j'));
        }
        let Some(Row::Agent(selected)) = home.cursor().map(|index| home.rows()[index].clone())
        else {
            panic!("the cursor is on an agent");
        };
        let buf = render_sized(&home, 0, W, 10);
        let lines = text(&buf);
        assert!(lines[0].starts_with("tx-ide"));
        let reversed: Vec<usize> = (0..10)
            .filter(|&y| buf[(0, y)].modifier.contains(Modifier::REVERSED))
            .map(usize::from)
            .collect();
        assert_eq!(reversed.len(), 1, "{lines:?}");
        assert!(lines[reversed[0]].contains(&selected.name), "{lines:?}");
        assert!(lines[1..9].iter().all(|line| !line.contains("wrangler  ")));
    }

    #[test]
    fn an_unfolded_detail_near_the_bottom_stays_in_view() {
        let mut context = context();
        for index in 0..40 {
            let name = format!("bulk-{index:02}");
            context.agents.push(agent(
                &name,
                &name,
                LlmEngineState::Working,
                800.0 - f64::from(index),
                0,
            ));
        }
        let mut home = Home::new(context);
        for _ in 0..30 {
            home.key(KeyCode::Char('j'));
        }
        let Some(Row::Agent(selected)) = home.cursor().map(|index| home.rows()[index].clone())
        else {
            panic!("the cursor is on an agent");
        };
        let selected = selected.clone();
        home.key(KeyCode::Right);
        home.context.details = BTreeMap::from([(selected.id.clone(), detail())]);

        let lines = text(&render_sized(&home, 0, 60, 10));
        let you = row_of(&lines, "you ›");
        assert!((1..9).contains(&you), "{lines:?}");
        assert!(lines[you - 2].contains(&selected.name), "{lines:?}");

        let buf = render_sized(&home, 0, 60, 5);
        let lines = text(&buf);
        assert!(lines[1].contains(&selected.name), "{lines:?}");
        assert!(buf[(0, 1)].modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn a_tiny_area_never_panics() {
        let mut unfolded = Home::new(context());
        unfolded.key(KeyCode::Right);
        unfolded.context.details = BTreeMap::from([("w1".to_owned(), detail())]);
        let mut finding = Home::new(context());
        finding.key(KeyCode::Char('f'));
        let mut plugins = Home::new(context());
        plugins.key(KeyCode::Char('p'));
        let mut keys = Home::new(context());
        keys.key(KeyCode::Char('K'));
        let mut failed = Home::new(context());
        failed.context.assistant = AssistantState::Failed("boom".into());
        let homes = [
            Home::new(context()),
            Home::new(first_run_context()),
            unfolded,
            finding,
            plugins,
            keys,
            failed,
        ];
        for home in &homes {
            for (width, height) in [(0, 0), (1, 1), (5, 3), (10, 3), (44, 8), (3, 40), (200, 2)] {
                render_sized(home, 3, width, height);
            }
        }
        let area = Rect::new(7, 5, 5, 3);
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 20));
        draw(&homes[0], 0, area, &mut buf);
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
    fn the_256_colour_palette_maps_rgb_into_the_cube_and_leaves_others() {
        assert_eq!(indexed(Color::Rgb(0, 0, 0)), Color::Indexed(16));
        assert_eq!(indexed(Color::Rgb(255, 255, 255)), Color::Indexed(231));
        assert_eq!(indexed(Color::Rgb(255, 0, 0)), Color::Indexed(196));
        assert_eq!(indexed(Color::Red), Color::Red);
    }
}
