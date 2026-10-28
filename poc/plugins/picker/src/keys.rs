//! The picker's vim-style normal mode and the stateful key handler behind `txc _picker-key`
//! (Esc, Enter, p, the row-number digits). Port of main's `picker_keys.rs`.

use tx_ui::render::{ACCENT_ANSI, BOLD, RESET};

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

/// Every printable key with no normal-mode action: bound to `ignore` so it never types.
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
    format!(
        "{action}({})+{action}[{}]",
        rest.join(","),
        parens.join(",")
    )
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

/// The fzf actions for `key` in `state`; `None` for a key the handler does not own.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn state(total: usize, popup: bool, digits: &str) -> KeyState<'_> {
        KeyState {
            searching: false,
            total,
            popup,
            digits,
        }
    }

    #[test]
    fn digits_open_at_once_when_unambiguous_and_buffer_otherwise() {
        let reply = press("3", &state(5, false, "")).unwrap();
        assert_eq!(reply.actions, "pos(3)+accept");
        assert_eq!(reply.pending(), "");
        let reply = press("1", &state(12, false, "")).unwrap();
        assert_eq!(reply.pending(), "1");
        let reply = press("2", &state(12, false, "1")).unwrap();
        assert_eq!(reply.actions, "pos(12)+accept");
        // out of range: the buffer clears, nothing opens
        let reply = press("7", &state(5, false, "")).unwrap();
        assert_eq!(reply.pending(), "");
        assert!(reply.actions.starts_with("change-footer:"));
    }

    #[test]
    fn popup_mode_arms_until_a_pick_or_esc() {
        let armed = press("p", &state(3, false, "")).unwrap();
        assert_eq!(armed.pending(), "p");
        let reply = press("2", &state(3, true, "")).unwrap();
        assert_eq!(reply.actions, "pos(2)+print(popup)+accept");
        assert_eq!(reply.pending(), "");
        let reply = press("enter", &state(3, true, "")).unwrap();
        assert_eq!(reply.actions, "print(popup)+accept");
        let reply = press("esc", &state(3, true, "")).unwrap();
        assert_eq!(reply.pending(), "");
        assert!(reply.actions.starts_with("change-footer:"));
        assert_eq!(press("esc", &state(3, false, "")).unwrap().actions, "abort");
        assert!(press("x", &state(3, false, "")).is_none());
    }

    #[test]
    fn parens_go_in_the_bracket_argument() {
        let toggled = toggle_normal_keys("unbind");
        assert!(toggled.ends_with("+unbind[(,)]"), "{toggled}");
        assert!(toggled.starts_with("unbind(j,k,"), "{toggled}");
    }
}
