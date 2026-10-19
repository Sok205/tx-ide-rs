//! The viewer: a state machine over the newest `content/*.json`, plus the terminal loop.
//!
//! [`Viewer`] is pure enough to test without a terminal: it polls `content/`, reacts to
//! [`KeyCode`]s, appends JSON lines to `state/events`, and draws into a [`Buffer`].
//! [`run`] wraps it in the crossterm loop.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::Rect;

use crate::model::{DEFAULT_FPS, Kind, Node, Screen, parse};
use crate::render::{Layout, ViewState, draw};

pub const POLL_INTERVAL: Duration = Duration::from_millis(200);
pub const MIN_FPS: u32 = 4;
pub const MAX_FPS: u32 = 30;

/// What the terminal loop should do after a key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    /// Switch the operator back to where they came from.
    Back,
}

/// Identity of the file currently shown: path, mtime and length. Any change means re-read.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Source {
    path: PathBuf,
    modified: SystemTime,
    length: u64,
}

impl Source {
    fn name(&self) -> String {
        self.path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }
}

pub struct Viewer {
    content_dir: PathBuf,
    events_path: PathBuf,
    current: Option<Source>,
    screen: Option<Screen>,
    state: ViewState,
    started: Instant,
}

impl Viewer {
    /// `content/` and `state/` live under `session_dir`; both are created.
    pub fn new(session_dir: &Path) -> io::Result<Self> {
        let content_dir = session_dir.join("content");
        let state_dir = session_dir.join("state");
        fs::create_dir_all(&content_dir)?;
        fs::create_dir_all(&state_dir)?;
        Ok(Viewer {
            content_dir,
            events_path: state_dir.join("events"),
            current: None,
            screen: None,
            state: ViewState::default(),
            started: Instant::now(),
        })
    }

    /// Re-reads the newest `*.json` when its path, mtime or length changed.
    /// Returns true when what is drawn changed.
    pub fn poll(&mut self) -> bool {
        let Some(newest) = self.newest() else {
            return false;
        };
        if self.current.as_ref() == Some(&newest) {
            return false;
        }
        let new_path = self
            .current
            .as_ref()
            .is_none_or(|current| current.path != newest.path);
        if new_path {
            self.state = ViewState {
                layout: self.state.layout,
                ..ViewState::default()
            };
            self.started = Instant::now();
            // A (re)started viewer keeps events the agent has not read yet.
            if self.current.is_some() {
                let _ = fs::write(&self.events_path, "");
            }
        }
        let name = newest.name();
        self.current = Some(newest);
        if name.starts_with("waiting") {
            self.state.waiting = true;
            self.state.error = None;
            return true;
        }
        self.state.waiting = false;
        let path = &self.current.as_ref().unwrap().path;
        let loaded = fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|text| parse(&text).map_err(|error| error.to_string()));
        match loaded {
            Ok(screen) => {
                self.screen = Some(screen);
                self.state.error = None;
            }
            Err(error) => self.state.error = Some(format!("{name}: {error}")),
        }
        true
    }

    fn newest(&self) -> Option<Source> {
        let entries = fs::read_dir(&self.content_dir).ok()?;
        entries
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                let modified = metadata.modified().ok()?;
                let created = metadata.created().unwrap_or(modified);
                Some((
                    created,
                    Source {
                        path: entry.path(),
                        modified,
                        length: metadata.len(),
                    },
                ))
            })
            // Equal mtimes (coarse filesystems): the later-created file wins, then the path.
            .max_by(|(a_created, a), (b_created, b)| {
                a.modified
                    .cmp(&b.modified)
                    .then_with(|| a_created.cmp(b_created))
                    .then_with(|| a.path.cmp(&b.path))
            })
            .map(|(_, source)| source)
    }

    pub fn key(&mut self, code: KeyCode) -> Action {
        match code {
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                if index < self.option_count() {
                    self.state.selected = Some(index);
                    self.state.focus = index;
                    self.append_event("select", Some(index));
                }
                Action::None
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.move_focus(-1);
                Action::None
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.move_focus(1);
                Action::None
            }
            KeyCode::Tab => {
                self.state.layout = match self.state.layout {
                    Layout::All => Layout::One,
                    Layout::One => Layout::All,
                };
                Action::None
            }
            KeyCode::Char('r') => {
                self.started = Instant::now();
                Action::None
            }
            KeyCode::Enter => {
                self.append_event("confirm", self.state.selected);
                Action::Back
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.append_event("dismiss", None);
                Action::Back
            }
            _ => Action::None,
        }
    }

    pub fn draw(&self, area: Rect, buf: &mut Buffer) {
        match &self.screen {
            Some(screen) => draw(screen, &self.state, self.started.elapsed(), area, buf),
            None => {
                let empty = Screen {
                    title: None,
                    sub: None,
                    fps: DEFAULT_FPS,
                    options: Vec::new(),
                    body: None,
                };
                draw(&empty, &self.state, self.started.elapsed(), area, buf);
            }
        }
    }

    /// The current screen's frame rate, for the draw cadence.
    /// The redraw rate: the fastest of the screen fps and every `frames` node's own fps.
    pub fn fps(&self) -> u32 {
        self.screen.as_ref().map_or(DEFAULT_FPS, |screen| {
            screen
                .options
                .iter()
                .map(|choice| &choice.body)
                .chain(&screen.body)
                .map(max_node_fps)
                .fold(screen.fps, u32::max)
        })
    }

    fn option_count(&self) -> usize {
        self.screen
            .as_ref()
            .map_or(0, |screen| screen.options.len())
    }

    fn move_focus(&mut self, delta: isize) {
        let count = self.option_count();
        if count == 0 {
            return;
        }
        let next = self.state.focus as isize + delta;
        self.state.focus = next.rem_euclid(count as isize) as usize;
    }

    fn append_event(&self, kind: &str, choice: Option<usize>) {
        let choice = choice.and_then(|index| self.screen.as_ref()?.options.get(index));
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let event = serde_json::json!({
            "type": kind,
            "choice": choice.map(|choice| choice.id.as_str()),
            "title": choice.map(|choice| choice.title.as_str()),
            "screen": self.current.as_ref().map(Source::name),
            "timestamp": timestamp,
        });
        let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)
        else {
            return;
        };
        let _ = writeln!(file, "{event}");
    }
}

