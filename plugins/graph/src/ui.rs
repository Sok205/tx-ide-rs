//! Drawing, after the "tx-ide visual identity" artifact: a header, the column labels, the
//! canvas (braille Bézier edges under heavy boxes), a legend, the selected node's detail line
//! and the key hints.
//!
//! The canvas is drawn whole into an off-screen buffer as tall as the layout, then the rows the
//! scroll shows are copied into the frame, so a box cut by the view's edge stays a box.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Context, Line as Segment};
use ratatui::widgets::{Block, BorderType, Clear, LineGauge, Paragraph, Widget};

use crate::app::App;
use crate::model::{Edge, Graph, Node, NodeState};
use tx_client::palette::{
    ACCENT, BG_DEEP, BORDER_DIM, DIM_FG, FG, GREEN, INFO, ON_ACCENT, WARN, palette,
};

/// Above this share of its window, a context gauge turns WARN.
const CONTEXT_WARN: f64 = 0.8;
/// How far a sibling arc bulges right of its column.
const ARC_BULGE: f64 = 7.0;
const CURVE_STEPS: u16 = 48;
/// The "parent ended" stub's length in cells.
const STUB_LENGTH: f64 = 12.0;

fn colour(color: Color) -> Style {
    Style::new().fg(palette(color))
}

fn dim() -> Style {
    colour(DIM_FG).add_modifier(Modifier::DIM)
}

fn bold(color: Color) -> Style {
    colour(color).add_modifier(Modifier::BOLD)
}

/// `from` moved `amount` (0..1) of the way to `to`, before the palette maps it.
fn mix(from: Color, to: Color, amount: f64) -> Color {
    let (Color::Rgb(red, green, blue), Color::Rgb(to_red, to_green, to_blue)) = (from, to) else {
        return from;
    };
    let blend = |start: u8, end: u8| {
        (f64::from(start) + (f64::from(end) - f64::from(start)) * amount.clamp(0.0, 1.0)).round()
            as u8
    };
    Color::Rgb(
        blend(red, to_red),
        blend(green, to_green),
        blend(blue, to_blue),
    )
}

/// `text` cut to `width` characters, the last one `…` when cut.
fn cut(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut kept: String = text.chars().take(width.saturating_sub(1)).collect();
    kept.push('…');
    kept
}

/// `4s`, `12m`, `3h`, `2d`.
pub fn age(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    match seconds {
        _ if seconds < 60.0 => format!("{}s", seconds as u64),
        _ if seconds < 3600.0 => format!("{}m", (seconds / 60.0) as u64),
        _ if seconds < 86400.0 => format!("{}h", (seconds / 3600.0) as u64),
        _ => format!("{}d", (seconds / 86400.0) as u64),
    }
}

/// The window as typed: `1h`, `30m`, `90s`, `2d`.
pub fn window_label(seconds: f64) -> String {
    let whole = |unit: f64| (seconds / unit).fract() == 0.0;
    match seconds {
        _ if whole(86400.0) => format!("{}d", seconds / 86400.0),
        _ if whole(3600.0) => format!("{}h", seconds / 3600.0),
        _ if whole(60.0) => format!("{}m", seconds / 60.0),
        _ => format!("{}s", seconds.round()),
    }
}

/// `1.2K`, `75K`, `1.0M`.
fn tokens(count: u64) -> String {
    match count {
        _ if count >= 1_000_000 => format!("{:.1}M", count as f64 / 1e6),
        _ if count >= 10_000 => format!("{}K", count / 1000),
        _ if count >= 1_000 => format!("{:.1}K", count as f64 / 1e3),
        _ => count.to_string(),
    }
}

fn glyph(state: NodeState) -> &'static str {
    match state {
        NodeState::Operator => "◇",
        NodeState::Starting => "◐",
        NodeState::Working => "●",
        NodeState::Idle | NodeState::Ended => "○",
        NodeState::NeedsYou => "▲",
    }
}

