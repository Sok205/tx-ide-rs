//! Drawing the timeline: axis, group bands, swimlanes, message arrows and the
//! footer. The axis is anchored at now: the red `┃` carries the clock time, ticks to its left
//! read as offsets from it (`-5m`, `-1h`, `-2d`). The canvas is a cell grid: `━` working (the agent holds the floor), `╌` idle
//! (waiting for the human), `×` killed, red `┃` now;
//! live sessions have a `▸` in a one-column gutter beyond the time axis.
//! a message is a green `•` on the sender's row, `┆` down/up, `▾`/`▴` on the receiver's row.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use super::app::{App, Geometry, RowItem};
use super::clock;

const AXIS_ROWS: u16 = 3;
const FOOTER_ROWS: u16 = 2;
const MAX_LABEL_WIDTH: u16 = 28;

const AMBER: Color = Color::Rgb(224, 175, 104);
const CURSOR_BG: Color = Color::Indexed(236);
const IDLE: Color = Color::DarkGray;
const NOW: Color = Color::Red;
const MESSAGE: Color = Color::Green;

fn put(buffer: &mut Buffer, x: u16, y: u16, symbol: &str, style: Style) {
    if let Some(cell) = buffer.cell_mut((x, y)) {
        cell.set_symbol(symbol);
        cell.set_style(style);
    }
}

fn symbol_at(buffer: &Buffer, x: u16, y: u16) -> &str {
    buffer.cell((x, y)).map_or(" ", |cell| cell.symbol())
}

/// Lay the frame out, remember the geometry for mouse hits, then draw every panel.
pub fn render(app: &mut App, frame: &mut Frame) {
    let area = frame.area();
    let footer = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(FOOTER_ROWS),
        area.width,
        FOOTER_ROWS.min(area.height),
    );
    let body_height = area.height.saturating_sub(FOOTER_ROWS);
    let axis = Rect::new(area.x, area.y, area.width, AXIS_ROWS.min(body_height));
    let lanes = Rect::new(
        area.x,
        area.y + axis.height,
        area.width,
        body_height.saturating_sub(axis.height),
    );
    let label_width = MAX_LABEL_WIDTH.min(area.width / 3);
    let canvas = Rect::new(
        area.x + label_width,
        lanes.y,
        area.width.saturating_sub(label_width + 1),
        lanes.height,
    );
    app.geometry = Geometry {
        label_width,
        axis,
        lanes,
        canvas,
        footer,
    };
    if app.geometry.canvas.width == 0 {
        return;
    }
    let buffer = frame.buffer_mut();
    draw_axis(app, buffer);
    draw_rows(app, buffer);
    draw_footer(app, buffer, footer);
}

fn canvas_columns(app: &App) -> impl Iterator<Item = (u16, f64)> + '_ {
    let canvas = app.geometry.canvas;
    (0..canvas.width).map(move |offset| (canvas.x + offset, app.minute_at(i64::from(offset))))
}

/// Tick spacing: the smallest step that leaves room for an offset label.
fn tick_step_minutes(minutes_per_column: f64) -> f64 {
    [
        1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 180.0, 360.0, 720.0, 1440.0,
    ]
    .into_iter()
    .find(|step| step / minutes_per_column >= 7.0)
    .unwrap_or(1440.0)
}

