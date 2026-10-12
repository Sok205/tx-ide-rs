//! Port of lib/tx/session_editor.py — the terminal form of single-line fields behind
//! `tx _edit-session` (name + tags) and `tx _new-agent` (name + tags + prompt).
//!
//! The reference is a `curses.wrapper` form in raw mode with keypad on and a 25 ms escape delay.
//! Here the form logic ([`Form`]) is pure — keys in, a [`Frame`] of positioned text out — and
//! [`run_form`] is the terminal shell around it: termios raw mode on stdin, the alternate
//! screen and keypad mode on stdout, and a small keypad/UTF-8 decoder for the input bytes.

use std::io::{self, Write};
use std::os::fd::RawFd;

use crate::palette::{ACCENT_ANSI, BOLD, DIM_FG_ANSI, FG_ANSI, SELECTION_BG, WARN_ANSI};

const TAGS_HELP: &str = "Comma-separated · empty clears tags";
const TOO_SMALL: &str = "Resize to edit";
const ESC_DELAY_MS: i32 = 25;
const RESIZE_POLL_MS: i32 = 100;

/// A decoded key (the `get_wch` result: a character, or a keypad key code).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Delete,
    BackTab,
    /// Keypad Enter (`KEY_ENTER`).
    Enter,
    /// `KEY_RESIZE`.
    Resize,
    /// Any other keypad sequence: a no-op key that still clears the error row.
    Other,
}

/// How the form finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome<const N: usize> {
    Cancel,
    /// The values as typed, in field order — not stripped.
    Save([String; N]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Inactive field: the palette's foreground.
    Normal,
    /// Active label: the palette accent, bold.
    Accent,
    /// Inactive label, help and hint rows: the palette's dim foreground.
    Muted,
    /// Active field: the palette's foreground on the selection background.
    Selected,
    /// Error row: the palette's warning amber.
    Warning,
}

impl Style {
    fn sgr(self) -> String {
        match self {
            Style::Normal => FG_ANSI.into(),
            Style::Accent => format!("{ACCENT_ANSI}{BOLD}"),
            Style::Muted => DIM_FG_ANSI.into(),
            Style::Selected => format!("{FG_ANSI}\x1b[48;5;{SELECTION_BG}m"),
            Style::Warning => WARN_ANSI.into(),
        }
    }
}

/// One `addstr` onto the erased screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draw {
    pub row: usize,
    pub col: usize,
    pub text: String,
    pub style: Style,
}

/// A full repaint: the draws in order and the cursor position (`screen.move`), if any.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub draws: Vec<Draw>,
    pub cursor: Option<(usize, usize)>,
}

impl Frame {
    fn add(&mut self, row: usize, col: usize, text: impl Into<String>, style: Style) {
        self.draws.push(Draw {
            row,
            col,
            text: text.into(),
            style,
        });
    }

    /// `addnstr`: at most `n` characters.
    fn addn(&mut self, row: usize, col: usize, text: &str, n: usize, style: Style) {
        self.add(row, col, text.chars().take(n).collect::<String>(), style);
    }

    /// The bytes that paint this frame on an erased screen.
    pub fn to_ansi(&self) -> String {
        let mut out = String::from("\x1b[0m\x1b[H\x1b[2J");
        for draw in &self.draws {
            out += &format!(
                "\x1b[{};{}H{}{}\x1b[0m",
                draw.row + 1,
                draw.col + 1,
                draw.style.sgr(),
                draw.text
            );
        }
        if let Some((row, col)) = self.cursor {
            out += &format!("\x1b[{};{}H", row + 1, col + 1);
        }
        out
    }

