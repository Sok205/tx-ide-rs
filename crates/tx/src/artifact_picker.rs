//! The prefix+a picker: a list in a popup over the pane, drawn like the tx picker's rows.
//!
//! Pure list logic ([`Picker`]) — keys in, the chosen action out — around the session editor's
//! terminal layer. Enter attaches the cursor's row in the pane and a digit attaches that row;
//! f (focus the pane already showing it) and p (open it in a popup) arm the operation instead,
//! and the next digit or Enter applies it to that row — so `f 3` focuses the third row, as does
//! `f ↓ ↓ Enter`. Rows past the ninth take choose-tree's keys: `M-a`…`M-z`, then `M-A`…`M-Z`.
//! Esc disarms, then closes (q too). A pick the caller declines (a focus with nothing to focus)
//! becomes a notice line and the list stays open.

use std::io;

use crate::palette::{ACCENT_ANSI, BOLD, DIM_FG_ANSI, RESET, SELECTION_BG, WARN_ANSI};
use crate::session_editor::{Decoder, Key, Terminal, read_key, write_all};

pub const HINT: &str = "Enter/key attach here   f focus its pane   p popup   Esc close";

/// The key that picks row `index` (0-based), as choose-tree labels them: `1`…`9`, then `M-a`…
/// `M-z`, then `M-A`…`M-Z`; `None` past those.
pub fn row_key(index: usize) -> Option<Key> {
    match index {
        0..=8 => Some(Key::Char((b'1' + index as u8) as char)),
        9..=34 => Some(Key::Alt((b'a' + (index - 9) as u8) as char)),
        35..=60 => Some(Key::Alt((b'A' + (index - 35) as u8) as char)),
        _ => None,
    }
}

/// The row a key picks: the inverse of [`row_key`].
fn row_index(key: Key) -> Option<usize> {
    match key {
        Key::Char(digit @ '1'..='9') => Some(digit as usize - '1' as usize),
        Key::Alt(letter @ 'a'..='z') => Some(9 + letter as usize - 'a' as usize),
        Key::Alt(letter @ 'A'..='Z') => Some(35 + letter as usize - 'A' as usize),
        _ => None,
    }
}

/// `row_key`'s label for the row's left edge: `(1)`, `(M-a)`.
pub fn row_label(index: usize) -> String {
    match row_key(index) {
        Some(Key::Char(digit)) => format!("({digit})"),
        Some(Key::Alt(letter)) => format!("(M-{letter})"),
        _ => String::new(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Attach(usize),
    Focus(usize),
    /// Open the row's view nest-attached in a popup over the terminal.
    Popup(usize),
}

/// What f / p arm: the operation the next digit or Enter applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Attach,
    Focus,
    Popup,
}

impl Operation {
    fn pick(self, index: usize) -> Pick {
        match self {
            Self::Attach => Pick::Attach(index),
            Self::Focus => Pick::Focus(index),
            Self::Popup => Pick::Popup(index),
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Attach => HINT,
            Self::Focus => "focus mode: digit or Enter picks the row   Esc back",
            Self::Popup => "popup mode: digit or Enter picks the row   Esc back",
        }
    }
}

/// One line of the list: a section title (not selectable) or a fully styled row (SGR, no
/// background). Rows are numbered in order across sections.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Section(String),
    Row(String),
}

impl Item {
    pub fn text(&self) -> &str {
        match self {
            Self::Section(text) | Self::Row(text) => text,
        }
    }
}

/// The list state: `items` in display order, `header` the dim column line above them, the
/// cursor counted over the rows only.
pub struct Picker<'a> {
    header: &'a str,
    items: &'a [Item],
    rows: usize,
    cursor: usize,
    /// The operation f / p armed; Enter and digits (rows are numbered from 1) apply it.
    /// `Attach` when nothing is armed.
    operation: Operation,
    /// A one-line message under the rows (a declined pick); the next key clears it.
    notice: Option<String>,
}

impl<'a> Picker<'a> {
    pub fn new(header: &'a str, items: &'a [Item]) -> Self {
        Self {
            header,
            items,
            rows: items
                .iter()
                .filter(|item| matches!(item, Item::Row(_)))
                .count(),
            cursor: 0,
            operation: Operation::Attach,
            notice: None,
        }
    }

