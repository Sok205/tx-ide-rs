//! Timeline state and input — pure: keys and mouse in, state out; [`super::ui`] draws it.
//!
//! The view shows two things per lane: when the session was working (bars) or idle, and the
//! peer messages it sent or received (arrows between lanes). Everything else the payload carries
//! (you-spoke events, milestones, spawn edges) stays in the model for parity and is not drawn.

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use super::model::{Edge, EdgeKind, Lane, Payload};

/// Minutes per canvas column, fine to coarse (15 s up to 8 h).
pub const ZOOM_LEVELS: [f64; 12] = [
    0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 240.0, 480.0,
];
/// At or below this many minutes per column the fine segment tier (`fsegs`, > 10 min idle)
/// is drawn — the HTML view's 60 px/h switch.
pub const FINE_TIER_MAX: f64 = 10.0;
/// Columns kept free right of "now" while following.
pub const FOLLOW_MARGIN: f64 = 3.0;
/// The default initial viewport: the last 4.5 h, glued to now (`--last` overrides).
pub const DEFAULT_WINDOW_MIN: f64 = 270.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowItem {
    Group(usize),
    Lane(usize),
}

/// Where the last frame put things; mouse hits resolve against it.
#[derive(Clone, Copy, Debug, Default)]
pub struct Geometry {
    pub label_width: u16,
    pub axis: Rect,
    pub lanes: Rect,
    pub canvas: Rect,
    pub drawer: Option<Rect>,
    pub footer: Rect,
}

pub struct App {
    pub payload: Payload,
    /// Minutes from the payload epoch.
    pub now: f64,
    /// The initial viewport length, minutes — what `fit_initial` makes fit.
    pub window: f64,
    pub zoom: usize,
    /// The user zoomed by hand: a resize keeps that zoom instead of re-fitting the window.
    pub custom_zoom: bool,
    /// The minute at canvas column 0.
    pub left: f64,
    pub follow: bool,
    /// Lane index into `payload.lanes`.
    pub cursor: Option<usize>,
    /// Edge index into `payload.edges`: the selected message.
    pub message: Option<usize>,
    pub row_scroll: usize,
    pub show_sys: bool,
    pub hide_idle: bool,
    pub search: String,
    pub searching: bool,
    pub drawer: bool,
    pub geometry: Geometry,
    pub quit: bool,
    drag: Option<(u16, f64)>,
}

impl App {
    pub fn new(payload: Payload, now_seconds: f64, window: f64) -> Self {
        let now = payload.minutes(now_seconds);
        let mut app = Self {
            payload,
            now,
            window,
            zoom: 4,
            custom_zoom: false,
            left: 0.0,
            follow: true,
            cursor: None,
            message: None,
            row_scroll: 0,
            show_sys: false,
            hide_idle: false,
            search: String::new(),
            searching: false,
            drawer: true,
            geometry: Geometry::default(),
            quit: false,
            drag: None,
        };
        app.cursor = app.visible_lanes().first().copied();
        app
    }

    // ----- time ↔ column ---------------------------------------------------------------------

    pub fn minutes_per_column(&self) -> f64 {
        ZOOM_LEVELS[self.zoom]
    }

    pub fn canvas_width(&self) -> f64 {
        f64::from(self.geometry.canvas.width.max(1))
    }

    /// Canvas column (may be off-canvas) of a minute.
    pub fn column_of(&self, minute: f64) -> i64 {
        ((minute - self.left) / self.minutes_per_column()).floor() as i64
    }

    pub fn on_canvas(&self, column: i64) -> bool {
        column >= 0 && column < i64::from(self.geometry.canvas.width)
    }

    /// The minute at a canvas column.
    pub fn minute_at(&self, column: i64) -> f64 {
        self.left + column as f64 * self.minutes_per_column()
    }

    pub fn right(&self) -> f64 {
        self.left + self.canvas_width() * self.minutes_per_column()
    }

    pub fn uses_fine_tier(&self) -> bool {
        self.minutes_per_column() <= FINE_TIER_MAX
    }