    /// The screen text as `capture-pane` shows it: one line per row, trailing blanks trimmed.
    pub fn rows(&self, height: usize, width: usize) -> Vec<String> {
        let mut grid = vec![vec![Some(' '); width]; height];
        for draw in &self.draws {
            let mut col = draw.col;
            for character in draw.text.chars() {
                let cells = char_width(character);
                if draw.row >= height || col + cells.max(1) > width {
                    break;
                }
                if cells == 0 {
                    continue;
                }
                grid[draw.row][col] = Some(character);
                for filler in 1..cells {
                    grid[draw.row][col + filler] = None;
                }
                col += cells;
            }
        }
        grid.into_iter()
            .map(|row| {
                row.into_iter()
                    .flatten()
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }
}

/// `unicodedata.combining(c)` → 0 columns, East Asian Wide / Fullwidth → 2, else 1. The tables
/// cover the common ranges (no Unicode database is available here).
fn char_width(character: char) -> usize {
    const COMBINING: &[(u32, u32)] = &[
        (0x0300, 0x036F),
        (0x0483, 0x0487),
        (0x0591, 0x05BD),
        (0x05BF, 0x05BF),
        (0x05C1, 0x05C2),
        (0x05C4, 0x05C5),
        (0x05C7, 0x05C7),
        (0x0610, 0x061A),
        (0x064B, 0x065F),
        (0x0670, 0x0670),
        (0x06D6, 0x06DC),
        (0x06DF, 0x06E4),
        (0x06E7, 0x06E8),
        (0x06EA, 0x06ED),
        (0x093C, 0x093C),
        (0x094D, 0x094D),
        (0x0E38, 0x0E3A),
        (0x0E48, 0x0E4B),
        (0x1AB0, 0x1ABD),
        (0x1DC0, 0x1DFF),
        (0x20D0, 0x20DC),
        (0x20E1, 0x20E1),
        (0x20E5, 0x20F0),
        (0x302A, 0x302F),
        (0x3099, 0x309A),
        (0xFE20, 0xFE2F),
    ];
    const WIDE: &[(u32, u32)] = &[
        (0x1100, 0x115F),
        (0x231A, 0x231B),
        (0x2329, 0x232A),
        (0x23E9, 0x23EC),
        (0x23F0, 0x23F0),
        (0x23F3, 0x23F3),
        (0x25FD, 0x25FE),
        (0x2614, 0x2615),
        (0x2648, 0x2653),
        (0x267F, 0x267F),
        (0x2693, 0x2693),
        (0x26A1, 0x26A1),
        (0x26AA, 0x26AB),
        (0x26BD, 0x26BE),
        (0x26C4, 0x26C5),
        (0x26CE, 0x26CE),
        (0x26D4, 0x26D4),
        (0x26EA, 0x26EA),
        (0x26F2, 0x26F3),
        (0x26F5, 0x26F5),
        (0x26FA, 0x26FA),
        (0x26FD, 0x26FD),
        (0x2705, 0x2705),
        (0x270A, 0x270B),
        (0x2728, 0x2728),
        (0x274C, 0x274C),
        (0x274E, 0x274E),
        (0x2753, 0x2755),
        (0x2757, 0x2757),
        (0x2795, 0x2797),
        (0x27B0, 0x27B0),
        (0x27BF, 0x27BF),
        (0x2B1B, 0x2B1C),
        (0x2B50, 0x2B50),
        (0x2B55, 0x2B55),
        (0x2E80, 0x303E),
        (0x3041, 0x33FF),
        (0x3400, 0x4DBF),
        (0x4E00, 0x9FFF),
        (0xA000, 0xA4CF),
        (0xA960, 0xA97F),
        (0xAC00, 0xD7A3),
        (0xF900, 0xFAFF),
        (0xFE10, 0xFE19),
        (0xFE30, 0xFE6F),
        (0xFF00, 0xFF60),
        (0xFFE0, 0xFFE6),
        (0x16FE0, 0x16FE4),
        (0x17000, 0x18CFF),
        (0x1B000, 0x1B2FF),
        (0x1F004, 0x1F004),
        (0x1F0CF, 0x1F0CF),
        (0x1F18E, 0x1F18E),
        (0x1F191, 0x1F19A),
        (0x1F200, 0x1F2FF),
        (0x1F300, 0x1F320),
        (0x1F32D, 0x1F335),
        (0x1F337, 0x1F37C),
        (0x1F37E, 0x1F393),
        (0x1F3A0, 0x1F3CA),
        (0x1F3CF, 0x1F3D3),
        (0x1F3E0, 0x1F3F0),
        (0x1F3F4, 0x1F3F4),
        (0x1F3F8, 0x1F43E),
        (0x1F440, 0x1F440),
        (0x1F442, 0x1F4FC),
        (0x1F4FF, 0x1F53D),
        (0x1F54B, 0x1F54E),
        (0x1F550, 0x1F567),
        (0x1F57A, 0x1F57A),
        (0x1F595, 0x1F596),
        (0x1F5A4, 0x1F5A4),
        (0x1F5FB, 0x1F64F),
        (0x1F680, 0x1F6C5),
        (0x1F6CC, 0x1F6CC),
        (0x1F6D0, 0x1F6D2),
        (0x1F6D5, 0x1F6D7),
        (0x1F6DC, 0x1F6DF),
        (0x1F6EB, 0x1F6EC),
        (0x1F6F4, 0x1F6FC),
        (0x1F7E0, 0x1F7EB),
        (0x1F7F0, 0x1F7F0),
        (0x1F90C, 0x1F93A),
        (0x1F93C, 0x1F945),
        (0x1F947, 0x1F9FF),
        (0x1FA70, 0x1FAFF),
        (0x20000, 0x2FFFD),
        (0x30000, 0x3FFFD),
    ];
    let code = u32::from(character);
    let within = |table: &[(u32, u32)]| {
        table
            .iter()
            .any(|&(low, high)| (low..=high).contains(&code))
    };
    if within(COMBINING) {
        0
    } else if within(WIDE) {
        2
    } else {
        1
    }
}

fn text_width(text: &[char]) -> usize {
    text.iter().copied().map(char_width).sum()
}

/// `str.isprintable()` for one character: not a control, format, separator (except space) or
/// private-use code point. Unassigned code points are not detected.
fn is_printable(character: char) -> bool {
    if character == ' ' {
        return true;
    }
    if character.is_control() || character.is_whitespace() {
        return false;
    }
    let code = u32::from(character);
    let format_or_private = [
        (0x00AD, 0x00AD),
        (0x0600, 0x0605),
        (0x061C, 0x061C),
        (0x06DD, 0x06DD),
        (0x070F, 0x070F),
        (0x0890, 0x0891),
        (0x08E2, 0x08E2),
        (0x180E, 0x180E),
        (0x200B, 0x200F),
        (0x202A, 0x202E),
        (0x2060, 0x2064),
        (0x2066, 0x206F),
        (0xE000, 0xF8FF),
        (0xFEFF, 0xFEFF),
        (0xFFF9, 0xFFFB),
        (0x110BD, 0x110BD),
        (0x1BCA0, 0x1BCA3),
        (0x1D173, 0x1D17A),
        (0xE0001, 0xE0001),
        (0xE0020, 0xE007F),
        (0xF0000, 0x10FFFF),
    ];
    !format_or_private
        .iter()
        .any(|&(low, high)| (low..=high).contains(&code))
}

/// One single-line field of a [`Form`].
#[derive(Clone, Copy, Debug)]
pub struct Field<'a> {
    pub label: &'static str,
    /// A muted line under the field.
    pub help: Option<&'static str>,
    /// Enter refuses to finish while this field is blank.
    pub required: bool,
    /// The starting text.
    pub value: &'a str,
}

/// A field being edited.
#[derive(Clone, Debug)]
struct Input {
    label: &'static str,
    help: Option<&'static str>,
    required: bool,
    value: Vec<char>,
    position: usize,
    offset: usize,
}

/// The form state (`values`, `positions`, `offsets`, `active`, `error` of `_form`). Each field
/// takes two rows — the field, then its help line — and the hint row closes the form.
#[derive(Clone, Debug)]
pub struct Form<const N: usize> {
    inputs: [Input; N],
    /// What Enter does, for the hint row.
    action: &'static str,
    active: usize,
    /// Enter found the active field required and blank; cleared by the next key.
    error: bool,
}

impl<const N: usize> Form<N> {
    pub fn new(fields: [Field; N], action: &'static str) -> Self {
        let inputs = fields.map(|field| {
            let value: Vec<char> = field.value.chars().collect();
            Input {
                label: field.label,
                help: field.help,
                required: field.required,
                position: value.len(),
                value,
                offset: 0,
            }
        });
        Self {
            inputs,
            action,
            active: 0,
            error: false,
        }
    }

