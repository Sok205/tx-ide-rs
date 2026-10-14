//! The prefix+a picker: a list in a popup over the pane, drawn like the tx picker's rows.
//!
//! Pure list logic ([`Picker`]) — keys in, the chosen action out — around the session editor's
//! terminal layer. Enter attaches the row's view in the pane, → focuses the pane already showing
//! it, a digit picks that row for attach, Esc / q close.

use std::io;

use crate::palette::{DIM_FG_ANSI, RESET, SELECTION_BG};
use crate::session_editor::{Decoder, Key, Terminal, read_key, write_all};

pub const HINT: &str = "Enter attach here   → focus its pane   Esc close";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Attach(usize),
    Focus(usize),
}

/// The list state: `lines` are fully styled rows (SGR, no background), `header` the dim column
/// line above them.
pub struct Picker<'a> {
    header: &'a str,
    lines: &'a [String],
    cursor: usize,
}

impl<'a> Picker<'a> {
    pub fn new(header: &'a str, lines: &'a [String]) -> Self {
        Self {
            header,
            lines,
            cursor: 0,
        }
    }

    /// `Some(Some(pick))` chooses, `Some(None)` cancels, `None` keeps going.
    pub fn handle(&mut self, key: Key) -> Option<Option<Pick>> {
        match key {
            Key::Up | Key::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            Key::Down | Key::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.lines.len().saturating_sub(1));
            }
            Key::Enter | Key::Char('\r') | Key::Char('\n') => {
                return Some(self.row().map(Pick::Attach));
            }
            Key::Right => return Some(self.row().map(Pick::Focus)),
            Key::Char(digit @ '0'..='9') => {
                let index = digit as usize - '0' as usize;
                if index < self.lines.len() {
                    return Some(Some(Pick::Attach(index)));
                }
            }
            Key::Char('\x1b' | 'q' | '\x03') => return Some(None),
            _ => {}
        }
        None
    }

    fn row(&self) -> Option<usize> {
        (!self.lines.is_empty()).then_some(self.cursor)
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
        if height > self.lines.len() + 2 {
            out += &format!("\x1b[{};1H{DIM_FG_ANSI}{HINT}{RESET}", height);
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

/// Run the picker on the controlling terminal; `None` on Esc / q / Ctrl-C.
pub fn pick(header: &str, lines: &[String]) -> io::Result<Option<Pick>> {
    let _terminal = Terminal::open()?;
    let mut picker = Picker::new(header, lines);
    let mut decoder = Decoder::default();
    loop {
        let (height, width) = Terminal::size();
        write_all(picker.render(height, width).as_bytes())?;
        let key = read_key(&mut decoder, (height, width))?;
        if let Some(outcome) = picker.handle(key) {
            return Ok(outcome);
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
        assert_eq!(picker.handle(Key::Right), Some(Some(Pick::Focus(1))));
        assert_eq!(picker.handle(Key::Char('0')), Some(Some(Pick::Attach(0))));
        assert_eq!(picker.handle(Key::Char('7')), None);
        assert_eq!(picker.handle(Key::Char('q')), Some(None));
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
