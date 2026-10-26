//! The `tx attach` picker's vim-style normal mode and the stateful key handler behind
//! `tx _picker-key` (Esc, Enter, p, the row-number digits).

use crate::palette::{ACCENT_ANSI, BOLD, RESET};

/// The pending pick: `p` while popup mode is armed, then the digit buffer. The picker's
/// auto-reload pauses while it is non-empty.
pub const DIGITS_FILE_ENV: &str = "TX_DIGITS_FILE";
/// The marker popup mode keeps ahead of the digits in the pending-pick file.
pub const POPUP_MARK: &str = "p";
/// What a popup pick has fzf print ahead of the selection.
pub const POPUP_LINE: &str = "popup";

pub const NORMAL_PROMPT: &str = "  ❯ ";
pub const SEARCH_PROMPT: &str = "  / ";

const ACTION_KEYS: [&str; 25] = [
    "j", "k", "g", "G", "ctrl-d", "ctrl-u", "/", "i", "d", "t", "r", "q", "y", "n", "p", "0", "1",
    "2", "3", "4", "5", "6", "7", "8", "9",
];

pub fn ignored_keys() -> Vec<String> {
    (b'!'..=b'~')
        .map(|byte| char::from(byte).to_string())
        .filter(|key| !ACTION_KEYS.contains(&key.as_str()))
        .chain(["space".to_owned()])
        .collect()
}

/// `(` and `)` go in a `[...]` argument: they would end a `(...)` one.
pub fn toggle_normal_keys(action: &str) -> String {
    let (parens, rest): (Vec<String>, Vec<String>) = ACTION_KEYS
        .iter()
        .map(ToString::to_string)
        .chain(ignored_keys())
        .partition(|key| key == "(" || key == ")");
    format!("{action}({})+{action}[{}]", rest.join(","), parens.join(","))
}

pub struct KeyState<'a> {
    pub searching: bool,
    pub total: usize,
    pub popup: bool,
    pub digits: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub popup: bool,
    pub digits: String,
    pub actions: String,
}

impl Reply {
    /// The pending-pick file's new content.
    pub fn pending(&self) -> String {
        let mark = if self.popup { POPUP_MARK } else { "" };
        format!("{mark}{}", self.digits)
    }
}

pub fn normal_footer(popup: bool, digits: &str) -> String {
    let digits = if digits.is_empty() {
        String::new()
    } else {
        format!("  {ACCENT_ANSI}{BOLD}{digits}_{RESET}")
    };
    if popup {
        format!(
            "{ACCENT_ANSI}{BOLD} POPUP{RESET}  j/k move · digit or enter opens the row in a popup · esc back{digits}"
        )
    } else {
        format!(
            "{ACCENT_ANSI}{BOLD} NORMAL{RESET}  j/k move · 1-9 open · p popup mode · i or / search · d kill · t tags · q quit{digits}"
        )
    }
}

pub fn search_footer() -> String {
    format!("{ACCENT_ANSI}{BOLD} SEARCH{RESET}  esc normal mode · enter open")
}

fn enter_normal() -> String {
    format!(
        "change-prompt({NORMAL_PROMPT})+{}+change-footer:{}",
        toggle_normal_keys("rebind"),
        normal_footer(false, "")
    )
}

pub fn press(key: &str, state: &KeyState<'_>) -> Option<Reply> {
    let cleared = |actions: String| Reply {
        popup: false,
        digits: String::new(),
        actions,
    };
    // Popup mode stays armed until a pick or esc.
    let pending = |digits: String| Reply {
        popup: state.popup,
        actions: format!("change-footer:{}", normal_footer(state.popup, &digits)),
        digits,
    };
    let footer = |popup: bool| format!("change-footer:{}", normal_footer(popup, ""));
    let accept = if state.popup {
        format!("print({POPUP_LINE})+accept")
    } else {
        "accept".to_owned()
    };
    let reply = match key {
        "esc" if state.searching => cleared(enter_normal()),
        "esc" if state.popup || !state.digits.is_empty() => cleared(footer(false)),
        "esc" => cleared("abort".to_owned()),
        "p" => Reply {
            popup: true,
            digits: String::new(),
            actions: footer(true),
        },
        "enter" if state.digits.is_empty() => cleared(accept),
        "enter" => cleared(format!("pos({})+{accept}", state.digits)),
        digit if digit.len() == 1 && digit.bytes().all(|byte| byte.is_ascii_digit()) => {
            let digits = format!("{}{digit}", state.digits);
            let number: usize = digits.parse().ok()?;
            if number == 0 || number > state.total {
                pending(String::new())
            } else if number * 10 > state.total {
                cleared(format!("pos({number})+{accept}"))
            } else {
                pending(digits)
            }
        }
        _ => return None,
    };
    Some(reply)
}