    /// The column of the fields' leading pad cell: two past the widest `› Label:`.
    fn field_col(&self) -> usize {
        let widest = self
            .inputs
            .iter()
            .map(|input| input.label.chars().count())
            .max()
            .unwrap_or(0);
        widest + 7
    }

    fn too_small(&self, height: usize, width: usize) -> bool {
        height < N * 2 + 3 || width < self.field_col() + 13
    }

    /// Paint the form for a `height`×`width` screen (also scrolls each field's offset so its
    /// cursor stays visible, as the reference does while drawing).
    pub fn render(&mut self, height: usize, width: usize) -> Frame {
        let mut frame = Frame::default();
        if self.too_small(height, width) {
            frame.addn(0, 0, TOO_SMALL, width.saturating_sub(1), Style::Normal);
            // curses leaves the cursor after the last write.
            let written = TOO_SMALL.chars().count().min(width.saturating_sub(1));
            frame.cursor = Some((0, written));
            return frame;
        }
        let field_col = self.field_col();
        let available = width - field_col - 3;
        for (index, input) in self.inputs.iter_mut().enumerate() {
            let row = index * 2 + 1;
            input.offset = input.offset.min(input.position);
            while text_width(&input.value[input.offset..input.position]) >= available {
                input.offset += 1;
            }
            let mut visible: Vec<char> = Vec::new();
            let mut used = 0;
            for &character in &input.value[input.offset..] {
                if used + char_width(character) > available {
                    break;
                }
                used += char_width(character);
                visible.push(character);
            }
            let active = index == self.active;
            let marker = if active { '›' } else { ' ' };
            let label_style = if active { Style::Accent } else { Style::Muted };
            frame.add(row, 2, format!("{marker} {}:", input.label), label_style);
            let field_style = if active {
                Style::Selected
            } else {
                Style::Normal
            };
            frame.add(row, field_col, " ".repeat(available + 2), field_style);
            frame.add(
                row,
                field_col + 1,
                visible.iter().collect::<String>(),
                field_style,
            );
            if let Some(help) = input.help {
                frame.addn(row + 1, field_col + 1, help, available, Style::Muted);
            }
        }
        let active = &self.inputs[self.active];
        let (message, style) = if self.error {
            (format!("{} cannot be empty.", active.label), Style::Warning)
        } else {
            let action = self.action;
            (
                format!("↑↓ switch   Enter {action}   Esc cancel"),
                Style::Muted,
            )
        };
        frame.addn(N * 2 + 2, 2, &message, width - 4, style);
        let cursor_col = field_col + 1 + text_width(&active.value[active.offset..active.position]);
        frame.cursor = Some((self.active * 2 + 1, cursor_col));
        frame
    }

