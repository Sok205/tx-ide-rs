//! Board state and input, pure: keys in, state and requests out; [`super::ui`] draws it and
//! [`super::run`] carries the requests out.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tx_client::show::Show;

use super::model::{Board, Card, ColumnKind};

/// What a key asked for; the event loop does it and clears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// `s`: a developer agent on the issue, tagged `issue-<n>`.
    StartAgent(u64),
    /// Enter / a / p: show the agent working on the card.
    ShowAgent(Show, String),
    /// `o`.
    Browser(u64),
    /// `c`: `gh issue comment`, on the terminal.
    Comment(u64),
    /// `n`: `gh issue create`, on the terminal.
    NewIssue,
    /// `x` on an open issue.
    Close(u64),
    /// `x` on a closed issue.
    Reopen(u64),
    /// `m`: assign yourself, or unassign if you are.
    ToggleAssignee(u64),
    /// `r`.
    Refresh,
}

pub struct App {
    pub board: Board,
    /// The login `gh` is signed in as; `m` toggles it.
    pub viewer: Option<String>,
    pub column: usize,
    pub cursor: [usize; 3],
    pub scroll: [usize; 3],
    pub detail_scroll: u16,
    pub help: bool,
    pub quit: bool,
    pub request: Option<Request>,
    /// A one-shot line for the footer; the next key clears it.
    pub notice: Option<String>,
    /// A fetch is in flight.
    pub refreshing: bool,
    /// Epoch seconds of the last successful fetch.
    pub fetched_at: Option<f64>,
    pub now: f64,
}

impl App {
    pub fn new(board: Board, now: f64) -> Self {
        let mut app = Self {
            board,
            viewer: None,
            column: 0,
            cursor: [0; 3],
            scroll: [0; 3],
            detail_scroll: 0,
            help: false,
            quit: false,
            request: None,
            notice: None,
            refreshing: false,
            fetched_at: None,
            now,
        };
        app.column = app
            .board
            .columns
            .iter()
            .position(|column| !column.cards.is_empty())
            .unwrap_or(0);
        app
    }

    pub fn column_of(&self, kind: ColumnKind) -> &[Card] {
        &self.board.columns[kind as usize].cards
    }

    pub fn selected(&self) -> Option<&Card> {
        self.board.columns[self.column]
            .cards
            .get(self.cursor[self.column])
    }

    /// A new board: the selected issue stays selected wherever it moved to.
    pub fn refresh(&mut self, board: Board, now: f64) {
        let selected = self.selected().map(|card| card.issue.number);
        self.board = board;
        self.now = now;
        if let Some(number) = selected
            && let Some((column, row)) = self.find(number)
        {
            self.column = column;
            self.cursor[column] = row;
        }
        self.clamp();
    }

    fn find(&self, number: u64) -> Option<(usize, usize)> {
        self.board
            .columns
            .iter()
            .enumerate()
            .find_map(|(column, cards)| {
                cards
                    .cards
                    .iter()
                    .position(|card| card.issue.number == number)
                    .map(|row| (column, row))
            })
    }

    fn clamp(&mut self) {
        for (column, cards) in self.board.columns.iter().enumerate() {
            self.cursor[column] = self.cursor[column].min(cards.cards.len().saturating_sub(1));
        }
    }

    fn move_cursor(&mut self, delta: i64) {
        let cards = self.board.columns[self.column].cards.len();
        if cards == 0 {
            return;
        }
        let row = (self.cursor[self.column] as i64 + delta).clamp(0, cards as i64 - 1);
        self.cursor[self.column] = row as usize;
        self.detail_scroll = 0;
    }

    fn move_column(&mut self, delta: i64) {
        self.column = (self.column as i64 + delta).clamp(0, 2) as usize;
        self.detail_scroll = 0;
    }

    /// Keeps the cursor of the current column inside a window of `visible` cards.
    pub fn keep_cursor_visible(&mut self, visible: usize) {
        let visible = visible.max(1);
        let column = self.column;
        let cursor = self.cursor[column];
        if cursor < self.scroll[column] {
            self.scroll[column] = cursor;
        } else if cursor >= self.scroll[column] + visible {
            self.scroll[column] = cursor + 1 - visible;
        }
    }