fn state_colour(state: NodeState) -> Color {
    match state {
        NodeState::Operator => FG,
        NodeState::Working => GREEN,
        NodeState::Starting | NodeState::NeedsYou => WARN,
        NodeState::Idle | NodeState::Ended => DIM_FG,
    }
}

fn quiet(node: &Node) -> bool {
    matches!(node.state, NodeState::Idle | NodeState::Ended)
}

pub fn render(app: &mut App, frame: &mut Frame) {
    let [header, _, labels, body, legend, detail, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    app.arrange(body);
    frame.render_widget(header_line(app), header);
    if app.graph.nodes.is_empty() {
        let text = format!(
            "  no agents running or active in the last {}",
            window_label(app.window)
        );
        frame.render_widget(Paragraph::new(Line::styled(text, dim())), body);
    } else {
        frame.render_widget(column_labels(app, labels.width), labels);
        draw_canvas(app, frame, body);
        frame.render_widget(legend_line(), legend);
        frame.render_widget(detail_line(app), detail);
    }
    let group = app.group().unwrap_or("all");
    frame.render_widget(
        Paragraph::new(Line::styled(
            format!("  tab/hjkl select · g group: {group} · q quit"),
            dim(),
        )),
        footer,
    );
}

fn header_line(app: &App) -> Paragraph<'static> {
    let graph = &app.graph;
    let count = |state: NodeState| {
        graph
            .nodes
            .iter()
            .filter(|node| node.state == state)
            .count()
    };
    let agents = graph.nodes.iter().filter(|node| node.id.is_some()).count();
    let scope = app.group().unwrap_or("all agents").to_owned();
    let mut spans = vec![
        Span::styled(" tx-ide", bold(ACCENT)),
        Span::styled("  graph  ", dim()),
        Span::styled(scope, bold(FG)),
        Span::styled(format!("   {agents} agents · "), dim()),
        Span::styled("●", colour(GREEN)),
        Span::styled(format!(" {} working · ", count(NodeState::Working)), dim()),
    ];
    let waiting = count(NodeState::NeedsYou);
    if waiting > 0 {
        spans.push(Span::styled("▲", bold(WARN)));
        spans.push(Span::styled(format!(" {waiting} needs you · "), dim()));
    }
    spans.push(Span::styled("━", colour(INFO)));
    spans.push(Span::styled(
        format!(
            " {} messages / {}",
            graph.traffic.len(),
            window_label(graph.window)
        ),
        dim(),
    ));
    Paragraph::new(Line::from(spans))
}

fn column_labels(app: &App, width: u16) -> Paragraph<'static> {
    let mut text = String::new();
    for (depth, x) in app.placement.column_x.iter().enumerate() {
        let label = if depth == 0 {
            "depth 0".to_owned()
        } else {
            depth.to_string()
        };
        let x = usize::from(*x);
        if text.chars().count() < x {
            text.push_str(&" ".repeat(x - text.chars().count()));
        }
        text.push_str(&label);
    }
    Paragraph::new(Line::styled(cut(&text, usize::from(width)), dim()))
}

fn legend_line() -> Paragraph<'static> {
    let swatch = |glyph: &'static str, color: Color, label: &'static str| {
        [Span::styled(glyph, bold(color)), Span::styled(label, dim())]
    };
    let mut spans = vec![Span::raw("  ")];
    spans.extend(swatch("┏┓", ACCENT, " selected   "));
    spans.extend(swatch("┏┓", GREEN, " working   "));
    spans.extend(swatch("┏┓", BORDER_DIM, " idle   "));
    spans.extend(swatch("⠉⠒⠤", INFO, " messages   "));
    spans.extend(swatch("⠁⠂⠄", BORDER_DIM, " lineage only"));
    Paragraph::new(Line::from(spans))
}