fn max_node_fps(node: &Node) -> u32 {
    match &node.kind {
        Kind::Frames { fps, .. } => fps.unwrap_or(0),
        Kind::Row(children) | Kind::Col(children) => {
            children.iter().map(max_node_fps).max().unwrap_or(0)
        }
        Kind::Block { body, .. } => max_node_fps(body),
        Kind::Text(_) | Kind::List { .. } | Kind::Table { .. } => 0,
    }
}

/// Terminal loop: poll every [`POLL_INTERVAL`], draw at the screen's fps (clamped to
/// [`MIN_FPS`]..=[`MAX_FPS`]), feed key presses to the viewer, and call `on_back` on
/// [`Action::Back`]. Never returns on its own; the verb decides when to stop it.
pub fn run(session_dir: &Path, mut on_back: impl FnMut()) -> io::Result<()> {
    let mut viewer = Viewer::new(session_dir)?;
    let mut terminal = ratatui::init();
    let result = event_loop(&mut viewer, &mut terminal, &mut on_back);
    ratatui::restore();
    result
}

fn event_loop(
    viewer: &mut Viewer,
    terminal: &mut ratatui::DefaultTerminal,
    on_back: &mut impl FnMut(),
) -> io::Result<()> {
    let mut last_poll = Instant::now() - POLL_INTERVAL;
    loop {
        if last_poll.elapsed() >= POLL_INTERVAL {
            viewer.poll();
            last_poll = Instant::now();
        }
        terminal.draw(|frame| viewer.draw(frame.area(), frame.buffer_mut()))?;
        let frame = Duration::from_secs(1) / viewer.fps().clamp(MIN_FPS, MAX_FPS);
        if event::poll(frame)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && viewer.key(key.code) == Action::Back
        {
            on_back();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::thread::sleep;
    use std::time::Duration;

    use ratatui::crossterm::event::KeyCode;
    use tempfile::{TempDir, tempdir};

    use super::*;

    const FIRST: &str = r#"{"title":"First","body":{"text":"one"}}"#;
    const SECOND: &str = r#"{"title":"Second","body":{"text":"two"}}"#;
    const THREE_OPTIONS: &str = r#"{"title":"Pick one","options":[
        {"id":"a","title":"Alpha","body":{"text":"body a"}},
        {"id":"b","title":"Beta","body":{"text":"body b"}},
        {"id":"c","title":"Gamma","body":{"text":"body c"}}]}"#;

    fn setup() -> (TempDir, Viewer) {
        let dir = tempdir().unwrap();
        let viewer = Viewer::new(dir.path()).unwrap();
        (dir, viewer)
    }

    /// Writes `content/<name>`, sleeping first so mtimes strictly increase.
    fn write(dir: &Path, name: &str, text: &str) {
        sleep(Duration::from_millis(10));
        fs::write(dir.join("content").join(name), text).unwrap();
    }

    fn events(dir: &Path) -> Vec<serde_json::Value> {
        let path = dir.join("state").join("events");
        if !path.exists() {
            return Vec::new();
        }
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn title(viewer: &Viewer) -> &str {
        viewer.screen.as_ref().unwrap().title.as_deref().unwrap()
    }

    #[test]
    fn new_creates_content_and_state_dirs() {
        let (dir, _viewer) = setup();
        assert!(dir.path().join("content").is_dir());
        assert!(dir.path().join("state").is_dir());
    }

    #[test]
    fn poll_without_files_is_false() {
        let (_dir, mut viewer) = setup();
        assert!(!viewer.poll());
        assert!(viewer.screen.is_none());
    }

    #[test]
    fn newest_file_wins() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "a.json", FIRST);
        write(dir.path(), "b.json", SECOND);
        assert!(viewer.poll());
        assert_eq!(title(&viewer), "Second");
        assert!(!viewer.poll(), "unchanged file must not re-report");
    }

    #[test]
    fn parse_error_keeps_last_good() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "a.json", FIRST);
        viewer.poll();
        write(
            dir.path(),
            "b.json",
            r#"{"title":"Broken","body":{"text":1}}"#,
        );
        viewer.poll();
        let error = viewer.state.error.as_deref().unwrap();
        assert!(error.starts_with("b.json: "), "{error}");
        assert_eq!(title(&viewer), "First");
    }

    #[test]
    fn half_written_file_then_complete_loads() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "a.json", &SECOND[..SECOND.len() / 2]);
        viewer.poll();
        assert!(viewer.state.error.is_some());
        assert!(viewer.screen.is_none());
        write(dir.path(), "a.json", SECOND);
        assert!(viewer.poll());
        assert_eq!(viewer.state.error, None);
        assert_eq!(title(&viewer), "Second");
    }

    #[test]
    fn waiting_file_sets_waiting() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "a.json", FIRST);
        viewer.poll();
        write(dir.path(), "waiting-1.json", "not json at all");
        assert!(viewer.poll());
        assert!(viewer.state.waiting);
        assert_eq!(viewer.state.error, None);
        write(dir.path(), "b.json", SECOND);
        viewer.poll();
        assert!(!viewer.state.waiting);
        assert_eq!(title(&viewer), "Second");
    }

    #[test]
    fn select_writes_event() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "layout.json", THREE_OPTIONS);
        viewer.poll();
        assert!(matches!(viewer.key(KeyCode::Char('2')), Action::None));
        assert_eq!(viewer.state.selected, Some(1));
        assert_eq!(viewer.state.focus, 1);
        let events = events(dir.path());
        assert_eq!(events.len(), 1, "{events:?}");
        assert_eq!(events[0]["type"], "select");
        assert_eq!(events[0]["choice"], "b");
        assert_eq!(events[0]["title"], "Beta");
        assert_eq!(events[0]["screen"], "layout.json");
        assert!(events[0]["timestamp"].as_u64().unwrap() > 1_700_000_000);
    }

    #[test]
    fn select_out_of_range_is_ignored() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "layout.json", THREE_OPTIONS);
        viewer.poll();
        assert!(matches!(viewer.key(KeyCode::Char('5')), Action::None));
        assert_eq!(viewer.state.selected, None);
        assert!(events(dir.path()).is_empty());
    }

    #[test]
    fn enter_confirms_and_returns_back() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "layout.json", THREE_OPTIONS);
        viewer.poll();
        viewer.key(KeyCode::Char('2'));
        assert!(matches!(viewer.key(KeyCode::Enter), Action::Back));
        let events = events(dir.path());
        assert_eq!(events.len(), 2, "{events:?}");
        assert_eq!(events[1]["type"], "confirm");
        assert_eq!(events[1]["choice"], "b");
        assert_eq!(events[1]["title"], "Beta");
    }

    #[test]
    fn enter_without_selection_confirms_null() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "layout.json", THREE_OPTIONS);
        viewer.poll();
        assert!(matches!(viewer.key(KeyCode::Enter), Action::Back));
        let events = events(dir.path());
        assert_eq!(events[0]["type"], "confirm");
        assert!(events[0]["choice"].is_null());
    }

    #[test]
    fn esc_dismisses() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "layout.json", THREE_OPTIONS);
        viewer.poll();
        assert!(matches!(viewer.key(KeyCode::Esc), Action::Back));
        let events = events(dir.path());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["type"], "dismiss");
        assert!(events[0]["choice"].is_null());
        assert!(events[0]["title"].is_null());
        assert_eq!(events[0]["screen"], "layout.json");
    }

    #[test]
    fn q_dismisses() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "layout.json", THREE_OPTIONS);
        viewer.poll();
        assert!(matches!(viewer.key(KeyCode::Char('q')), Action::Back));
        assert_eq!(events(dir.path())[0]["type"], "dismiss");
    }

    #[test]
    fn new_screen_truncates_events() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "a.json", THREE_OPTIONS);
        viewer.poll();
        viewer.key(KeyCode::Char('1'));
        assert_eq!(events(dir.path()).len(), 1);
        write(dir.path(), "b.json", SECOND);
        viewer.poll();
        assert!(events(dir.path()).is_empty());
        assert_eq!(viewer.state.selected, None);
        assert_eq!(viewer.state.focus, 0);
    }

    #[test]
    fn restarted_viewer_keeps_unread_events() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "a.json", THREE_OPTIONS);
        viewer.poll();
        viewer.key(KeyCode::Char('1'));
        let mut restarted = Viewer::new(dir.path()).unwrap();
        restarted.poll();
        assert_eq!(events(dir.path()).len(), 1);
    }

    #[test]
    fn arrows_wrap() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "a.json", THREE_OPTIONS);
        viewer.poll();
        viewer.key(KeyCode::Left);
        assert_eq!(viewer.state.focus, 2);
        viewer.key(KeyCode::Right);
        assert_eq!(viewer.state.focus, 0);
        viewer.key(KeyCode::Char('l'));
        assert_eq!(viewer.state.focus, 1);
        viewer.key(KeyCode::Char('h'));
        assert_eq!(viewer.state.focus, 0);
        assert!(events(dir.path()).is_empty());
    }

    #[test]
    fn arrows_without_options_do_nothing() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "a.json", FIRST);
        viewer.poll();
        viewer.key(KeyCode::Left);
        assert_eq!(viewer.state.focus, 0);
    }

    #[test]
    fn tab_toggles_layout() {
        let (_dir, mut viewer) = setup();
        assert_eq!(viewer.state.layout, Layout::All);
        viewer.key(KeyCode::Tab);
        assert_eq!(viewer.state.layout, Layout::One);
        viewer.key(KeyCode::Tab);
        assert_eq!(viewer.state.layout, Layout::All);
    }

    #[test]
    fn r_resets_started() {
        let (_dir, mut viewer) = setup();
        sleep(Duration::from_millis(20));
        viewer.key(KeyCode::Char('r'));
        assert!(viewer.started.elapsed() < Duration::from_millis(15));
    }

    #[test]
    fn draw_without_screen_shows_error_bar() {
        let (dir, mut viewer) = setup();
        write(dir.path(), "a.json", "{");
        viewer.poll();
        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        viewer.draw(area, &mut buf);
        let last: String = (0..60)
            .map(|x| buf.cell((x, 5)).unwrap().symbol().to_string())
            .collect();
        assert!(last.contains("a.json: "), "{last}");
    }

    #[test]
    fn fps_is_the_fastest_frames_node() {
        let (dir, mut viewer) = setup();
        write(
            dir.path(),
            "a.json",
            r#"{"fps":4,"body":{"col":[{"text":"x"},{"block":"b","body":{"frames":["a","b"],"fps":20}}]}}"#,
        );
        viewer.poll();
        assert_eq!(viewer.fps(), 20);
    }

    #[test]
    fn fps_falls_back_to_default_without_screen() {
        let (_dir, viewer) = setup();
        assert_eq!(viewer.fps(), crate::model::DEFAULT_FPS);
    }
}
