//! The menu's state machine: which items show, and what a key does. Pure: [`Context`] is
//! gathered by the verbs, [`Menu::key`] turns a key into an [`Outcome`], nothing here touches
//! tmux or the store.

use std::collections::BTreeSet;
use std::time::Duration;

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    NewSession,
    Attach,
    History,
    Fork,
    Timeline,
    Artifacts,
    Plugins,
    Keys,
    Tutor,
    Assistant,
    Quit,
}

/// Every item, in menu order.
pub const ITEMS: [Item; 11] = [
    Item::NewSession,
    Item::Attach,
    Item::History,
    Item::Fork,
    Item::Timeline,
    Item::Artifacts,
    Item::Plugins,
    Item::Keys,
    Item::Tutor,
    Item::Assistant,
    Item::Quit,
];

impl Item {
    pub fn label(self) -> &'static str {
        match self {
            Item::NewSession => "New tx session",
            Item::Attach => "Attach to running",
            Item::History => "Resume from history",
            Item::Fork => "Fork this session",
            Item::Timeline => "Timeline",
            Item::Artifacts => "Artifacts",
            Item::Plugins => "Plugins",
            Item::Keys => "Keys cheat sheet",
            Item::Tutor => "Tutor",
            Item::Assistant => "Assistant",
            Item::Quit => "Quit",
        }
    }

    /// The single key that picks the item: the IDE's prefix key for the same thing wherever
    /// one exists. The cheat sheet is `K`: `k` moves up.
    pub fn key(self) -> char {
        match self {
            Item::NewSession => 'n',
            Item::Attach => 't',
            Item::History => 'h',
            Item::Fork => 'f',
            Item::Timeline => 'T',
            Item::Artifacts => 'a',
            Item::Plugins => 'p',
            Item::Keys => 'K',
            Item::Tutor => '?',
            Item::Assistant => '/',
            Item::Quit => 'q',
        }
    }

    pub fn section(self) -> &'static str {
        match self {
            Item::NewSession | Item::Attach | Item::History | Item::Fork => "sessions",
            Item::Timeline | Item::Artifacts => "look around",
            Item::Plugins | Item::Keys => "setup",
            Item::Tutor | Item::Assistant | Item::Quit => "learn",
        }
    }

    /// The plugins whose verbs the item runs; it is hidden unless every one is active.
    pub fn needs(self) -> &'static [&'static str] {
        match self {
            Item::NewSession => &["spawn"],
            Item::Attach => &["picker"],
            Item::History => &["history", "chat"],
            Item::Fork => &["chat"],
            Item::Timeline => &["timeline"],
            Item::Artifacts => &["artifacts"],
            Item::Tutor => &["tutor"],
            Item::Assistant => &["assistant"],
            Item::Plugins | Item::Keys | Item::Quit => &[],
        }
    }
}

/// A past (exited or stopped) session, newest first in [`Context::history`].
#[derive(Clone, Debug, PartialEq)]
pub struct Past {
    pub id: String,
    pub name: String,
    /// The first tag, or empty.
    pub tag: String,
    /// When it ended (or was created, without an end), epoch seconds.
    pub at: f64,
    pub chats: usize,
    pub cwd: String,
}

/// One line of the cheat sheet: a fragment's `# keys:` and its first prose comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyHint {
    pub plugin: String,
    pub keys: Vec<String>,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    /// Ids of the active plugins.
    pub active: BTreeSet<String>,
    /// Every plugin with whether it is active, manifest order.
    pub plugins: Vec<(String, bool)>,
    /// The launching pane shows a tx session (fork and artifacts need one).
    pub pane_session: bool,
    pub history: Vec<Past>,
    pub keys: Vec<KeyHint>,
    /// Running agents, and views (tx-client's non-LLM sessions), counted apart.
    pub agents: usize,
    pub views: usize,
    /// Ages are drawn against this, epoch seconds.
    pub now: f64,
}

