//! The `tx attach` picker's vim-style normal mode: the keys it claims, its footer, and the fzf
//! actions `tx _picker-key` hands back for the keys that need state (Esc, Enter, the digits, and
//! the `result` event that finishes a number jump).
//!
//! Normal mode hides fzf's input, so printable keys stop editing the query and the bound ones
//! (`j`, `k`, the digits, ...) fire instead; `/` or `i` shows the input and unbinds them. A digit
//! buffer lives in the file `$TX_DIGITS_FILE`: a number commits as soon as no further digit could
//! extend it to a row, or on Enter. Committing with a query set clears it first and finishes on
//! the next `result` event, because fzf's `pos(N)` counts the rows of the filtered list and the
//! re-match after `clear-query` is asynchronous.

use crate::palette::{ACCENT_ANSI, BOLD, RESET};

/// Env var naming the picker's digit-buffer file: the pending row number, or the number of a
/// jump waiting for its `result` event. The picker's auto-reload pauses while it is non-empty.
pub const DIGITS_FILE_ENV: &str = "TX_DIGITS_FILE";

/// The keys bound only in normal mode; entering search mode unbinds them so they type.
pub const NORMAL_KEYS: &str = "j,k,g,G,ctrl-d,ctrl-u,/,i,d,t,r,q,0,1,2,3,4,5,6,7,8,9";

/// What fzf reports when a key handler runs (`$FZF_INPUT_STATE`, `$FZF_QUERY`,
/// `$FZF_MATCH_COUNT`, `$FZF_TOTAL_COUNT`), plus the digit buffer.
pub struct KeyState<'a> {
    pub searching: bool,
    pub query: &'a str,
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

pub fn normal_footer(query: &str, digits: &str) -> String {
    let query = if query.is_empty() {
        String::new()
    } else {
        format!("  /{query}")
    };
    let digits = if digits.is_empty() {
        String::new()
    } else {
        format!("  {ACCENT_ANSI}{BOLD}{digits}_{RESET}")
    };
    format!(
        "{ACCENT_ANSI}{BOLD} NORMAL{RESET}  j/k move · 1-9 open · / search · d kill · t tags · q quit{query}{digits}"
    )
}

pub fn search_footer() -> String {
    format!("{ACCENT_ANSI}{BOLD} SEARCH{RESET}  esc normal mode · enter open")
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
    let footer = |digits: &str| format!("change-footer:{}", normal_footer(state.query, digits));
    let reply = match key {
        "esc" if state.searching => {
            cleared(format!("hide-input+rebind({NORMAL_KEYS})+{}", footer("")))
        }
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
            // With the input hidden a query change starts no search; `search()` runs it.
            actions: "rebind(result)+clear-query+search()".to_owned(),
        }
    }
}