fn draw_axis(app: &App, buffer: &mut Buffer) {
    let axis = app.geometry.axis;
    let canvas = app.geometry.canvas;
    if axis.height < AXIS_ROWS {
        return;
    }
    let (day_row, tick_row, rule_row) = (axis.y, axis.y + 1, axis.y + 2);
    let header = Style::new().fg(Color::DarkGray);
    buffer.set_stringn(
        axis.x,
        day_row,
        "tx timeline",
        usize::from(app.geometry.label_width),
        header.add_modifier(Modifier::BOLD),
    );
    buffer.set_stringn(
        axis.x,
        tick_row,
        format!(
            "{} lanes · {} groups",
            app.payload.lanes.len(),
            app.payload.groups.len()
        ),
        usize::from(app.geometry.label_width),
        header,
    );
    for x in canvas.x..canvas.x + canvas.width {
        put(buffer, x, rule_row, "─", header);
    }
    // ticks every `step` minutes back from now; the now column itself carries the clock
    let step = tick_step_minutes(app.minutes_per_column());
    let mut last_label_end: i64 = -1;
    for (x, minute) in canvas_columns(app) {
        let next = app.minute_at(i64::from(x - canvas.x) + 1);
        let age = app.now - minute;
        let steps_back = (age / step).floor();
        let tick = app.now - steps_back * step;
        if steps_back >= 1.0 && tick >= minute && tick < next {
            put(buffer, x, rule_row, "┬", header);
            let label = age_label(steps_back * step);
            let width = label.len() as u16;
            if i64::from(x) > last_label_end && x + width <= canvas.x + canvas.width {
                buffer.set_stringn(
                    x,
                    tick_row,
                    &label,
                    usize::from(width),
                    Style::new().fg(Color::Gray),
                );
                last_label_end = i64::from(x + width);
            }
        }
    }
    // day labels: the running day pins to the left edge until it ends
    let mut last_day_end: i64 = -1;
    for day in &app.payload.days {
        let column = app.column_of(day.offset);
        let end_column = app
            .column_of(day.offset + 1440.0)
            .min(i64::from(canvas.width));
        if end_column <= 0 || column >= i64::from(canvas.width) {
            continue;
        }
        let start = column.max(0);
        if column >= 0 {
            put(
                buffer,
                canvas.x + column as u16,
                rule_row,
                "┳",
                header.add_modifier(Modifier::BOLD),
            );
        }
        let room = (end_column - start).max(0) as usize;
        if start > last_day_end && room >= 3 {
            let style = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);
            buffer.set_stringn(
                canvas.x + start as u16,
                day_row,
                &day.label,
                room.min(day.label.len()),
                style,
            );
            last_day_end = start + day.label.len() as i64;
        }
    }
    draw_now_column(app, buffer, day_row, rule_row + 1, true);
}

fn draw_now_column(app: &App, buffer: &mut Buffer, top: u16, bottom: u16, label: bool) {
    let column = app.column_of(app.now);
    if !app.on_canvas(column) {
        return;
    }
    let x = app.geometry.canvas.x + column as u16;
    for y in top..bottom {
        put(buffer, x, y, "┃", Style::new().fg(NOW));
    }
    if label {
        // "now HH:MM", right-aligned against the now column
        let text = format!(
            "now {}",
            clock::clock_label(app.payload.timestamp(app.now))
        );
        let width = text.len() as u16;
        let label_x = x.saturating_sub(width).max(app.geometry.canvas.x);
        buffer.set_stringn(
            label_x,
            top,
            &text,
            usize::from(width),
            Style::new().fg(NOW).add_modifier(Modifier::BOLD),
        );
    }
}

/// An offset back from now as a tick label: `-5m`, `-1h`, `-1h30`, `-2d`, `-1d6h`.
fn age_label(minutes: f64) -> String {
    let minutes = minutes.round() as i64;
    let (days, rest) = (minutes / 1440, minutes % 1440);
    let (hours, rest) = (rest / 60, rest % 60);
    match (days, hours, rest) {
        (0, 0, minutes) => format!("-{minutes}m"),
        (0, hours, 0) => format!("-{hours}h"),
        (0, hours, minutes) => format!("-{hours}h{minutes:02}"),
        (days, 0, _) => format!("-{days}d"),
        (days, hours, _) => format!("-{days}d{hours}h"),
    }
}