    /// Apply one key read while the screen was `height`×`width` (the size of the last paint).
    /// `Some` when the form is done.
    pub fn handle(&mut self, key: Key, height: usize, width: usize) -> Option<Outcome<N>> {
        if matches!(key, Key::Char('\x1b' | '\x03')) {
            return Some(Outcome::Cancel);
        }
        if self.too_small(height, width) {
            return None;
        }
        if matches!(key, Key::Char('\n' | '\r') | Key::Enter) {
            let blank = self
                .inputs
                .iter()
                .position(|input| input.required && input.value.iter().all(|c| c.is_whitespace()));
            if let Some(blank) = blank {
                self.error = true;
                self.active = blank;
                return None;
            }
            let values = self
                .inputs
                .each_ref()
                .map(|input| input.value.iter().collect());
            return Some(Outcome::Save(values));
        }
        self.error = false;
        match key {
            Key::Down | Key::Char('\t') => {
                self.active = (self.active + 1) % N;
                return None;
            }
            Key::Up | Key::BackTab => {
                self.active = (self.active + N - 1) % N;
                return None;
            }
            _ => {}
        }
        let input = &mut self.inputs[self.active];
        let position = input.position;
        let value = &mut input.value;
        match key {
            Key::Left | Key::Char('\x02') => input.position = position.saturating_sub(1),
            Key::Right | Key::Char('\x06') => {
                input.position = value.len().min(position + 1);
            }
            Key::Home | Key::Char('\x01') => input.position = 0,
            Key::End | Key::Char('\x05') => input.position = value.len(),
            Key::Char('\x7f' | '\x08') if position > 0 => {
                value.remove(position - 1);
                input.position -= 1;
            }
            Key::Delete => {
                if position < value.len() {
                    value.remove(position);
                }
            }
            Key::Char('\x15') => {
                value.drain(..position);
                input.position = 0;
            }
            Key::Char('\x0b') => value.truncate(position),
            Key::Char(character) if is_printable(character) => {
                value.insert(position, character);
                input.position += 1;
            }
            _ => {}
        }
        None
    }
}

/// Splits input bytes into keys: keypad sequences (CSI / SS3), UTF-8 characters, lone bytes.
#[derive(Default)]
struct Decoder {
    pending: Vec<u8>,
}

enum Decoded {
    Key(Key, usize),
    /// The buffer holds a prefix of a longer sequence.
    Incomplete,
}

impl Decoder {
    fn decode(bytes: &[u8]) -> Decoded {
        let Some(&first) = bytes.first() else {
            return Decoded::Incomplete;
        };
        if first == 0x1b {
            return match bytes.get(1) {
                None => Decoded::Incomplete,
                Some(b'[') => Self::csi(bytes),
                Some(b'O') => match bytes.get(2) {
                    None => Decoded::Incomplete,
                    Some(&final_byte) => {
                        let key = match final_byte {
                            b'A' => Key::Up,
                            b'B' => Key::Down,
                            b'C' => Key::Right,
                            b'D' => Key::Left,
                            b'H' => Key::Home,
                            b'F' => Key::End,
                            b'M' => Key::Enter,
                            _ => Key::Other,
                        };
                        Decoded::Key(key, 3)
                    }
                },
                // Not a keypad sequence: curses hands back the ESC on its own.
                Some(_) => Decoded::Key(Key::Char('\x1b'), 1),
            };
        }
        let len = match first {
            0x00..=0x7f => 1,
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => return Decoded::Key(Key::Other, 1),
        };
        if bytes.len() < len {
            return Decoded::Incomplete;
        }
        match std::str::from_utf8(&bytes[..len])
            .ok()
            .and_then(|text| text.chars().next())
        {
            Some(character) => Decoded::Key(Key::Char(character), len),
            None => Decoded::Key(Key::Other, 1),
        }
    }

