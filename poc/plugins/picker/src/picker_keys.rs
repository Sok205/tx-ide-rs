//! The picker's vim-style normal mode and the stateful key handler behind `_picker-key` (Esc,
//! Enter, p). Port of main's `crates/tx/src/picker_keys.rs` without the row-number digits.

use tx_client::render::{ACCENT_ANSI, BOLD, RESET};

/// The pending-pick file: holds [`POPUP_MARK`] while popup mode is armed, else empty. The
/// picker's auto-reload pauses while it is non-empty.
pub const DIGITS_FILE_ENV: &str = "TX_DIGITS_FILE";
/// What the pending-pick file holds while popup mode is armed.
pub const POPUP_MARK: &str = "p";
/// What a popup pick has fzf print ahead of the selection.
pub const POPUP_LINE: &str = "popup";

pub const NORMAL_PROMPT: &str = "  ❯ ";
pub const SEARCH_PROMPT: &str = "  / ";

const ACTION_KEYS: [&str; 15] = [
    "j", "k", "g", "G", "ctrl-d", "ctrl-u", "/", "i", "d", "t", "r", "q", "y", "n", "p",
];

/// Every printable key but the action keys, and space: bound to `ignore` in normal mode.
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

pub struct KeyState {
    pub searching: bool,
    pub popup: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Reply {
    pub popup: bool,
    pub actions: String,
}

impl Reply {
    /// The pending-pick file's new content.
    pub fn pending(&self) -> &'static str {
        if self.popup { POPUP_MARK } else { "" }
    }
}

pub fn normal_footer(popup: bool) -> String {
    if popup {
        format!(
            "{ACCENT_ANSI}{BOLD} POPUP{RESET}  j/k move · enter opens the row in a popup · esc back"
        )
    } else {
        format!(
            "{ACCENT_ANSI}{BOLD} NORMAL{RESET}  j/k move · enter open · p popup mode · i or / search · d kill · t tags · q quit"
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
        normal_footer(false)
    )
}

pub fn press(key: &str, state: &KeyState) -> Option<Reply> {
    let cleared = |actions: String| Reply {
        popup: false,
        actions,
    };
    let footer = |popup: bool| format!("change-footer:{}", normal_footer(popup));
    let reply = match key {
        "esc" if state.searching => cleared(enter_normal()),
        "esc" if state.popup => cleared(footer(false)),
        "esc" => cleared("abort".to_owned()),
        "p" => Reply {
            popup: true,
            actions: footer(true),
        },
        "enter" if state.popup => cleared(format!("print({POPUP_LINE})+accept")),
        "enter" => cleared("accept".to_owned()),
        _ => return None,
    };
    Some(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL: KeyState = KeyState {
        searching: false,
        popup: false,
    };
    const POPUP: KeyState = KeyState {
        searching: false,
        popup: true,
    };
    const SEARCH: KeyState = KeyState {
        searching: true,
        popup: false,
    };

    #[test]
    fn esc_leaves_search_then_popup_then_closes() {
        let reply = press("esc", &SEARCH).unwrap();
        assert!(reply.actions.starts_with("change-prompt(  ❯ )+rebind("));
        assert!(!reply.popup);
        let reply = press("esc", &POPUP).unwrap();
        assert!(reply.actions.starts_with("change-footer:"));
        assert!(reply.actions.contains("NORMAL"));
        assert_eq!(reply.pending(), "");
        assert_eq!(press("esc", &NORMAL).unwrap().actions, "abort");
    }

    #[test]
    fn p_arms_popup_mode_and_enter_opens_the_row_in_it() {
        let reply = press("p", &NORMAL).unwrap();
        assert!(reply.popup);
        assert_eq!(reply.pending(), POPUP_MARK);
        assert!(reply.actions.contains("POPUP"));
        let reply = press("enter", &POPUP).unwrap();
        assert_eq!(reply.actions, "print(popup)+accept");
        assert_eq!(reply.pending(), "");
        assert_eq!(press("enter", &NORMAL).unwrap().actions, "accept");
    }

    #[test]
    fn digits_are_not_keys() {
        assert_eq!(press("3", &NORMAL), None);
        let ignored = ignored_keys();
        for digit in '0'..='9' {
            assert!(ignored.contains(&digit.to_string()), "{digit}");
        }
        for key in ["j", "/", "p", "d", "y"] {
            assert!(!ignored.contains(&key.to_owned()), "{key}");
        }
        assert!(ignored.contains(&"space".to_owned()));
    }

    #[test]
    fn toggling_puts_parens_in_brackets() {
        let unbind = toggle_normal_keys("unbind");
        assert!(unbind.starts_with("unbind(j,k,g,G,ctrl-d,ctrl-u,/,i,d,t,r,q,y,n,p,"));
        assert!(unbind.ends_with("+unbind[(,)]"));
    }
}