fn draw_rows(app: &App, buffer: &mut Buffer) {
    let lanes_rect = app.geometry.lanes;
    let canvas = app.geometry.canvas;
    let rows = app.rows();
    let height = usize::from(lanes_rect.height);
    let visible: Vec<(u16, RowItem)> = rows
        .iter()
        .skip(app.row_scroll)
        .take(height)
        .enumerate()
        .map(|(offset, row)| (lanes_rect.y + offset as u16, *row))
        .collect();

    // grid: day boundaries
    for (y, _) in &visible {
        for day in &app.payload.days {
            let column = app.column_of(day.offset);
            if app.on_canvas(column) {
                put(
                    buffer,
                    canvas.x + column as u16,
                    *y,
                    "┊",
                    Style::new().fg(Color::Indexed(238)),
                );
            }
        }
    }

    let mut lane_row: Vec<Option<u16>> = vec![None; app.payload.lanes.len()];
    for (y, row) in &visible {
        match row {
            RowItem::Group(index) => draw_group_header(app, buffer, *y, *index),
            RowItem::Lane(index) => {
                lane_row[*index] = Some(*y);
                draw_lane(app, buffer, *y, *index);
            }
        }
    }
    draw_now_column(
        app,
        buffer,
        lanes_rect.y,
        lanes_rect.y + lanes_rect.height,
        false,
    );
    draw_message_arrows(app, buffer, &lane_row);
}

fn draw_group_header(app: &App, buffer: &mut Buffer, y: u16, index: usize) {
    let group = &app.payload.groups[index];
    let style = Style::new()
        .fg(Color::Indexed(group.cube))
        .add_modifier(Modifier::BOLD);
    let count = app
        .visible_lanes()
        .into_iter()
        .filter(|index| app.payload.lanes[*index].group == group.key)
        .count();
    let text = format!("{} ({count})", group.key.to_uppercase());
    buffer.set_stringn(
        app.geometry.lanes.x,
        y,
        &text,
        usize::from(app.geometry.lanes.width),
        style,
    );
}

fn draw_lane(app: &App, buffer: &mut Buffer, y: u16, index: usize) {
    let lane = &app.payload.lanes[index];
    let canvas = app.geometry.canvas;
    let group_color = Color::Indexed(app.payload.group_cube(&lane.group));
    let selected = app.cursor == Some(index);
    let muted = !app.lane_matches(lane) || app.lane_dim(lane);
    let dim = if muted {
        Modifier::DIM
    } else {
        Modifier::empty()
    };
    if selected {
        for x in app.geometry.lanes.x..app.geometry.lanes.x + app.geometry.lanes.width {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_bg(CURSOR_BG);
            }
        }
    }

    // State marker replaces the group stripe; group headings and bars carry the group color.
    let label_width = usize::from(app.geometry.label_width);
    let indicator = match (lane.live, lane.state.as_str()) {
        (false, _) => "×",
        (true, "working") => "●",
        (true, "idle" | "waiting") => "○",
        (true, _) => "·",
    };
    put(
        buffer,
        app.geometry.lanes.x,
        y,
        indicator,
        Style::new().fg(Color::Gray).add_modifier(Modifier::BOLD | dim),
    );
    let name_style = Style::new()
        .fg(if lane.live { Color::White } else { Color::Gray })
        .add_modifier(if lane.live {
            Modifier::BOLD
        } else {
            Modifier::empty()
        })
        .add_modifier(dim);
    let engine = lane.meta.split(" · ").next().unwrap_or("");
    let name_room = label_width.saturating_sub(3);
    let name: String = lane.name.chars().take(name_room).collect();
    buffer.set_stringn(app.geometry.lanes.x + 2, y, &name, name_room, name_style);
    let after_name = name.chars().count() + 3;
    if label_width > after_name + engine.len() + 1 {
        buffer.set_stringn(
            app.geometry.lanes.x + after_name as u16,
            y,
            engine,
            label_width - after_name - 1,
            Style::new().fg(Color::DarkGray).add_modifier(dim),
        );
    }

    // working bars, idle dashes from the first segment to the kill / now
    let segments = app.segments(lane);
    let idle_style = Style::new().fg(IDLE).add_modifier(dim);
    let bar_style = Style::new().fg(group_color).add_modifier(dim);
    let tail = lane.kill.unwrap_or(app.now);
    let first = segments.first().map_or(lane.a, |segment| segment.0);
    for (x, minute) in canvas_columns(app) {
        let next = app.minute_at(i64::from(x - canvas.x) + 1).min(app.now);
        let minute = minute.max(app.observation_start());
        if next <= minute || next <= first || minute >= tail {
            continue;
        }
        let active = segments
            .iter()
            .any(|(begin, end)| *end > minute && *begin < next);
        if active {
            put(buffer, x, y, "━", bar_style);
        } else {
            put(buffer, x, y, "╌", idle_style);
        }
    }
    if let Some(kill) = lane.kill {
        let column = app.column_of(kill);
        if app.observes(kill) && app.on_canvas(column) {
            put(
                buffer,
                canvas.x + column as u16,
                y,
                "×",
                Style::new()
                    .fg(group_color)
                    .add_modifier(Modifier::BOLD | dim),
            );
        }
    }
    let now_column = app.column_of(app.now);
    if lane.live && app.on_canvas(now_column) {
        put(
            buffer,
            canvas.x + now_column as u16 + 1,
            y,
            "▸",
            Style::new().fg(group_color).add_modifier(Modifier::BOLD),
        );
    }
}

