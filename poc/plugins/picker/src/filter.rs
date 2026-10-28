//! The picker's role filter: the roles `txc pick` currently hides, kept in a small state file
//! (one role per line) that every `_list` reload, the TAB roles panel and the header chips read.
//! A missing or empty file hides nothing. Port of main's `picker_filter.rs` over tx-ui's role
//! names.

use std::fs;
use std::io;
use std::path::Path;

use tx_ui::render::{BOLD, DIM_FG_ANSI, RESET, RESET_FG, tag_ansi};

/// The roles `tx_ui::render::role_of` yields, in chip order.
pub const ROLES: [&str; 4] = ["llm", "nvim", "view", "shell"];

const ON: &str = "●";
const OFF: &str = "○";

pub fn read_hidden(path: &Path) -> io::Result<Vec<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse_hidden(&text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

/// Role names separated by newlines or commas; anything else is ignored.
pub fn parse_hidden(text: &str) -> Vec<String> {
    let mut hidden: Vec<String> = text
        .split(['\n', ','])
        .map(str::trim)
        .filter(|word| ROLES.contains(word))
        .map(str::to_owned)
        .collect();
    hidden.dedup();
    hidden
}

pub fn write_hidden(path: &Path, hidden: &[String]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text: String = hidden.iter().map(|role| format!("{role}\n")).collect();
    fs::write(path, text)
}

pub fn toggle(path: &Path, role: &str) -> io::Result<()> {
    let mut hidden = read_hidden(path)?;
    match hidden.iter().position(|hidden| hidden == role) {
        Some(index) => {
            hidden.remove(index);
        }
        None => hidden.push(role.to_owned()),
    }
    write_hidden(path, &hidden)
}

/// All on when anything is hidden, else all off.
pub fn toggle_all(path: &Path) -> io::Result<()> {
    if read_hidden(path)?.is_empty() {
        write_hidden(path, &ROLES.map(str::to_owned))
    } else {
        write_hidden(path, &[])
    }
}

fn chip(role: &str, hidden: &[String]) -> String {
    if hidden.iter().any(|hidden| hidden == role) {
        format!("{DIM_FG_ANSI}{OFF} {role}{RESET_FG}")
    } else {
        format!("{}{ON} {role}{RESET_FG}", tag_ansi(role))
    }
}

/// The picker's header line: one chip per role, then the key hint.
pub fn chips_line(hidden: &[String]) -> String {
    let chips: Vec<String> = ROLES.iter().map(|role| chip(role, hidden)).collect();
    format!(
        "{DIM_FG_ANSI}roles{RESET_FG}  {}   {DIM_FG_ANSI}TAB filter{RESET_FG}",
        chips.join("  ")
    )
}

/// The TAB panel's fzf feed: `role \t chip`, one row per role.
pub fn panel_rows(hidden: &[String]) -> String {
    ROLES
        .iter()
        .map(|role| format!("{role}\t{}", chip(role, hidden)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The TAB panel's header: a bold key legend.
pub fn panel_legend() -> String {
    format!(
        "{DIM_FG_ANSI}{BOLD}space{RESET}{DIM_FG_ANSI} toggle · {BOLD}a{RESET}{DIM_FG_ANSI} toggle all · {BOLD}⏎{RESET}{DIM_FG_ANSI} close{RESET}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_takes_newlines_commas_and_ignores_junk() {
        assert_eq!(parse_hidden("llm\nnvim\n"), ["llm", "nvim"]);
        assert_eq!(parse_hidden("shell, view,bogus"), ["shell", "view"]);
        assert!(parse_hidden("").is_empty());
    }

    #[test]
    fn toggle_flips_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plugins/picker/hidden-roles");
        assert!(read_hidden(&path).unwrap().is_empty());
        toggle(&path, "nvim").unwrap();
        assert_eq!(read_hidden(&path).unwrap(), ["nvim"]);
        toggle(&path, "shell").unwrap();
        toggle(&path, "nvim").unwrap();
        assert_eq!(read_hidden(&path).unwrap(), ["shell"]);
        toggle_all(&path).unwrap();
        assert!(read_hidden(&path).unwrap().is_empty());
        toggle_all(&path).unwrap();
        assert_eq!(read_hidden(&path).unwrap(), ROLES);
    }

    #[test]
    fn chips_mark_hidden_roles_hollow() {
        let line = chips_line(&["nvim".to_owned()]);
        assert!(line.contains("● llm"));
        assert!(line.contains("○ nvim"));
        assert_eq!(panel_rows(&[]).lines().count(), ROLES.len());
        assert!(panel_rows(&[]).starts_with("llm\t"));
    }
}
