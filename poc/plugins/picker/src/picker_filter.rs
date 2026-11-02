//! The picker's role filter: the roles prefix+t currently hides, kept in `<state>/picker/hidden`
//! (one role per line), which every `_list` reload, the TAB roles panel and the header chips
//! read, so the set survives between pickers. A missing or empty file hides nothing. Port of
//! main's `crates/tx/src/picker_filter.rs`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tx_client::render::{BOLD, DIM_FG_ANSI, RESET, RESET_FG, tag_ansi};
use tx_core::TxCore;

/// The four roles `role_of` gives a row, in panel and chip order.
pub const ROLES: [&str; 4] = ["llm", "nvim", "shell", "other"];

const ON: &str = "●";
const OFF: &str = "○";

pub fn hidden_path(core: &TxCore) -> PathBuf {
    core.config()
        .directories
        .state
        .join("picker")
        .join("hidden")
}

pub fn read_hidden(path: &Path) -> io::Result<Vec<&'static str>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    Ok(parse_hidden(&text))
}

/// Role names separated by newlines or commas; anything else is ignored.
pub fn parse_hidden(text: &str) -> Vec<&'static str> {
    let mut hidden: Vec<&'static str> = text
        .split(['\n', ','])
        .filter_map(|word| ROLES.into_iter().find(|role| *role == word.trim()))
        .collect();
    hidden.dedup();
    hidden
}

pub fn write_hidden(path: &Path, hidden: &[&str]) -> io::Result<()> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
    }
    let text: String = hidden.iter().map(|role| format!("{role}\n")).collect();
    fs::write(path, text)
}

pub fn toggle(path: &Path, role: &'static str) -> io::Result<()> {
    let mut hidden = read_hidden(path)?;
    match hidden.iter().position(|hidden| *hidden == role) {
        Some(index) => {
            hidden.remove(index);
        }
        None => hidden.push(role),
    }
    write_hidden(path, &hidden)
}

/// All on when anything is hidden, else all off.
pub fn toggle_all(path: &Path) -> io::Result<()> {
    let hidden = read_hidden(path)?;
    if hidden.is_empty() {
        write_hidden(path, &ROLES)
    } else {
        write_hidden(path, &[])
    }
}

/// `word` when it names a role; a click on the header's other words gives `None`.
pub fn role_named(word: &str) -> Option<&'static str> {
    ROLES.into_iter().find(|role| *role == word)
}

/// The whitespace-separated word of a header `line` under the 1-based `column` (the line is
/// plain text, one column per character), empty on a space or past the end. fzf hands the click
/// bind the whole line and the column: with `--delimiter=\t` its `FZF_CLICK_HEADER_WORD` is the
/// tab-delimited field, the whole line, not the word.
pub fn word_at(line: &str, column: usize) -> &str {
    let Some((index, character)) = line.char_indices().nth(column.saturating_sub(1)) else {
        return "";
    };
    if character.is_whitespace() {
        return "";
    }
    let start = line[..index]
        .char_indices()
        .rev()
        .find(|(_, character)| character.is_whitespace())
        .map_or(0, |(space, character)| space + character.len_utf8());
    let end = line[index..]
        .find(char::is_whitespace)
        .map_or(line.len(), |space| index + space);
    &line[start..end]
}

fn chip(role: &str, hidden: &[&str]) -> String {
    if hidden.contains(&role) {
        format!("{DIM_FG_ANSI}{OFF} {role}{RESET_FG}")
    } else {
        format!("{}{ON} {role}{RESET_FG}", tag_ansi(role))
    }
}

/// The picker's header line: one chip per role, then the key hint.
pub fn chips_line(hidden: &[&str]) -> String {
    let chips: Vec<String> = ROLES.iter().map(|role| chip(role, hidden)).collect();
    format!(
        "{DIM_FG_ANSI}roles{RESET_FG}  {}   {DIM_FG_ANSI}TAB filter{RESET_FG}",
        chips.join("  ")
    )
}

/// The TAB panel's fzf feed: `role \t chip`, one row per role.
pub fn panel_rows(hidden: &[&str]) -> String {
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
        assert_eq!(parse_hidden("shell, other,bogus"), ["shell", "other"]);
        assert!(parse_hidden("").is_empty());
    }

    #[test]
    fn toggle_flips_and_round_trips() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("picker").join("hidden");
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
        toggle_all(&path).unwrap();
        assert!(read_hidden(&path).unwrap().is_empty());
    }

    #[test]
    fn only_role_words_are_roles() {
        assert_eq!(role_named("shell"), Some("shell"));
        assert_eq!(role_named("roles"), None);
        assert_eq!(role_named("TAB"), None);
        assert_eq!(role_named("●"), None);
    }

    #[test]
    fn the_word_under_a_click() {
        let line = "roles  ● llm  ● nvim  ● shell  ● other   TAB filter";
        assert_eq!(word_at(line, 1), "roles");
        assert_eq!(word_at(line, 10), "llm");
        assert_eq!(word_at(line, 12), "llm");
        assert_eq!(word_at(line, 18), "nvim");
        assert_eq!(word_at(line, 8), "●");
        assert_eq!(word_at(line, 6), "");
        assert_eq!(word_at(line, 200), "");
        assert_eq!(word_at(line, 0), "roles");
    }

    #[test]
    fn chips_mark_hidden_roles_hollow() {
        let line = chips_line(&["nvim"]);
        assert!(line.contains("● llm"));
        assert!(line.contains("○ nvim"));
        assert_eq!(panel_rows(&[]).lines().count(), ROLES.len());
        assert!(panel_rows(&[]).starts_with("llm\t"));
    }
}