    fn csi(bytes: &[u8]) -> Decoded {
        let Some(end) = bytes[2..].iter().position(|b| (0x40..=0x7e).contains(b)) else {
            return Decoded::Incomplete;
        };
        let params = &bytes[2..2 + end];
        let key = match (params, bytes[2 + end]) {
            (b"", b'A') => Key::Up,
            (b"", b'B') => Key::Down,
            (b"", b'C') => Key::Right,
            (b"", b'D') => Key::Left,
            (b"", b'H') | (b"1" | b"7", b'~') => Key::Home,
            (b"", b'F') | (b"4" | b"8", b'~') => Key::End,
            (b"3", b'~') => Key::Delete,
            (b"", b'Z') => Key::BackTab,
            _ => Key::Other,
        };
        Decoded::Key(key, 3 + end)
    }

    /// The next complete key, if the buffer holds one. With `flush`, an incomplete escape prefix
    /// is given up on (the escape delay expired): the ESC is returned alone.
    fn next(&mut self, flush: bool) -> Option<Key> {
        match Self::decode(&self.pending) {
            Decoded::Key(key, used) => {
                self.pending.drain(..used);
                Some(key)
            }
            Decoded::Incomplete if flush && !self.pending.is_empty() => {
                let key = if self.pending[0] == 0x1b {
                    Key::Char('\x1b')
                } else {
                    Key::Other
                };
                self.pending.remove(0);
                Some(key)
            }
            Decoded::Incomplete => None,
        }
    }
}

const STDIN: RawFd = 0;
const STDOUT: RawFd = 1;

/// Raw mode on stdin + alternate screen / keypad mode on stdout, undone on drop (`endwin`).
struct Terminal {
    saved: libc::termios,
}

impl Terminal {
    fn open() -> io::Result<Self> {
        // SAFETY: `termios` is plain data; tcgetattr fills it or fails.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: valid fd number and a valid pointer to a termios.
        if unsafe { libc::tcgetattr(STDIN, &mut saved) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = saved;
        // SAFETY: `raw` is a valid termios.
        unsafe { libc::cfmakeraw(&mut raw) };
        // SAFETY: valid fd and termios pointer.
        if unsafe { libc::tcsetattr(STDIN, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let terminal = Self { saved };
        write_all(b"\x1b[?1049h\x1b[?1h\x1b=")?;
        Ok(terminal)
    }

    fn size() -> (usize, usize) {
        for fd in [STDOUT, STDIN] {
            // SAFETY: `winsize` is plain data; ioctl fills it or fails.
            let mut size: libc::winsize = unsafe { std::mem::zeroed() };
            // SAFETY: TIOCGWINSZ takes a pointer to a winsize.
            if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut size) } == 0 && size.ws_col > 0 {
                return (usize::from(size.ws_row), usize::from(size.ws_col));
            }
        }
        (24, 80)
    }

    /// Wait up to `timeout_ms` for input; the bytes read (empty on timeout).
    fn read(timeout_ms: i32) -> io::Result<Vec<u8>> {
        let mut poll = libc::pollfd {
            fd: STDIN,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut poll, 1, timeout_ms) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::Interrupted {
                Ok(Vec::new())
            } else {
                Err(error)
            };
        }
        if ready == 0 {
            return Ok(Vec::new());
        }
        let mut buffer = [0u8; 256];
        // SAFETY: the buffer is valid for its length.
        let count = unsafe { libc::read(STDIN, buffer.as_mut_ptr().cast(), buffer.len()) };
        match count {
            n if n > 0 => Ok(buffer[..n as usize].to_vec()),
            0 => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "terminal closed",
            )),
            _ => {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    Ok(Vec::new())
                } else {
                    Err(error)
                }
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = write_all(b"\x1b[0m\x1b[?1l\x1b>\x1b[?1049l");
        // SAFETY: restores the attributes read in `open`.
        unsafe { libc::tcsetattr(STDIN, libc::TCSANOW, &self.saved) };
    }
}

