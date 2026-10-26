//! The `tx attach` picker's vim-style normal mode and the stateful key handler behind
//! `tx _picker-key` (Esc, Enter, the row-number digits).

use crate::palette::{ACCENT_ANSI, BOLD, RESET};

/// The digit buffer; the picker's auto-reload pauses while it is non-empty.
pub const DIGITS_FILE_ENV: &str = "TX_DIGITS_FILE";

pub const NORMAL_PROMPT: &str = "  ❯ ";
pub const SEARCH_PROMPT: &str = "  / ";

const ACTION_KEYS: [&str; 24] = [
    "j", "k", "g", "G", "ctrl-d", "ctrl-u", "/", "i", "d", "t", "r", "q", "y", "n", "0", "1", "2",
    "3", "4", "5", "6", "7", "8", "9",
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
    pub digits: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub digits: String,
    pub actions: String,
}

pub fn normal_footer(digits: &str) -> String {
    let digits = if digits.is_empty() {
        String::new()
    } else {
        format!("  {ACCENT_ANSI}{BOLD}{digits}_{RESET}")
    };
    format!(
        "{ACCENT_ANSI}{BOLD} NORMAL{RESET}  j/k move · 1-9 open · i or / search · d kill · t tags · q quit{digits}"
    )
}

pub fn search_footer() -> String {
    format!("{ACCENT_ANSI}{BOLD} SEARCH{RESET}  esc normal mode · enter open")
}

fn enter_normal() -> String {
    format!(
        "change-prompt({NORMAL_PROMPT})+{}+change-footer:{}",
        toggle_normal_keys("rebind"),
        normal_footer("")
    )
}

pub fn press(key: &str, state: &KeyState<'_>) -> Option<Reply> {
    let cleared = |actions: String| Reply {
        digits: String::new(),
        actions,
    };
    let footer = |digits: &str| format!("change-footer:{}", normal_footer(digits));
    let reply = match key {
        "esc" if state.searching => cleared(enter_normal()),
        "esc" if !state.digits.is_empty() => cleared(footer("")),
        "esc" => cleared("abort".to_owned()),
        "enter" if state.digits.is_empty() => cleared("accept".to_owned()),
        "enter" => cleared(format!("pos({})+accept", state.digits)),
        digit if digit.len() == 1 && digit.bytes().all(|byte| byte.is_ascii_digit()) => {
            let digits = format!("{}{digit}", state.digits);
            let number: usize = digits.parse().ok()?;
            if number == 0 || number > state.total {
                cleared(footer(""))
            } else if number * 10 > state.total {
                cleared(format!("pos({number})+accept"))
            } else {
                Reply {
                    actions: footer(&digits),
                    digits,
                }
            }
        }
        _ => return None,
    };
    Some(reply)
}