    fn card_request(&self, make: impl FnOnce(&Card) -> Option<Request>) -> Option<Request> {
        self.selected().and_then(make)
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        self.notice = None;
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if control => self.quit = true,
            KeyCode::Esc => self.help = false,
            KeyCode::Char('?') => self.help = !self.help,
            KeyCode::Char('h') | KeyCode::Left => self.move_column(-1),
            KeyCode::Char('l') | KeyCode::Right => self.move_column(1),
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('g') => self.move_cursor(i64::MIN / 2),
            KeyCode::Char('G') => self.move_cursor(i64::MAX / 2),
            KeyCode::Char('J') | KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(3);
            }
            KeyCode::Char('K') | KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(3);
            }
            KeyCode::Char('r') => self.request = Some(Request::Refresh),
            KeyCode::Char('n') => self.request = Some(Request::NewIssue),
            KeyCode::Char('s') => {
                self.request =
                    self.card_request(|card| Some(Request::StartAgent(card.issue.number)));
            }
            KeyCode::Char('o') => {
                self.request = self.card_request(|card| Some(Request::Browser(card.issue.number)));
            }
            KeyCode::Char('c') => {
                self.request = self.card_request(|card| Some(Request::Comment(card.issue.number)));
            }
            KeyCode::Char('m') => {
                self.request =
                    self.card_request(|card| Some(Request::ToggleAssignee(card.issue.number)));
            }
            KeyCode::Char('x') => {
                self.request = self.card_request(|card| {
                    Some(if card.issue.is_open() {
                        Request::Close(card.issue.number)
                    } else {
                        Request::Reopen(card.issue.number)
                    })
                });
            }
            KeyCode::Enter => self.show(Show::Focus),
            KeyCode::Char('a') => self.show(Show::AttachTo),
            KeyCode::Char('p') => self.show(Show::Popup),
            _ => {}
        }
    }

    fn show(&mut self, show: Show) {
        let Some(card) = self.selected() else {
            return;
        };
        match card.live_agent() {
            Some(agent) => self.request = Some(Request::ShowAgent(show, agent.name.clone())),
            None => {
                self.notice = Some(format!(
                    "#{}: no agent running on it (s starts one)",
                    card.issue.number
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::{Issue, Repository};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn board() -> Board {
        let issue = |number: u64, open: bool| Issue {
            number,
            state: if open { "OPEN" } else { "CLOSED" }.to_owned(),
            ..Issue::default()
        };
        Board::build(
            Repository::parse("o/n").unwrap(),
            vec![issue(1, true), issue(2, true), issue(3, false)],
            &[],
        )
    }

    #[test]
    fn keys_move_and_request() {
        let mut app = App::new(board(), 0.0);
        assert_eq!(app.selected().unwrap().issue.number, 1);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected().unwrap().issue.number, 2);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected().unwrap().issue.number, 2);
        app.handle_key(key(KeyCode::Char('x')));
        assert_eq!(app.request, Some(Request::Close(2)));
        app.handle_key(key(KeyCode::Char('l')));
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.column, 2);
        app.handle_key(key(KeyCode::Char('x')));
        assert_eq!(app.request, Some(Request::Reopen(3)));
        app.handle_key(key(KeyCode::Enter));
        assert!(app.notice.as_deref().unwrap().starts_with("#3: no agent"));
        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.quit);
    }

    #[test]
    fn a_refresh_follows_the_selected_issue() {
        let mut app = App::new(board(), 0.0);
        app.handle_key(key(KeyCode::Char('j')));
        let mut moved = board();
        let card = moved.columns[0].cards.remove(1);
        moved.columns[1].cards.push(card);
        app.refresh(moved, 1.0);
        assert_eq!((app.column, app.cursor[1]), (1, 0));
        assert_eq!(app.selected().unwrap().issue.number, 2);
    }

    #[test]
    fn the_first_column_with_cards_starts_selected() {
        let mut empty_backlog = board();
        empty_backlog.columns[0].cards.clear();
        assert_eq!(App::new(empty_backlog, 0.0).column, 2);
        assert_eq!(
            App::new(Board::empty(Repository::parse("o/n").unwrap()), 0.0).column,
            0
        );
    }
}