    pub fn segments<'a>(&self, lane: &'a Lane) -> &'a [(f64, f64)] {
        if self.uses_fine_tier() {
            &lane.fsegs
        } else {
            &lane.segs
        }
    }

    /// Pick the finest zoom that fits the initial window, glue to now.
    pub fn fit_initial(&mut self) {
        let width = self.canvas_width();
        self.zoom = ZOOM_LEVELS
            .iter()
            .position(|level| level * width >= self.window)
            .unwrap_or(ZOOM_LEVELS.len() - 1);
        self.jump_to_now();
    }

    pub fn jump_to_now(&mut self) {
        self.left = self.now + FOLLOW_MARGIN * self.minutes_per_column()
            - self.canvas_width() * self.minutes_per_column();
        self.follow = true;
    }

    fn pan(&mut self, minutes: f64) {
        self.left += minutes;
        let right_limit = self.now + FOLLOW_MARGIN * self.minutes_per_column();
        if self.right() >= right_limit {
            self.left = right_limit - self.canvas_width() * self.minutes_per_column();
            self.follow = true;
        } else {
            self.follow = false;
        }
        if self.left < 0.0 {
            self.left = 0.0;
        }
    }

    fn zoom_to(&mut self, zoom: usize, anchor_minute: f64, anchor_column: f64) {
        self.custom_zoom = true;
        self.zoom = zoom.min(ZOOM_LEVELS.len() - 1);
        self.left = anchor_minute - anchor_column * self.minutes_per_column();
        self.pan(0.0);
    }

    fn zoom_anchor(&self) -> (f64, f64) {
        if let Some(edge) = self.selected_message() {
            let column = self.column_of(edge.at);
            if self.on_canvas(column) {
                return (edge.at, column as f64);
            }
        }
        let column = (self.canvas_width() / 2.0).floor();
        (self.minute_at(column as i64), column)
    }

    fn zoom_by(&mut self, delta: i64) {
        let zoom = (self.zoom as i64 + delta).clamp(0, ZOOM_LEVELS.len() as i64 - 1) as usize;
        if zoom != self.zoom {
            let (minute, column) = self.zoom_anchor();
            self.zoom_to(zoom, minute, column);
        }
    }

    fn ensure_minute_visible(&mut self, minute: f64) {
        if !self.on_canvas(self.column_of(minute)) {
            let shift = minute - self.minute_at((self.canvas_width() / 2.0) as i64);
            self.pan(shift);
        }
    }

    // ----- rows ------------------------------------------------------------------------------

    /// A lane with no segment of the current tier in the viewport.
    pub fn lane_dim(&self, lane: &Lane) -> bool {
        let (left, right) = (self.left, self.right());
        !self
            .segments(lane)
            .iter()
            .any(|(begin, end)| *end >= left && *begin <= right)
    }

    pub fn lane_matches(&self, lane: &Lane) -> bool {
        if self.search.is_empty() {
            return true;
        }
        let needle = self.search.to_lowercase();
        [&lane.name, &lane.group, &lane.meta]
            .iter()
            .any(|field| field.to_lowercase().contains(&needle))
    }

    /// Group headers and lanes in display order, after the sys / idle toggles.
    pub fn rows(&self) -> Vec<RowItem> {
        let mut rows = Vec::new();
        for (group_index, group) in self.payload.groups.iter().enumerate() {
            let lanes: Vec<usize> = self
                .payload
                .lanes
                .iter()
                .enumerate()
                .filter(|(_, lane)| lane.group == group.key)
                .filter(|(_, lane)| self.show_sys || !lane.sys)
                .filter(|(_, lane)| !self.hide_idle || !self.lane_dim(lane))
                .map(|(index, _)| index)
                .collect();
            if lanes.is_empty() {
                continue;
            }
            rows.push(RowItem::Group(group_index));
            rows.extend(lanes.into_iter().map(RowItem::Lane));
        }
        rows
    }

    pub fn visible_lanes(&self) -> Vec<usize> {
        self.rows()
            .into_iter()
            .filter_map(|row| match row {
                RowItem::Lane(index) => Some(index),
                RowItem::Group(_) => None,
            })
            .collect()
    }

    pub fn cursor_lane(&self) -> Option<&Lane> {
        self.cursor.map(|index| &self.payload.lanes[index])
    }

    fn move_cursor(&mut self, delta: i64) {
        let lanes = self.visible_lanes();
        if lanes.is_empty() {
            self.cursor = None;
            return;
        }
        let position = self
            .cursor
            .and_then(|current| lanes.iter().position(|index| *index == current))
            .map_or(0, |position| {
                (position as i64 + delta).rem_euclid(lanes.len() as i64) as usize
            });
        self.cursor = Some(lanes[position]);
        self.ensure_cursor_row_visible();
    }

    fn ensure_cursor_row_visible(&mut self) {
        let Some(cursor) = self.cursor else { return };
        let rows = self.rows();
        let Some(row) = rows.iter().position(|row| *row == RowItem::Lane(cursor)) else {
            return;
        };
        let height = usize::from(self.geometry.lanes.height.max(1));
        if row < self.row_scroll {
            self.row_scroll = row.saturating_sub(1);
        } else if row >= self.row_scroll + height {
            self.row_scroll = row + 1 - height;
        }
    }

    // ----- messages --------------------------------------------------------------------------

    /// Edge indices of every peer message, in time order — the bottom panel's log.
    pub fn messages(&self) -> Vec<usize> {
        self.payload
            .edges
            .iter()
            .enumerate()
            .filter(|(_, edge)| edge.kind == EdgeKind::Message)
            .map(|(index, _)| index)
            .collect()
    }

    /// Edge indices of the peer messages the cursor lane sent or received, in time order.
    pub fn lane_messages(&self) -> Vec<usize> {
        let Some(cursor) = self.cursor else {
            return Vec::new();
        };
        self.payload
            .edges
            .iter()
            .enumerate()
            .filter(|(_, edge)| edge.kind == EdgeKind::Message)
            .filter(|(_, edge)| edge.to == cursor || edge.from == Some(cursor))
            .map(|(index, _)| index)
            .collect()
    }

    pub fn selected_message(&self) -> Option<&Edge> {
        self.message.map(|index| &self.payload.edges[index])
    }

    fn select_message(&mut self, delta: i64) {
        let messages = self.messages();
        if messages.is_empty() {
            self.message = None;
            return;
        }
        let count = messages.len() as i64;
        let position = match self
            .message
            .and_then(|current| messages.iter().position(|index| *index == current))
        {
            Some(current) => (current as i64 + delta).rem_euclid(count),
            // first step from nothing: the last message in view, else the last message
            None => {
                let right = self.right();
                messages
                    .iter()
                    .rposition(|index| self.payload.edges[*index].at <= right)
                    .map_or(count - 1, |position| position as i64)
            }
        } as usize;
        self.message = Some(messages[position]);
        let minute = self.payload.edges[messages[position]].at;
        self.ensure_minute_visible(minute);
    }

    /// `from → to` for an edge, by lane names.
    pub fn message_parties(&self, edge: &Edge) -> (String, String) {
        let sender = edge.from.map_or_else(
            || edge.token.clone(),
            |index| self.payload.lanes[index].name.clone(),
        );
        (sender, self.payload.lanes[edge.to].name.clone())
    }

    // ----- input -----------------------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.searching {
            self.handle_search_key(key);
            return;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let quarter = self.canvas_width() * self.minutes_per_column() / 4.0;
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if control => self.quit = true,
            KeyCode::Char('h') | KeyCode::Left if shift => self.pan(-quarter * 4.0),
            KeyCode::Char('l') | KeyCode::Right if shift => self.pan(quarter * 4.0),
            KeyCode::Char('H') => self.pan(-quarter * 4.0),
            KeyCode::Char('L') => self.pan(quarter * 4.0),
            KeyCode::Char('h') | KeyCode::Left => self.pan(-quarter),
            KeyCode::Char('l') | KeyCode::Right => self.pan(quarter),
            KeyCode::Char('+' | '=' | ']') => self.zoom_by(-1),
            KeyCode::Char('-' | '[') => self.zoom_by(1),
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Tab | KeyCode::Char('.') => self.select_message(1),
            KeyCode::BackTab | KeyCode::Char(',') => self.select_message(-1),
            KeyCode::Char('n') => self.jump_to_now(),
            KeyCode::Char('g') => self.pan(-self.left),
            KeyCode::Enter => self.drawer = !self.drawer,
            KeyCode::Esc => {
                if self.message.is_some() {
                    self.message = None;
                } else if !self.search.is_empty() {
                    self.search.clear();
                } else {
                    self.drawer = false;
                }
            }
            KeyCode::Char('s') => self.show_sys = !self.show_sys,
            KeyCode::Char('i') => self.hide_idle = !self.hide_idle,
            KeyCode::Char('/') => self.searching = true,
            _ => {}
        }
        if let Some(cursor) = self.cursor
            && !self.visible_lanes().contains(&cursor)
        {
            self.cursor = self.visible_lanes().first().copied();
            self.message = None;
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.search.clear();
                self.searching = false;
            }
            KeyCode::Enter => self.searching = false,
            KeyCode::Backspace => {
                self.search.pop();
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Char(character) => self.search.push(character),
            _ => {}
        }
    }

    fn row_at(&self, screen_row: u16) -> Option<RowItem> {
        let lanes = self.geometry.lanes;
        if screen_row < lanes.y || screen_row >= lanes.y + lanes.height {
            return None;
        }
        let index = usize::from(screen_row - lanes.y) + self.row_scroll;
        self.rows().get(index).copied()
    }

    fn inside(rect: Rect, column: u16, row: u16) -> bool {
        column >= rect.x
            && column < rect.x + rect.width
            && row >= rect.y
            && row < rect.y + rect.height
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        let (column, row) = (mouse.column, mouse.row);
        let control = mouse.modifiers.contains(KeyModifiers::CONTROL);
        let over_canvas = Self::inside(self.geometry.canvas, column, row)
            || Self::inside(self.geometry.axis, column, row);
        let over_drawer = self
            .geometry
            .drawer
            .is_some_and(|rect| Self::inside(rect, column, row));
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let direction: i64 = if mouse.kind == MouseEventKind::ScrollUp {
                    -1
                } else {
                    1
                };
                if control && over_canvas {
                    let canvas_column = i64::from(column) - i64::from(self.geometry.canvas.x);
                    let minute = self.minute_at(canvas_column);
                    let zoom =
                        (self.zoom as i64 + direction).clamp(0, ZOOM_LEVELS.len() as i64 - 1);
                    self.zoom_to(zoom as usize, minute, canvas_column as f64);
                } else if over_drawer {
                    self.select_message(direction);
                } else if over_canvas {
                    self.pan(direction as f64 * 3.0 * self.minutes_per_column());
                } else {
                    let total = self.rows().len();
                    let height = usize::from(self.geometry.lanes.height.max(1));
                    let maximum = total.saturating_sub(height) as i64;
                    self.row_scroll =
                        (self.row_scroll as i64 + direction).clamp(0, maximum) as usize;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                match self.row_at(row) {
                    Some(RowItem::Lane(index)) => {
                        self.cursor = Some(index);
                        self.message = None;
                        if column >= self.geometry.canvas.x {
                            let canvas_column = i64::from(column - self.geometry.canvas.x);
                            self.message = self.messages().into_iter().find(|edge| {
                                self.column_of(self.payload.edges[*edge].at) == canvas_column
                            });
                        }
                    }
                    Some(RowItem::Group(index)) => {
                        let key = self.payload.groups[index].key.clone();
                        self.search = if self.search == key {
                            String::new()
                        } else {
                            key
                        };
                    }
                    None => {}
                }
                if over_canvas {
                    self.drag = Some((column, self.left));
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((start_column, start_left)) = self.drag {
                    let delta = i64::from(start_column) - i64::from(column);
                    let target = start_left + delta as f64 * self.minutes_per_column();
                    self.pan(target - self.left);
                }
            }
            MouseEventKind::Up(_) => self.drag = None,
            _ => {}
        }
    }

    /// The terminal changed size: re-fit the window (unless zoomed by hand), re-glue to now.
    pub fn resized(&mut self) {
        if !self.custom_zoom {
            self.fit_initial();
        } else if self.follow {
            self.jump_to_now();
        }
    }

    /// A fresh payload from the refresh tick: keep the cursor by session id and the selected
    /// message by time, keep following.
    pub fn refresh(&mut self, payload: Payload, now_seconds: f64) {
        let cursor_sid = self.cursor_lane().map(|lane| lane.sid.clone());
        let message_at = self.selected_message().map(|edge| edge.at);
        self.payload = payload;
        self.now = self.payload.minutes(now_seconds);
        self.cursor = cursor_sid
            .and_then(|sid| self.payload.lanes.iter().position(|lane| lane.sid == sid))
            .or_else(|| self.visible_lanes().first().copied());
        self.message = message_at.and_then(|at| {
            self.messages()
                .into_iter()
                .find(|index| self.payload.edges[*index].at == at)
        });
        if self.follow {
            self.jump_to_now();
        }
    }

    // ----- footer text -----------------------------------------------------------------------

    pub fn status_line(&self) -> String {
        let Some(lane) = self.cursor_lane() else {
            return "no lanes in the window".to_owned();
        };
        match self.selected_message() {
            Some(edge) => {
                let (sender, receiver) = self.message_parties(edge);
                format!(
                    "{} · {sender} → {receiver}: {}",
                    super::clock::short_time(self.payload.timestamp(edge.at)),
                    first_line(&edge.snippet)
                )
            }
            None => {
                let messages = self.lane_messages().len();
                format!("{} · {} · {messages} messages", lane.name, lane.meta)
            }
        }
    }

    pub fn hint_line(&self) -> String {
        if self.searching {
            return format!("search: {}▏", self.search);
        }
        let mpc = self.minutes_per_column();
        let zoom = if mpc >= 60.0 {
            format!("{}h/col", mpc / 60.0)
        } else if mpc >= 1.0 {
            format!("{mpc}m/col")
        } else {
            format!("{}s/col", mpc * 60.0)
        };
        let flags = [
            (self.show_sys, "sys"),
            (self.hide_idle, "no-idle"),
            (self.follow, "follow"),
        ]
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, label)| *label)
        .collect::<Vec<_>>()
        .join(" ");
        format!(
            "h/l pan  [ ] zoom  j/k lane  tab/shift-tab message  enter panel  / search  s sys  i idle  n now  q   {zoom} {flags}"
        )
    }
}