/// The selected node as `label value` pairs.
fn detail_line(app: &App) -> Paragraph<'static> {
    let graph = &app.graph;
    let node = &graph.nodes[app.selected];
    let mut spans = vec![Span::styled(format!("  {}  ", node.name), bold(FG))];
    let mut pair = |label: &str, value: String| {
        if spans.len() > 1 {
            spans.push(Span::styled(" · ", dim()));
        }
        spans.push(Span::styled(format!("{label} "), dim()));
        spans.push(Span::styled(value, colour(FG)));
    };
    if node.state == NodeState::Operator {
        pair("sent", sent_by(graph, app.selected).to_string());
        return Paragraph::new(Line::from(spans));
    }
    if let Some(engine) = node.engine {
        pair("engine", engine.as_str().to_owned());
    }
    if let Some(definition) = &node.definition {
        pair("definition", definition.clone());
    }
    pair("group", node.group.clone());
    let parent = match (node.parent, node.parent_lost) {
        (Some(parent), _) => graph.nodes[parent].name.clone(),
        (None, true) => "ended".to_owned(),
        (None, false) => "—".to_owned(),
    };
    pair("parent", parent);
    let children = graph
        .nodes
        .iter()
        .filter(|child| child.parent == Some(app.selected))
        .count();
    pair("children", children.to_string());
    let messages: u32 = graph
        .edges
        .iter()
        .filter(|edge| edge.from == app.selected || edge.to == app.selected)
        .map(Edge::messages)
        .sum();
    pair("messages", messages.to_string());
    if let Some(context) = node.context {
        let value = match context.ratio() {
            Some(ratio) => format!("{} ({:.0}%)", tokens(context.tokens), ratio * 100.0),
            None => tokens(context.tokens),
        };
        pair("ctx", value);
    }
    Paragraph::new(Line::from(spans))
}

fn sent_by(graph: &Graph, index: usize) -> usize {
    graph
        .traffic
        .iter()
        .filter(|traffic| traffic.from == index)
        .count()
}

fn draw_canvas(app: &App, frame: &mut Frame, view: Rect) {
    let height = app.placement.height.max(view.height).max(1);
    let area = Rect::new(0, 0, view.width, height);
    let mut buffer = Buffer::empty(area);
    draw_edges(app, area, &mut buffer);
    for (index, node) in app.graph.nodes.iter().enumerate() {
        let rect = app.placement.boxes[index];
        let selected = index == app.selected;
        if app.placement.compact {
            draw_compact_node(node, rect, selected, &mut buffer);
        } else {
            draw_node(app, index, rect, selected, &mut buffer);
        }
    }
    let target = frame.buffer_mut();
    for row in 0..view.height {
        let source_row = row + app.scroll;
        if source_row >= height {
            break;
        }
        for column in 0..view.width {
            target[(view.x + column, view.y + row)] = buffer[(column, source_row)].clone();
        }
    }
}

/// A cell position to canvas coordinates: x right, y up from the bottom row.
fn point(area: Rect, column: f64, row: f64) -> (f64, f64) {
    (column, f64::from(area.height - 1) - row)
}

type Curve = [(f64, f64); 4];

fn bezier(curve: &Curve, t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    let weights = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    let mut point = (0.0, 0.0);
    for (weight, control) in weights.iter().zip(curve) {
        point.0 += weight * control.0;
        point.1 += weight * control.1;
    }
    point
}

/// The row an edge meets a box at: the first inner line, or the only line of a compact box.
fn anchor_row(rect: Rect) -> f64 {
    f64::from(rect.y + 1.min(rect.height - 1))
}