fn write_all(bytes: &[u8]) -> io::Result<()> {
    let mut stdout = io::stdout().lock();
    stdout.write_all(bytes)?;
    stdout.flush()
}

/// The prefix+e form: the session's name (required) and tags.
pub fn session_form(name: &str, tags: &str) -> Form<2> {
    Form::new(
        [
            Field {
                label: "Name",
                help: None,
                required: true,
                value: name,
            },
            Field {
                label: "Tags",
                help: Some(TAGS_HELP),
                required: false,
                value: tags,
            },
        ],
        "save",
    )
}

/// Run the prefix+e form; `Some((name, tags))` on save, `None` on Esc / Ctrl-C.
pub fn edit_session(name: &str, tags: &str) -> io::Result<Option<(String, String)>> {
    Ok(run_form(session_form(name, tags))?.map(|[name, tags]| (name, tags)))
}

/// Run `form` on the controlling terminal (stdin/stdout); the values on Enter, `None` on Esc /
/// Ctrl-C.
pub fn run_form<const N: usize>(mut form: Form<N>) -> io::Result<Option<[String; N]>> {
    let _terminal = Terminal::open()?;
    let mut decoder = Decoder::default();
    loop {
        let (height, width) = Terminal::size();
        write_all(form.render(height, width).to_ansi().as_bytes())?;
        let key = loop {
            if let Some(key) = decoder.next(false) {
                break key;
            }
            let escape_pending = decoder.pending.first() == Some(&0x1b);
            let timeout = if escape_pending {
                ESC_DELAY_MS
            } else {
                RESIZE_POLL_MS
            };
            let bytes = Terminal::read(timeout)?;
            if bytes.is_empty() {
                if escape_pending {
                    if let Some(key) = decoder.next(true) {
                        break key;
                    }
                } else if Terminal::size() != (height, width) {
                    break Key::Resize;
                }
            }
            decoder.pending.extend_from_slice(&bytes);
        };
        match form.handle(key, height, width) {
            Some(Outcome::Cancel) => return Ok(None),
            Some(Outcome::Save(values)) => return Ok(Some(values)),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(form: &mut Form<2>, keys: &[Key]) -> Option<Outcome<2>> {
        keys.iter().find_map(|&key| form.handle(key, 24, 80))
    }

    fn typed(text: &str) -> Vec<Key> {
        text.chars().map(Key::Char).collect()
    }

    #[test]
    fn rendered_rows_match_the_reference_layout() {
        let mut form = session_form("abc", "t");
        let rows = form.render(24, 80).rows(24, 80);
        assert_eq!(rows[1], "  › Name:   abc");
        assert_eq!(rows[3], "    Tags:   t");
        assert_eq!(rows[4], "            Comma-separated · empty clears tags");
        assert_eq!(rows[6], "  ↑↓ switch   Enter save   Esc cancel");
        assert_eq!(form.render(24, 80).cursor, Some((1, 15)));
    }

    #[test]
    fn cancel_keys() {
        for key in [Key::Char('\x1b'), Key::Char('\x03')] {
            let mut form = session_form("w", "a");
            assert_eq!(form.handle(key, 24, 80), Some(Outcome::Cancel));
        }
    }

    #[test]
    fn empty_name_refused_then_any_key_clears_the_error() {
        let mut form = session_form("abc", "t");
        assert_eq!(keys(&mut form, &[Key::Down, Key::Char('\x15')]), None);
        assert_eq!(keys(&mut form, &[Key::Up, Key::Char('\x15')]), None);
        assert_eq!(form.handle(Key::Char('\r'), 24, 80), None);
        assert_eq!(
            form.render(24, 80).rows(24, 80)[6],
            "  Name cannot be empty."
        );
        form.handle(Key::Right, 24, 80);
        assert_eq!(
            form.render(24, 80).rows(24, 80)[6],
            "  ↑↓ switch   Enter save   Esc cancel"
        );
        assert_eq!(
            keys(&mut form, &typed("  x \r")),
            Some(Outcome::Save(["  x ".into(), "".into()]))
        );
    }

    #[test]
    fn editing_keys() {
        let mut form = session_form("abc", "t");
        keys(&mut form, &[Key::Left, Key::Left, Key::Char('\x0b')]);
        assert_eq!(form.render(24, 80).rows(24, 80)[1], "  › Name:   a");

        let mut form = session_form("abc", "t");
        keys(&mut form, &[Key::Home, Key::Delete]);
        assert_eq!(form.render(24, 80).rows(24, 80)[1], "  › Name:   bc");

        let mut form = session_form("abc", "t");
        keys(
            &mut form,
            &[Key::Char('\x01'), Key::Char('\x06'), Key::Char('\x7f')],
        );
        keys(
            &mut form,
            &[Key::Char('\x05'), Key::Char('\x02'), Key::Char('X')],
        );
        assert_eq!(form.render(24, 80).rows(24, 80)[1], "  › Name:   bXc");

        let mut form = session_form("abc", "t");
        keys(
            &mut form,
            &[Key::Home, Key::Char('Y'), Key::Char('\x08'), Key::End],
        );
        assert_eq!(
            keys(&mut form, &typed("X\n")),
            Some(Outcome::Save(["abcX".into(), "t".into()]))
        );

        let mut form = session_form("w", "a");
        let mut sequence = vec![Key::Char('\x15')];
        sequence.extend(typed("w2"));
        sequence.extend([Key::Char('\t'), Key::End]);
        sequence.extend(typed(",b"));
        keys(&mut form, &sequence);
        let rows = form.render(24, 80).rows(24, 80);
        assert_eq!(
            (rows[1].as_str(), rows[3].as_str()),
            ("    Name:   w2", "  › Tags:   a,b")
        );
        assert_eq!(
            form.handle(Key::Enter, 24, 80),
            Some(Outcome::Save(["w2".into(), "a,b".into()]))
        );
    }

    #[test]
    fn too_small_only_cancels() {
        let mut form = session_form("abc", "t");
        let rows = form.render(5, 20).rows(5, 20);
        assert_eq!(rows[0], "Resize to edit");
        assert_eq!(form.handle(Key::Char('\r'), 5, 20), None);
        assert_eq!(form.handle(Key::Char('\x1b'), 5, 20), Some(Outcome::Cancel));
    }

    #[test]
    fn long_value_scrolls_to_keep_the_cursor_visible() {
        let name = "x".repeat(100);
        let mut form = session_form(&name, "");
        let frame = form.render(24, 40);
        // available = 26: the cursor sits at the end, 25 characters shown before it.
        assert_eq!(
            frame.rows(24, 40)[1],
            format!("  › Name:   {}", "x".repeat(25))
        );
        assert_eq!(frame.cursor, Some((1, 37)));
    }

    #[test]
    fn decoder_keypad_and_utf8() {
        let mut decoder = Decoder::default();
        decoder.pending.extend_from_slice(
            "\x1b[A\x1bOB\x1b[1~\x1b[4~\x1b[3~\x1b[Z\x1b[1;5A\x1bOMé\r".as_bytes(),
        );
        let mut seen = Vec::new();
        while let Some(key) = decoder.next(false) {
            seen.push(key);
        }
        assert_eq!(
            seen,
            [
                Key::Up,
                Key::Down,
                Key::Home,
                Key::End,
                Key::Delete,
                Key::BackTab,
                Key::Other,
                Key::Enter,
                Key::Char('é'),
                Key::Char('\r'),
            ]
        );
        decoder.pending.push(0x1b);
        assert_eq!(decoder.next(false), None);
        assert_eq!(decoder.next(true), Some(Key::Char('\x1b')));
    }

    #[test]
    fn widths_and_printability() {
        assert_eq!(text_width(&"日本a\u{301}".chars().collect::<Vec<_>>()), 5);
        assert!(is_printable('é') && is_printable(' ') && is_printable('›'));
        assert!(!is_printable('\x1f') && !is_printable('\u{200b}') && !is_printable('\u{a0}'));
    }
}