    pub fn notice(&mut self, text: String) {
        self.notice = Some(text);
    }

    /// `Some(Some(pick))` chooses, `Some(None)` cancels, `None` keeps going.
    pub fn handle(&mut self, key: Key) -> Option<Option<Pick>> {
        self.notice = None;
        match key {
            Key::Up | Key::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            Key::Down | Key::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.rows.saturating_sub(1));
            }
            Key::Enter | Key::Char('\r') | Key::Char('\n') => {
                return Some(self.row().map(|index| self.disarm().pick(index)));
            }
            Key::Char('f') => self.operation = Operation::Focus,
            Key::Char('p') => self.operation = Operation::Popup,
            Key::Char('1'..='9') | Key::Alt(_) => {
                if let Some(index) = row_index(key).filter(|index| *index < self.rows) {
                    return Some(Some(self.disarm().pick(index)));
                }
            }
            Key::Char('\x1b') if self.operation != Operation::Attach => {
                self.disarm();
            }
            Key::Char('\x1b' | 'q' | '\x03') => return Some(None),
            _ => {}
        }
        None
    }

    fn row(&self) -> Option<usize> {
        (self.rows > 0).then_some(self.cursor)
    }

    /// Back to plain attach; the operation that was armed.
    fn disarm(&mut self) -> Operation {
        std::mem::replace(&mut self.operation, Operation::Attach)
    }

    /// The screen: header, sections and rows (the cursor's row on the selection background), the
    /// hint last.
    pub fn render(&self, height: usize, width: usize) -> String {
        let mut out = String::from("\x1b[0m\x1b[H\x1b[2J");
        let mut screen_row = 0;
        let mut put = |text: &str| {
            if screen_row < height {
                out += &format!("\x1b[{};1H{text}{RESET}", screen_row + 1);
            }
            screen_row += 1;
        };
        put(&format!("{DIM_FG_ANSI}{}", self.header));
        let mut row = 0;
        for item in self.items {
            match item {
                Item::Section(title) => put(&format!("{ACCENT_ANSI}{BOLD}{title}")),
                Item::Row(line) if row == self.cursor => {
                    let fill = width.saturating_sub(visible_width(line));
                    put(&format!(
                        "\x1b[48;5;{SELECTION_BG}m{line}{}",
                        " ".repeat(fill)
                    ));
                    row += 1;
                }
                Item::Row(line) => {
                    put(line);
                    row += 1;
                }
            }
        }
        if height > self.items.len() + 3 {
            if let Some(notice) = &self.notice {
                out += &format!("\x1b[{};1H{WARN_ANSI}{notice}{RESET}", height - 1);
            }
            out += &format!(
                "\x1b[{};1H{DIM_FG_ANSI}{}{RESET}",
                height,
                self.operation.hint()
            );
        }
        out
    }
}