/// From the right side of `from` to just before the left side of `to`, leaving the cell
/// before `to` for the arrowhead; within one column, an arc out to the right and back.
fn edge_curve(area: Rect, from: Rect, to: Rect) -> Curve {
    if from.x == to.x {
        let start = point(area, f64::from(from.right()), anchor_row(from));
        let end = point(area, f64::from(to.right()), anchor_row(to));
        return [
            start,
            (start.0 + ARC_BULGE, start.1),
            (end.0 + ARC_BULGE, end.1),
            end,
        ];
    }
    let start = point(area, f64::from(from.right()), anchor_row(from));
    let end = point(area, f64::from(to.x) - 2.0, anchor_row(to));
    let reach = (end.0 - start.0) * 0.55;
    [
        start,
        (start.0 + reach, start.1),
        (end.0 - reach, end.1),
        end,
    ]
}

fn draw_curve(context: &mut Context, curve: &Curve, color: Color, dashed: bool, thick: bool) {
    let mut previous = curve[0];
    for step in 1..=CURVE_STEPS {
        let next = bezier(curve, f64::from(step) / f64::from(CURVE_STEPS));
        if !dashed || (step / 3) % 2 == 0 {
            context.draw(&Segment::new(previous.0, previous.1, next.0, next.1, color));
            if thick {
                context.draw(&Segment::new(
                    previous.0,
                    previous.1 + 0.25,
                    next.0,
                    next.1 + 0.25,
                    color,
                ));
            }
        }
        previous = next;
    }
}

fn draw_edges(app: &App, area: Rect, buffer: &mut Buffer) {
    let graph = &app.graph;
    let boxes = &app.placement.boxes;
    let busiest = graph
        .edges
        .iter()
        .map(Edge::messages)
        .max()
        .unwrap_or(0)
        .max(1);
    let touches = |edge: &Edge| edge.from == app.selected || edge.to == app.selected;
    let edge_colour = |edge: &Edge| {
        if edge.messages() == 0 {
            BORDER_DIM
        } else if touches(edge) {
            INFO
        } else {
            let strength = 0.35 + 0.65 * f64::from(edge.messages()) / f64::from(busiest);
            mix(BG_DEEP, INFO, strength * 0.7)
        }
    };
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, f64::from(area.width.max(2) - 1)])
        .y_bounds([0.0, f64::from(area.height.max(2) - 1)])
        .paint(|context| {
            for edge in &graph.edges {
                let curve = edge_curve(area, boxes[edge.from], boxes[edge.to]);
                let quiet = edge.messages() == 0;
                draw_curve(
                    context,
                    &curve,
                    palette(edge_colour(edge)),
                    quiet,
                    touches(edge) && !quiet,
                );
            }
            for (index, node) in graph.nodes.iter().enumerate() {
                if node.parent_lost {
                    let rect = boxes[index];
                    let end = point(area, f64::from(rect.x) - 2.0, anchor_row(rect));
                    let start = (end.0 - STUB_LENGTH, end.1);
                    draw_curve(
                        context,
                        &[start, start, end, end],
                        palette(BORDER_DIM),
                        true,
                        false,
                    );
                    context.print(start.0, start.1 + 1.0, Line::styled("parent ended", dim()));
                }
            }
            for edge in &graph.edges {
                let curve = edge_curve(area, boxes[edge.from], boxes[edge.to]);
                let selected = touches(edge);
                if edge.messages() > 0 {
                    let (x, y) = bezier(&curve, 0.5);
                    let badge = if selected {
                        bold(ON_ACCENT).bg(palette(INFO))
                    } else {
                        bold(INFO).bg(palette(BG_DEEP))
                    };
                    context.print(
                        x - 1.0,
                        y,
                        Line::styled(format!(" {} ", edge.messages()), badge),
                    );
                }
                if boxes[edge.from].x == boxes[edge.to].x {
                    continue;
                }
                let arrow_colour = if selected { INFO } else { edge_colour(edge) };
                if edge.lineage || edge.forward > 0 {
                    let head = if edge.messages() == 0 { "▷" } else { "▶" };
                    let end = curve[3];
                    context.print(end.0 + 1.0, end.1, Line::styled(head, colour(arrow_colour)));
                }
                if edge.backward > 0 {
                    let start = curve[0];
                    context.print(start.0, start.1, Line::styled("◀", colour(arrow_colour)));
                }
            }
        });
    canvas.render(area, buffer);
}

