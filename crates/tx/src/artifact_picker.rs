//! The prefix+a picker: a list in a popup over the pane, drawn like the tx picker's rows.
//!
//! Pure list logic ([`Picker`]) — keys in, the chosen action out — around the session editor's
//! terminal layer. Enter attaches the cursor's row in the pane and a digit attaches that row;
//! f (focus the pane already showing it) and p (open it in a popup) arm the operation instead,
//! and the next digit or Enter applies it to that row — so `f 3` focuses the third row, as does
//! `f ↓ ↓ Enter`. Esc disarms, then closes (q too). A pick the caller declines (a focus with
//! nothing to focus) becomes a notice line and the list stays open.

use std::io;

use crate::palette::{DIM_FG_ANSI, RESET, SELECTION_BG, WARN_ANSI};
use crate::session_editor::{Decoder, Key, Terminal, read_key, write_all};

pub const HINT: &str = "Enter/digit attach here   f focus its pane   p popup   Esc close";

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

/// The list state: `lines` are fully styled rows (SGR, no background), `header` the dim column
/// line above them.
pub struct Picker<'a> {
    header: &'a str,
    lines: &'a [String],
    cursor: usize,
    /// The operation f / p armed; Enter and digits (rows are numbered from 1) apply it.
    /// `Attach` when nothing is armed.
    operation: Operation,
    /// A one-line message under the rows (a declined pick); the next key clears it.
    notice: Option<String>,
}

impl<'a> Picker<'a> {
    pub fn new(header: &'a str, lines: &'a [String]) -> Self {
        Self {
            header,
            lines,
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
                self.cursor = (self.cursor + 1).min(self.lines.len().saturating_sub(1));
            }
            Key::Enter | Key::Char('\r') | Key::Char('\n') => {
                return Some(self.row().map(|index| self.disarm().pick(index)));
            }
            Key::Char('f') => self.operation = Operation::Focus,
            Key::Char('p') => self.operation = Operation::Popup,
            Key::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                if index < self.lines.len() {
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
        (!self.lines.is_empty()).then_some(self.cursor)
    }

    /// Back to plain attach; the operation that was armed.
    fn disarm(&mut self) -> Operation {
        std::mem::replace(&mut self.operation, Operation::Attach)
    }

    /// The screen: header, rows (the cursor's on the selection background), the hint last.
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
        for (index, line) in self.lines.iter().enumerate() {
            if index == self.cursor {
                let fill = width.saturating_sub(visible_width(line));
                put(&format!(
                    "\x1b[48;5;{SELECTION_BG}m{line}{}",
                    " ".repeat(fill)
                ));
            } else {
                put(line);
            }
        }
        if height > self.lines.len() + 3 {
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
    lines: &[String],
    mut decline: impl FnMut(Pick) -> Option<String>,
) -> io::Result<Option<Pick>> {
    let _terminal = Terminal::open()?;
    let mut picker = Picker::new(header, lines);
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
        let lines = vec!["a".to_owned(), "b".to_owned()];
        let mut picker = Picker::new("H", &lines);
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
        let lines = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let mut picker = Picker::new("H", &lines);
        assert_eq!(picker.handle(Key::Char('f')), None);
        assert!(picker.render(8, 40).contains("focus mode:"));
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
        assert!(picker.render(8, 40).contains(HINT));
        assert_eq!(picker.handle(Key::Char('\x1b')), Some(None));
    }

    #[test]
    fn notice_shows_under_the_rows_until_the_next_key() {
        let lines = vec!["a".to_owned()];
        let mut picker = Picker::new("H", &lines);
        picker.notice("nope".into());
        assert!(picker.render(6, 20).contains("nope"));
        picker.handle(Key::Down);
        assert!(!picker.render(6, 20).contains("nope"));
    }

    #[test]
    fn empty_list_chooses_nothing() {
        let mut picker = Picker::new("H", &[]);
        assert_eq!(picker.handle(Key::Enter), Some(None));
    }

    #[test]
    fn visible_width_skips_sgr() {
        assert_eq!(visible_width("\x1b[38;5;1m[x]\x1b[39m y"), 5);
    }
}