impl Context {
    pub fn shows(&self, item: Item) -> bool {
        item.needs().iter().all(|id| self.active.contains(*id))
            && (!matches!(item, Item::Fork | Item::Artifacts) || self.pane_session)
    }
}

/// What the menu runs once its popup has closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    NewSession,
    Attach,
    Fork,
    Timeline,
    Artifacts,
    Tutor,
    Assistant,
    /// Resume the past session with this id.
    Resume(String),
}

impl Action {
    /// One line, the choice file's format.
    pub fn encode(&self) -> String {
        match self {
            Action::NewSession => "new".into(),
            Action::Attach => "attach".into(),
            Action::Fork => "fork".into(),
            Action::Timeline => "timeline".into(),
            Action::Artifacts => "artifacts".into(),
            Action::Tutor => "tutor".into(),
            Action::Assistant => "assistant".into(),
            Action::Resume(id) => format!("resume {id}"),
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        Some(match text.trim() {
            "new" => Action::NewSession,
            "attach" => Action::Attach,
            "fork" => Action::Fork,
            "timeline" => Action::Timeline,
            "artifacts" => Action::Artifacts,
            "tutor" => Action::Tutor,
            "assistant" => Action::Assistant,
            other => {
                let id = other.strip_prefix("resume ")?.trim();
                if id.is_empty() {
                    return None;
                }
                Action::Resume(id.to_owned())
            }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Main,
    /// The history list, with its selected row.
    History(usize),
    /// The read-only plugin list, scrolled to this row.
    Plugins(usize),
    /// The cheat sheet, scrolled to this row.
    Keys(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Run(Action),
}

/// How many past sessions the recent box shows (keys `1`..`3`).
pub const RECENT: usize = 3;

pub struct Menu {
    pub ctx: Context,
    items: Vec<Item>,
    sel: usize,
    view: View,
}

impl Menu {
    pub fn new(ctx: Context) -> Self {
        let items = ITEMS.into_iter().filter(|item| ctx.shows(*item)).collect();
        Menu {
            ctx,
            items,
            sel: 0,
            view: View::Main,
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn selected(&self) -> Item {
        self.items[self.sel]
    }

    pub fn view(&self) -> View {
        self.view
    }

    /// The recent box: the newest past sessions, when resuming is available.
    pub fn recent(&self) -> &[Past] {
        if !self.ctx.shows(Item::History) {
            return &[];
        }
        &self.ctx.history[..self.ctx.history.len().min(RECENT)]
    }

    pub fn key(&mut self, code: KeyCode) -> Outcome {
        match self.view {
            View::Main => self.main_key(code),
            View::History(row) => self.history_key(row, code),
            View::Plugins(row) => {
                self.view = View::Plugins(self.scroll(row, self.ctx.plugins.len(), code));
                self.back_on(code)
            }
            View::Keys(row) => {
                self.view = View::Keys(self.scroll(row, self.ctx.keys.len(), code));
                self.back_on(code)
            }
        }
    }

    fn main_key(&mut self, code: KeyCode) -> Outcome {
        match code {
            KeyCode::Esc => Outcome::Close,
            KeyCode::Down | KeyCode::Char('j') => {
                self.sel = (self.sel + 1) % self.items.len();
                Outcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.sel = (self.sel + self.items.len() - 1) % self.items.len();
                Outcome::Stay
            }
            KeyCode::Enter => self.activate(self.selected()),
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                match self.recent().get(index) {
                    Some(past) => Outcome::Run(Action::Resume(past.id.clone())),
                    None => Outcome::Stay,
                }
            }
            KeyCode::Char(c) => match self.items.iter().position(|item| item.key() == c) {
                Some(index) => {
                    self.sel = index;
                    self.activate(self.items[index])
                }
                None => Outcome::Stay,
            },
            _ => Outcome::Stay,
        }
    }

    fn activate(&mut self, item: Item) -> Outcome {
        let action = match item {
            Item::Quit => return Outcome::Close,
            Item::History => {
                self.view = View::History(0);
                return Outcome::Stay;
            }
            Item::Plugins => {
                self.view = View::Plugins(0);
                return Outcome::Stay;
            }
            Item::Keys => {
                self.view = View::Keys(0);
                return Outcome::Stay;
            }
            Item::NewSession => Action::NewSession,
            Item::Attach => Action::Attach,
            Item::Fork => Action::Fork,
            Item::Timeline => Action::Timeline,
            Item::Artifacts => Action::Artifacts,
            Item::Tutor => Action::Tutor,
            Item::Assistant => Action::Assistant,
        };
        Outcome::Run(action)
    }

    fn history_key(&mut self, row: usize, code: KeyCode) -> Outcome {
        if code == KeyCode::Enter {
            return match self.ctx.history.get(row) {
                Some(past) => Outcome::Run(Action::Resume(past.id.clone())),
                None => Outcome::Stay,
            };
        }
        self.view = View::History(self.scroll(row, self.ctx.history.len(), code));
        self.back_on(code)
    }

    fn scroll(&self, row: usize, len: usize, code: KeyCode) -> usize {
        let last = len.saturating_sub(1);
        match code {
            KeyCode::Down | KeyCode::Char('j') => (row + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => row.saturating_sub(1),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => last,
            _ => row,
        }
    }

    /// Esc, `q`, Backspace and `h` leave a sub-list for the main menu.
    fn back_on(&mut self, code: KeyCode) -> Outcome {
        if matches!(
            code,
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q' | 'h')
        ) {
            self.view = View::Main;
        }
        Outcome::Stay
    }
}

/// How long a menu opened by the first attach acts only on navigation, Enter and close: keys
/// typed for the IDE while the client attached must not pick items.
pub const GRACE: Duration = Duration::from_millis(400);

/// The key the menu acts on, if any. Ctrl and Alt chords (the prefix and the IDE's root keys)
/// never reach it; during the grace period only arrows, `j`/`k`, Enter, Esc and `q` do.
pub fn accept(code: KeyCode, modifiers: KeyModifiers, in_grace: bool) -> Option<KeyCode> {
    if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return None;
    }
    let navigation = matches!(
        code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Char('j' | 'k' | 'q')
    );
    (!in_grace || navigation).then_some(code)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn all_active() -> BTreeSet<String> {
        [
            "spawn",
            "picker",
            "history",
            "chat",
            "timeline",
            "artifacts",
            "tutor",
            "assistant",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    pub(crate) fn past(id: &str, name: &str, tag: &str, at: f64) -> Past {
        Past {
            id: id.into(),
            name: name.into(),
            tag: tag.into(),
            at,
            chats: 1,
            cwd: "/w".into(),
        }
    }

    pub(crate) fn context() -> Context {
        Context {
            active: all_active(),
            plugins: vec![("spawn".into(), true), ("kill".into(), false)],
            pane_session: true,
            history: vec![
                past("id-1", "mockup-t5-verbs", "tx-mockup", 1000.0 - 660.0),
                past("id-2", "fixer", "agent", 1000.0 - 3420.0),
                past("id-3", "tx-demo-ai", "tx-demo", 1000.0 - 3600.0),
                past("id-4", "old", "", 0.0),
            ],
            keys: vec![KeyHint {
                plugin: "kill".into(),
                keys: vec!["X".into()],
                text: "stop".into(),
            }],
            agents: 2,
            views: 1,
            now: 1000.0,
        }
    }

    #[test]
    fn every_item_shows_with_every_plugin_active() {
        let menu = Menu::new(context());
        assert_eq!(menu.items(), ITEMS);
    }

    #[test]
    fn a_disabled_plugin_hides_its_items() {
        let mut ctx = context();
        ctx.active.remove("chat");
        ctx.active.remove("timeline");
        let menu = Menu::new(ctx);
        for hidden in [Item::History, Item::Fork, Item::Timeline] {
            assert!(!menu.items().contains(&hidden), "{hidden:?}");
        }
        assert!(menu.recent().is_empty(), "no resume, no recent box");
        assert!(menu.items().contains(&Item::NewSession));
        assert!(menu.items().contains(&Item::Plugins));
    }

    #[test]
    fn fork_and_artifacts_need_a_tx_session_in_the_launching_pane() {
        let mut ctx = context();
        ctx.pane_session = false;
        let menu = Menu::new(ctx);
        assert!(!menu.items().contains(&Item::Fork));
        assert!(!menu.items().contains(&Item::Artifacts));
    }

    #[test]
    fn with_nothing_active_the_core_items_remain() {
        let ctx = Context {
            active: BTreeSet::new(),
            ..context()
        };
        assert_eq!(
            Menu::new(ctx).items(),
            [Item::Plugins, Item::Keys, Item::Quit]
        );
    }

    #[test]
    fn item_keys_are_unique_and_never_movement_keys() {
        let keys: BTreeSet<char> = ITEMS.iter().map(|item| item.key()).collect();
        assert_eq!(keys.len(), ITEMS.len());
        assert!(!keys.contains(&'j') && !keys.contains(&'k'));
    }

    #[test]
    fn j_k_and_arrows_move_and_wrap() {
        let mut menu = Menu::new(context());
        assert_eq!(menu.selected(), Item::NewSession);
        assert_eq!(menu.key(KeyCode::Char('j')), Outcome::Stay);
        assert_eq!(menu.selected(), Item::Attach);
        menu.key(KeyCode::Down);
        assert_eq!(menu.selected(), Item::History);
        menu.key(KeyCode::Up);
        menu.key(KeyCode::Char('k'));
        assert_eq!(menu.selected(), Item::NewSession);
        menu.key(KeyCode::Up);
        assert_eq!(menu.selected(), Item::Quit, "wraps");
    }

    #[test]
    fn enter_runs_the_selected_item() {
        let mut menu = Menu::new(context());
        menu.key(KeyCode::Down);
        assert_eq!(menu.key(KeyCode::Enter), Outcome::Run(Action::Attach));
    }

    #[test]
    fn item_keys_run_their_action() {
        for (key, action) in [
            ('n', Action::NewSession),
            ('t', Action::Attach),
            ('f', Action::Fork),
            ('T', Action::Timeline),
            ('a', Action::Artifacts),
            ('?', Action::Tutor),
            ('/', Action::Assistant),
        ] {
            let mut menu = Menu::new(context());
            assert_eq!(menu.key(KeyCode::Char(key)), Outcome::Run(action), "{key}");
        }
    }

    #[test]
    fn a_hidden_items_key_does_nothing() {
        let mut ctx = context();
        ctx.active.remove("timeline");
        let mut menu = Menu::new(ctx);
        assert_eq!(menu.key(KeyCode::Char('T')), Outcome::Stay);
        assert_eq!(menu.view(), View::Main);
    }

    #[test]
    fn q_and_esc_close() {
        assert_eq!(Menu::new(context()).key(KeyCode::Char('q')), Outcome::Close);
        assert_eq!(Menu::new(context()).key(KeyCode::Esc), Outcome::Close);
        let mut menu = Menu::new(context());
        menu.key(KeyCode::Up);
        assert_eq!(menu.key(KeyCode::Enter), Outcome::Close, "Enter on Quit");
    }

    #[test]
    fn digits_resume_the_recent_sessions() {
        let mut menu = Menu::new(context());
        assert_eq!(menu.recent().len(), RECENT);
        assert_eq!(
            menu.key(KeyCode::Char('1')),
            Outcome::Run(Action::Resume("id-1".into()))
        );
        assert_eq!(
            menu.key(KeyCode::Char('3')),
            Outcome::Run(Action::Resume("id-3".into()))
        );
        assert_eq!(menu.key(KeyCode::Char('4')), Outcome::Stay, "not recent");
    }

    #[test]
    fn history_lists_and_enter_resumes_the_row() {
        let mut menu = Menu::new(context());
        assert_eq!(menu.key(KeyCode::Char('h')), Outcome::Stay);
        assert_eq!(menu.view(), View::History(0));
        menu.key(KeyCode::Char('j'));
        menu.key(KeyCode::Char('j'));
        menu.key(KeyCode::Char('j'));
        menu.key(KeyCode::Char('j'));
        assert_eq!(menu.view(), View::History(3), "stops at the last row");
        assert_eq!(
            menu.key(KeyCode::Enter),
            Outcome::Run(Action::Resume("id-4".into()))
        );
        menu.key(KeyCode::Esc);
        assert_eq!(menu.view(), View::Main);
    }

    #[test]
    fn empty_history_enter_stays() {
        let mut ctx = context();
        ctx.history.clear();
        let mut menu = Menu::new(ctx);
        menu.key(KeyCode::Char('h'));
        assert_eq!(menu.key(KeyCode::Enter), Outcome::Stay);
        assert_eq!(menu.view(), View::History(0));
    }

    #[test]
    fn plugins_and_keys_are_read_only_lists() {
        let mut menu = Menu::new(context());
        menu.key(KeyCode::Char('p'));
        assert_eq!(menu.view(), View::Plugins(0));
        assert_eq!(menu.key(KeyCode::Enter), Outcome::Stay);
        menu.key(KeyCode::Char('j'));
        assert_eq!(menu.view(), View::Plugins(1));
        menu.key(KeyCode::Char('q'));
        assert_eq!(menu.view(), View::Main, "q leaves the list, not the menu");
        menu.key(KeyCode::Char('K'));
        assert_eq!(menu.view(), View::Keys(0));
        menu.key(KeyCode::Backspace);
        assert_eq!(menu.view(), View::Main);
    }

    #[test]
    fn item_keys_mirror_the_ide_prefix_keys() {
        for (item, key) in [
            (Item::NewSession, 'n'),
            (Item::Attach, 't'),
            (Item::Fork, 'f'),
            (Item::Timeline, 'T'),
            (Item::Artifacts, 'a'),
            (Item::Assistant, '/'),
            (Item::History, 'h'),
            (Item::Plugins, 'p'),
            (Item::Keys, 'K'),
            (Item::Tutor, '?'),
            (Item::Quit, 'q'),
        ] {
            assert_eq!(item.key(), key, "{item:?}");
        }
    }

    #[test]
    fn ctrl_and_alt_chords_never_reach_the_menu() {
        let none = KeyModifiers::NONE;
        assert_eq!(
            accept(KeyCode::Char('a'), KeyModifiers::CONTROL, false),
            None
        );
        assert_eq!(accept(KeyCode::Char('n'), KeyModifiers::ALT, false), None);
        assert_eq!(accept(KeyCode::Down, KeyModifiers::CONTROL, false), None);
        assert_eq!(
            accept(KeyCode::Char('n'), none, false),
            Some(KeyCode::Char('n'))
        );
        assert_eq!(
            accept(KeyCode::Char('K'), KeyModifiers::SHIFT, false),
            Some(KeyCode::Char('K'))
        );
    }

    #[test]
    fn the_grace_period_takes_only_navigation_enter_and_close() {
        let none = KeyModifiers::NONE;
        for code in [
            KeyCode::Char('n'),
            KeyCode::Char('T'),
            KeyCode::Char('1'),
            KeyCode::Char('h'),
            KeyCode::Char('/'),
        ] {
            assert_eq!(accept(code, none, true), None, "{code:?}");
        }
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Char('q'),
        ] {
            assert_eq!(accept(code, none, true), Some(code), "{code:?}");
        }
    }

    #[test]
    fn actions_round_trip_through_the_choice_line() {
        for action in [
            Action::NewSession,
            Action::Attach,
            Action::Fork,
            Action::Timeline,
            Action::Artifacts,
            Action::Tutor,
            Action::Assistant,
            Action::Resume("0f3a".into()),
        ] {
            assert_eq!(Action::decode(&action.encode()), Some(action.clone()));
        }
        assert_eq!(Action::decode(""), None);
        assert_eq!(Action::decode("resume "), None);
        assert_eq!(Action::decode("bogus"), None);
    }
}