/// Peer messages, lane to lane: `•` where it left, a dotted line, `▾`/`▴` where it arrived —
/// small glyphs, plain weight, so the arrows sit under the bars rather than over them. The
/// selected message is amber and bold; a bar the line crosses keeps its heavy horizontal (`┿`).
fn draw_message_arrows(app: &App, buffer: &mut Buffer, lane_row: &[Option<u16>]) {
    let canvas = app.geometry.canvas;
    let mut ordered = app.messages();
    // the selected message draws last so nothing covers it
    ordered.sort_by_key(|index| app.message == Some(*index));
    for index in ordered {
        let edge = &app.payload.edges[index];
        let Some(to_row) = lane_row[edge.to] else {
            continue;
        };
        let column = app.column_of(edge.at);
        if !app.on_canvas(column) {
            continue;
        }
        let x = canvas.x + column as u16;
        let selected = app.message == Some(index);
        let color = if selected { AMBER } else { MESSAGE };
        let style = if selected {
            Style::new().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(color)
        };
        match edge.from.and_then(|from| lane_row[from]) {
            Some(from_row) if from_row != to_row => {
                let (top, bottom) = (from_row.min(to_row), from_row.max(to_row));
                for y in top + 1..bottom {
                    let crossing = symbol_at(buffer, x, y) == "━";
                    put(buffer, x, y, if crossing { "┿" } else { "┆" }, style);
                }
                put(buffer, x, from_row, "•", style);
                let head = if from_row < to_row { "▾" } else { "▴" };
                put(buffer, x, to_row, head, style);
            }
            // a self-message, or a sender without a row: mark the arrival only
            _ => put(buffer, x, to_row, "▾", style),
        }
    }
}

fn draw_footer(app: &App, buffer: &mut Buffer, rect: Rect) {
    if rect.height == 0 {
        return;
    }
    let status_style = if app.message.is_some() {
        Style::new().fg(AMBER)
    } else {
        Style::new().fg(Color::White)
    };
    buffer.set_stringn(
        rect.x,
        rect.y,
        app.status_line(),
        usize::from(rect.width),
        status_style,
    );
    if rect.height > 1 {
        let hint_style = if app.searching {
            Style::new().fg(AMBER)
        } else {
            Style::new().fg(Color::DarkGray)
        };
        buffer.set_stringn(
            rect.x,
            rect.y + 1,
            app.hint_line(),
            usize::from(rect.width),
            hint_style,
        );
    }
}

/// The frame as plain text, one line per terminal row (snapshots, tests).
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
    fn age_labels() {
        assert_eq!(age_label(5.0), "-5m");
        assert_eq!(age_label(60.0), "-1h");
        assert_eq!(age_label(90.0), "-1h30");
        assert_eq!(age_label(1440.0), "-1d");
        assert_eq!(age_label(1440.0 + 360.0), "-1d6h");
    }
}
