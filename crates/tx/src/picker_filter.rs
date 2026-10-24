//! The picker's role filter: the roles `tx attach` currently hides, kept in a small state file
//! (one role per line) that every `tx _list` reload, the TAB roles panel and the header chips
//! read. The file's path travels in [`HIDE_FILE_ENV`]; a missing or empty file hides nothing.

use std::fs;
use std::io;
use std::path::Path;

use crate::palette::{BOLD, DIM_FG_ANSI, RESET, RESET_FG, tag_ansi};
use crate::session::Role;

pub const HIDE_FILE_ENV: &str = "TX_HIDE_FILE";

const ON: &str = "●";
const OFF: &str = "○";

pub fn read_hidden(path: &Path) -> io::Result<Vec<Role>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    Ok(parse_hidden(&text))
}

/// Role names separated by newlines or commas; anything else is ignored.
pub fn parse_hidden(text: &str) -> Vec<Role> {
    let mut hidden: Vec<Role> = text
        .split(['\n', ','])
        .filter_map(|word| word.trim().parse().ok())
        .collect();
    hidden.dedup();
    hidden
}

pub fn write_hidden(path: &Path, hidden: &[Role]) -> io::Result<()> {
    let text: String = hidden.iter().map(|role| format!("{role}\n")).collect();
    fs::write(path, text)
}

pub fn toggle(path: &Path, role: Role) -> io::Result<()> {
    let mut hidden = read_hidden(path)?;
    match hidden.iter().position(|hidden| *hidden == role) {
        Some(index) => {
            hidden.remove(index);
        }
        None => hidden.push(role),
    }
    write_hidden(path, &hidden)
}

fn chip(role: Role, hidden: &[Role]) -> String {
    let name = role.as_str();
    if hidden.contains(&role) {
        format!("{DIM_FG_ANSI}{OFF} {name}{RESET_FG}")
    } else {
        format!("{}{ON} {name}{RESET_FG}", tag_ansi(name))
    }
}

/// The picker's header line: one chip per role, then the key hint.
pub fn chips_line(hidden: &[Role]) -> String {
    let chips: Vec<String> = Role::ALL.iter().map(|role| chip(*role, hidden)).collect();
    format!(
        "{DIM_FG_ANSI}roles{RESET_FG}  {}   {DIM_FG_ANSI}TAB filter{RESET_FG}",
        chips.join("  ")
    )
}

/// The TAB panel's fzf feed: `role \t chip`, one row per role.
pub fn panel_rows(hidden: &[Role]) -> String {
    Role::ALL
        .iter()
        .map(|role| format!("{role}\t{}", chip(*role, hidden)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The TAB panel's header: a bold key legend.
pub fn panel_legend() -> String {
    format!(
        "{DIM_FG_ANSI}{BOLD}space{RESET}{DIM_FG_ANSI} toggle · {BOLD}a{RESET}{DIM_FG_ANSI} all on · {BOLD}⏎{RESET}{DIM_FG_ANSI} close{RESET}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_takes_newlines_commas_and_ignores_junk() {
        assert_eq!(parse_hidden("llm\nnvim\n"), [Role::Llm, Role::Nvim]);
        assert_eq!(
            parse_hidden("shell, other,bogus"),
            [Role::Shell, Role::Other]
        );
        assert!(parse_hidden("").is_empty());
    }

    #[test]
    fn toggle_flips_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hide");
        assert!(read_hidden(&path).unwrap().is_empty());
        toggle(&path, Role::Nvim).unwrap();
        assert_eq!(read_hidden(&path).unwrap(), [Role::Nvim]);
        toggle(&path, Role::Shell).unwrap();
        toggle(&path, Role::Nvim).unwrap();
        assert_eq!(read_hidden(&path).unwrap(), [Role::Shell]);
    }

    #[test]
    fn chips_mark_hidden_roles_hollow() {
        let line = chips_line(&[Role::Nvim]);
        assert!(line.contains("● llm"));
        assert!(line.contains("○ nvim"));
        assert_eq!(panel_rows(&[]).lines().count(), Role::ALL.len());
        assert!(panel_rows(&[]).starts_with("llm\t"));
    }
}