fn title_style(node: &Node, selected: bool) -> Style {
    if selected {
        bold(ON_ACCENT).bg(palette(ACCENT))
    } else if quiet(node) || node.connector {
        dim()
    } else {
        bold(FG)
    }
}

fn draw_node(app: &App, index: usize, rect: Rect, selected: bool, buffer: &mut Buffer) {
    let node = &app.graph.nodes[index];
    let state = node.state;
    let border = match state {
        _ if selected => ACCENT,
        NodeState::Working => GREEN,
        NodeState::Starting | NodeState::NeedsYou => WARN,
        NodeState::Operator => FG,
        NodeState::Idle | NodeState::Ended => BORDER_DIM,
    };
    let name_style = title_style(node, selected);
    let glyph_style = if selected {
        name_style
    } else {
        bold(state_colour(state))
    };
    let room = usize::from(rect.width.saturating_sub(6));
    let title = Line::from(vec![
        Span::styled(" ", name_style),
        Span::styled(glyph(state), glyph_style),
        Span::styled(format!(" {} ", cut(&node.name, room)), name_style),
    ]);
    let block = Block::bordered()
        .border_type(if state == NodeState::Operator {
            BorderType::Rounded
        } else {
            BorderType::Thick
        })
        .border_style(bold(border))
        .title(title);
    let inner = block.inner(rect).inner(Margin::new(1, 0));
    Clear.render(rect, buffer);
    block.render(rect, buffer);
    let [status_area, context_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
    if state == NodeState::Operator {
        let sent = sent_by(&app.graph, index);
        Paragraph::new(Line::from(vec![
            Span::styled("operator", colour(FG)),
            Span::styled(format!(" · {sent} sent"), dim()),
        ]))
        .render(status_area, buffer);
        return;
    }
    let since = node.last_activity.unwrap_or(node.created_at);
    let status = Line::from(vec![
        Span::styled(state.word(), colour(state_colour(state))),
        Span::styled(format!(" · {}", age(app.graph.now - since)), dim()),
    ]);
    Paragraph::new(status).render(status_area, buffer);
    let Some(context) = node.context else {
        return;
    };
    match context.ratio() {
        Some(ratio) => {
            let filled = if ratio > CONTEXT_WARN {
                WARN
            } else {
                mix(INFO, ACCENT, 0.5)
            };
            LineGauge::default()
                .ratio(ratio)
                .label(Span::styled("ctx", dim()))
                .filled_symbol("━")
                .unfilled_symbol("─")
                .filled_style(colour(filled))
                .unfilled_style(colour(BORDER_DIM))
                .render(context_area, buffer);
        }
        None => Paragraph::new(Line::styled(
            format!("ctx {}", tokens(context.tokens)),
            dim(),
        ))
        .render(context_area, buffer),
    }
}

/// One borderless line: `glyph name`.
fn draw_compact_node(node: &Node, rect: Rect, selected: bool, buffer: &mut Buffer) {
    let name_style = title_style(node, selected);
    let glyph_style = if selected {
        name_style
    } else {
        bold(state_colour(node.state))
    };
    let room = usize::from(rect.width.saturating_sub(3));
    Clear.render(rect, buffer);
    Paragraph::new(Line::from(vec![
        Span::styled(glyph(node.state), glyph_style),
        Span::styled(format!(" {:<room$}", cut(&node.name, room)), name_style),
    ]))
    .render(rect, buffer);
}

/// The buffer as plain text, one line per row, trailing spaces trimmed.
pub fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut lines = Vec::with_capacity(usize::from(area.height));
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        for x in area.left()..area.right() {
            line.push_str(buffer[(x, y)].symbol());
        }
        lines.push(line.trim_end().to_owned());
    }
    lines.join("\n")
}