pub fn first_line(text: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("").to_owned();
    if lines.next().is_some() {
        format!("{first} ⏎")
    } else {
        first
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::timeline::model::{Day, Group};

    fn lane(name: &str, group: &str, a: f64, b: f64) -> Lane {
        Lane {
            sid: format!("sid-{name}"),
            name: name.into(),
            group: group.into(),
            meta: "claude · alive · 3 msgs".into(),
            blurb: "hello".into(),
            state: "alive".into(),
            sys: false,
            live: true,
            kill: None,
            a,
            b,
            segs: vec![(a, b)],
            fsegs: vec![(a, b)],
            events: Vec::new(),
            rows: Vec::new(),
        }
    }

    fn payload() -> Payload {
        Payload {
            epoch: 0.0,
            t1: 600.0,
            days: vec![Day {
                offset: 0.0,
                label: "Thu Jan 01".into(),
                weekend: false,
            }],
            groups: vec![
                Group {
                    key: "alpha".into(),
                    cube: 210,
                },
                Group {
                    key: "beta".into(),
                    cube: 167,
                },
            ],
            lanes: vec![
                lane("one", "alpha", 10.0, 300.0),
                lane("two", "beta", 50.0, 600.0),
            ],
            edges: vec![
                Edge {
                    at: 100.0,
                    from: Some(0),
                    to: 1,
                    token: "one".into(),
                    snippet: "ping".into(),
                    kind: EdgeKind::Message,
                },
                Edge {
                    at: 120.0,
                    from: None,
                    to: 1,
                    token: "tx-assistant".into(),
                    snippet: String::new(),
                    kind: EdgeKind::Spawn,
                },
                Edge {
                    at: 400.0,
                    from: Some(1),
                    to: 0,
                    token: "two".into(),
                    snippet: "pong\nmore".into(),
                    kind: EdgeKind::Message,
                },
            ],
        }
    }

    fn app() -> App {
        let mut app = App::new(payload(), 700.0 * 60.0, DEFAULT_WINDOW_MIN);
        app.geometry.canvas = Rect::new(20, 3, 60, 10);
        app.geometry.lanes = Rect::new(0, 3, 80, 10);
        app.fit_initial();
        app
    }

    #[test]
    fn initial_fit_glues_to_now() {
        let app = app();
        assert_eq!(app.minutes_per_column(), 5.0);
        assert!(app.follow);
        assert!((app.right() - (700.0 + 15.0)).abs() < 1e-9);
    }

    #[test]
    fn rows_group_lanes_and_cursor_walks_them() {
        let mut app = app();
        assert_eq!(
            app.rows(),
            vec![
                RowItem::Group(0),
                RowItem::Lane(0),
                RowItem::Group(1),
                RowItem::Lane(1)
            ]
        );
        assert_eq!(app.cursor, Some(0));
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.cursor, Some(1));
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.cursor, Some(0));
    }

    #[test]
    fn messages_skip_spawns_and_cycle_by_time() {
        let mut app = app();
        assert_eq!(app.messages(), vec![0, 2]);
        assert_eq!(app.lane_messages(), vec![0, 2]);
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.message, Some(2));
        assert!(app.on_canvas(app.column_of(400.0)));
        assert!(app.status_line().ends_with("two → one: pong ⏎"));
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.message, Some(0));
        assert!(app.status_line().ends_with("one → two: ping"));
    }

    #[test]
    fn a_thirty_minute_window_fits_at_half_a_minute_per_column() {
        let mut app = App::new(payload(), 700.0 * 60.0, 30.0);
        app.geometry.canvas = Rect::new(20, 3, 60, 10);
        app.fit_initial();
        assert_eq!(app.minutes_per_column(), 0.5);
        assert!((app.right() - app.left - 30.0).abs() < 1e-9);
        assert!(app.hint_line().contains("30s/col"));
    }

    #[test]
    fn resize_refits_until_zoomed_by_hand() {
        let mut app = App::new(payload(), 700.0 * 60.0, 30.0);
        app.geometry.canvas = Rect::new(20, 3, 60, 10);
        app.fit_initial();
        assert_eq!(app.minutes_per_column(), 0.5);
        app.geometry.canvas = Rect::new(20, 3, 122, 10);
        app.resized();
        assert_eq!(app.minutes_per_column(), 0.25);
        app.handle_key(KeyEvent::from(KeyCode::Char('[')));
        app.geometry.canvas = Rect::new(20, 3, 60, 10);
        app.resized();
        assert_eq!(app.minutes_per_column(), 0.5);
    }

    #[test]
    fn zoom_keeps_anchor_column() {
        let mut app = app();
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        let before = app.column_of(400.0);
        app.handle_key(KeyEvent::from(KeyCode::Char(']')));
        assert_eq!(app.minutes_per_column(), 2.0);
        assert_eq!(app.column_of(400.0), before);
        for _ in 0..4 {
            app.handle_key(KeyEvent::from(KeyCode::Char(']')));
        }
        assert_eq!(app.minutes_per_column(), 0.25);
    }

    #[test]
    fn search_input_and_escape() {
        let mut app = app();
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.search, "t");
        assert!(!app.lane_matches(&app.payload.lanes[0]));
        assert!(app.lane_matches(&app.payload.lanes[1]));
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.search.is_empty());
    }

    #[test]
    fn refresh_keeps_cursor_and_message() {
        let mut app = app();
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        let selected_at = app.selected_message().unwrap().at;
        let mut next = payload();
        next.lanes.reverse();
        for edge in &mut next.edges {
            edge.to = 1 - edge.to;
            edge.from = edge.from.map(|from| 1 - from);
        }
        app.refresh(next, 710.0 * 60.0);
        assert_eq!(app.cursor_lane().unwrap().name, "two");
        assert_eq!(app.selected_message().unwrap().at, selected_at);
    }
}