/// Columns a styled line occupies: SGR sequences (`ESC [ … m`) take none.
pub fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\x1b' && characters.peek() == Some(&'[') {
            for inner in characters.by_ref() {
                if inner == 'm' {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }
    width
}

/// Run the picker on the controlling terminal; `None` on Esc / q / Ctrl-C. `decline` vets a pick:
/// `Some(message)` shows it as the notice and keeps the list open, `None` accepts the pick.
pub fn pick(
    header: &str,
    items: &[Item],
    mut decline: impl FnMut(Pick) -> Option<String>,
) -> io::Result<Option<Pick>> {
    let _terminal = Terminal::open()?;
    let mut picker = Picker::new(header, items);
    let mut decoder = Decoder::default();
    loop {
        let (height, width) = Terminal::size();
        write_all(picker.render(height, width).as_bytes())?;
        let key = read_key(&mut decoder, (height, width))?;
        match picker.handle(key) {
            Some(Some(chosen)) => match decline(chosen) {
                Some(message) => picker.notice(message),
                None => return Ok(Some(chosen)),
            },
            Some(None) => return Ok(None),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_move_choose_and_cancel() {
        let items = vec![Item::Row("a".to_owned()), Item::Row("b".to_owned())];
        let mut picker = Picker::new("H", &items);
        assert_eq!(picker.handle(Key::Down), None);
        assert_eq!(picker.handle(Key::Enter), Some(Some(Pick::Attach(1))));
        assert_eq!(picker.handle(Key::Char('1')), Some(Some(Pick::Attach(0))));
        assert_eq!(picker.handle(Key::Char('0')), None);
        assert_eq!(picker.handle(Key::Char('7')), None);
        assert_eq!(picker.handle(Key::Right), None);
        assert_eq!(picker.handle(Key::Char('l')), None);
        assert_eq!(picker.handle(Key::Char('q')), Some(None));
    }

    #[test]
    fn f_and_p_arm_the_operation_for_the_next_digit_or_enter() {
        let items = vec![
            Item::Section("X".to_owned()),
            Item::Row("a".to_owned()),
            Item::Section("Y".to_owned()),
            Item::Row("b".to_owned()),
            Item::Row("c".to_owned()),
        ];
        let mut picker = Picker::new("H", &items);
        assert_eq!(picker.handle(Key::Char('f')), None);
        assert!(picker.render(10, 40).contains("focus mode:"));
        assert_eq!(picker.handle(Key::Char('3')), Some(Some(Pick::Focus(2))));
        // Applied once: the next Enter is a plain attach again.
        assert_eq!(picker.handle(Key::Enter), Some(Some(Pick::Attach(0))));
        assert_eq!(picker.handle(Key::Char('p')), None);
        assert_eq!(picker.handle(Key::Down), None);
        assert_eq!(picker.handle(Key::Down), None);
        assert_eq!(picker.handle(Key::Enter), Some(Some(Pick::Popup(2))));
        // Esc disarms first, then closes.
        assert_eq!(picker.handle(Key::Char('f')), None);
        assert_eq!(picker.handle(Key::Char('\x1b')), None);
        assert!(picker.render(10, 40).contains(HINT));
        assert_eq!(picker.handle(Key::Char('\x1b')), Some(None));
    }

    #[test]
    fn rows_past_nine_take_meta_letters() {
        assert_eq!(row_key(0), Some(Key::Char('1')));
        assert_eq!(row_key(8), Some(Key::Char('9')));
        assert_eq!(row_key(9), Some(Key::Alt('a')));
        assert_eq!(row_key(34), Some(Key::Alt('z')));
        assert_eq!(row_key(35), Some(Key::Alt('A')));
        assert_eq!(row_key(60), Some(Key::Alt('Z')));
        assert_eq!(row_key(61), None);
        for index in 0..61 {
            assert_eq!(row_index(row_key(index).unwrap()), Some(index));
        }
        assert_eq!(row_label(9), "(M-a)");
        let items: Vec<Item> = (0..12).map(|i| Item::Row(i.to_string())).collect();
        let mut picker = Picker::new("H", &items);
        assert_eq!(picker.handle(Key::Alt('c')), Some(Some(Pick::Attach(11))));
        assert_eq!(picker.handle(Key::Alt('d')), None);
        assert_eq!(picker.handle(Key::Char('f')), None);
        assert_eq!(picker.handle(Key::Alt('a')), Some(Some(Pick::Focus(9))));
    }

    #[test]
    fn notice_shows_under_the_rows_until_the_next_key() {
        let items = vec![Item::Row("a".to_owned())];
        let mut picker = Picker::new("H", &items);
        picker.notice("nope".into());
        assert!(picker.render(6, 20).contains("nope"));
        picker.handle(Key::Down);
        assert!(!picker.render(6, 20).contains("nope"));
    }

    #[test]
    fn empty_list_chooses_nothing() {
        let items = vec![Item::Section("X".to_owned())];
        let mut picker = Picker::new("H", &items);
        assert_eq!(picker.handle(Key::Enter), Some(None));
    }

    #[test]
    fn visible_width_skips_sgr() {
        assert_eq!(visible_width("\x1b[38;5;1m[x]\x1b[39m y"), 5);
    }
}
