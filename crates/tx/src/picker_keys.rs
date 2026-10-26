//! The `tx attach` picker's vim-style normal mode: the keys it claims, its prompt and footer, and
//! the fzf actions `tx _picker-key` hands back for the keys that need state (Esc, Enter, the
//! digits, and the `result` event that finishes a number jump).
//!
//! The input line stays visible in both modes. Normal mode binds its own keys (`j`, `k`, the
//! digits, ...) and every other printable key to `ignore`, so nothing types into the query; `i`
//! or `/` unbinds them all and the query takes keys again. The prompt tells the modes apart, for
//! the user and for the Esc handler. A digit buffer lives in the file `$TX_DIGITS_FILE`: a number
//! commits as soon as no further digit could extend it to a row, or on Enter. Committing while a
//! query filters the list clears it first and finishes on the next `result` event, because fzf's
//! `pos(N)` counts the rows of the filtered list and the re-match after `clear-query` is
//! asynchronous.

use crate::palette::{ACCENT_ANSI, BOLD, RESET};

/// Env var naming the picker's digit-buffer file: the pending row number, or the number of a
/// jump waiting for its `result` event. The picker's auto-reload pauses while it is non-empty.
pub const DIGITS_FILE_ENV: &str = "TX_DIGITS_FILE";

pub const NORMAL_PROMPT: &str = "  ❯ ";
pub const SEARCH_PROMPT: &str = "  / ";

/// The keys normal mode binds to its own actions.
const ACTION_KEYS: [&str; 24] = [
    "j", "k", "g", "G", "ctrl-d", "ctrl-u", "/", "i", "d", "t", "r", "q", "y", "n", "0", "1", "2",
    "3", "4", "5", "6", "7", "8", "9",
];

/// The printable keys normal mode swallows (bound to `ignore`).
pub fn ignored_keys() -> Vec<String> {
    (b'!'..=b'~')
        .map(|byte| char::from(byte).to_string())
        .filter(|key| !ACTION_KEYS.contains(&key.as_str()))
        .chain(["space".to_owned()])
        .collect()
}

/// `unbind` or `rebind` of every normal-mode key. `(` and `)` go in a `[...]` argument, since
/// they would end a `(...)` one.
pub fn toggle_normal_keys(action: &str) -> String {
    let (parens, rest): (Vec<String>, Vec<String>) = ACTION_KEYS
        .iter()
        .map(ToString::to_string)
        .chain(ignored_keys())
        .partition(|key| key == "(" || key == ")");
    format!("{action}({})+{action}[{}]", rest.join(","), parens.join(","))
}

/// What fzf reports when a key handler runs (`$FZF_PROMPT`, `$FZF_MATCH_COUNT`,
/// `$FZF_TOTAL_COUNT`), plus the digit buffer.
pub struct KeyState<'a> {
    pub searching: bool,
    pub matched: usize,
    pub total: usize,
    pub digits: &'a str,
}

impl KeyState<'_> {
    /// Every row matches, so `pos(N)` is row N of the feed. Read from the counts, not the
    /// query: `$FZF_QUERY` is stale in the `result` handler that follows a `search()`.
    fn unfiltered(&self) -> bool {
        self.matched == self.total
    }
}

/// The reply to one key: the digit buffer to store, and the fzf actions to run.
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

/// The actions that leave search mode, keeping the query.
fn enter_normal() -> String {
    format!(
        "change-prompt({NORMAL_PROMPT})+{}+change-footer:{}",
        toggle_normal_keys("rebind"),
        normal_footer("")
    )
}

/// The fzf actions for `key` (`esc`, `enter`, `result`, or a digit); `None` for any other key.
pub fn press(key: &str, state: &KeyState<'_>) -> Option<Reply> {
    let keep = |actions: String| Reply {
        digits: state.digits.to_owned(),
        actions,
    };
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
        "enter" => commit(state.digits.parse().ok()?, state),
        "result" if state.unfiltered() && !state.digits.is_empty() => {
            cleared(format!("unbind(result)+pos({})+accept", state.digits))
        }
        "result" => keep(String::new()),
        digit if digit.len() == 1 && digit.bytes().all(|byte| byte.is_ascii_digit()) => {
            let digits = format!("{}{digit}", state.digits);
            let number: usize = digits.parse().ok()?;
            if number == 0 || number > state.total {
                cleared(footer(""))
            } else if number * 10 > state.total {
                commit(number, state)
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

/// Select row `number` of the full list: directly when nothing filters it, else clear the query
/// and let the `result` handler finish once fzf has re-matched.
fn commit(number: usize, state: &KeyState<'_>) -> Reply {
    if state.unfiltered() {
        Reply {
            digits: String::new(),
            actions: format!("pos({number})+accept"),
        }
    } else {
        Reply {
            digits: number.to_string(),
            actions: "rebind(result)+clear-query+search()".to_owned(),
        }
    }
}
