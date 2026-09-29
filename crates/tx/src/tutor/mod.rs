//! `tx tutor`: lesson packs, progress and checks for the vimtutor-style walkthrough.

pub mod checks;
pub mod http;
pub mod pack;
pub mod progress;
pub mod project;
pub mod tx_check;

use std::path::PathBuf;

use crate::storage::Home;

/// The tutor view's nvim `--listen` socket.
pub fn socket_path(home: &Home, lang: &str) -> PathBuf {
    home.root().join("tutor").join(format!("{lang}.sock"))
}

/// The cheat sheet shown under the lesson: `tutor/keys.txt` (`# title`, then `KEYS<TAB>WHAT`
/// rows) with `{prefix}` replaced by the tmux prefix and the key column aligned.
pub fn render_keys(text: &str, prefix: Option<&str>) -> String {
    let prefix = prefix
        .filter(|prefix| !prefix.is_empty() && !prefix.eq_ignore_ascii_case("none"))
        .unwrap_or("prefix");
    let rows: Vec<(String, &str)> = text
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .map(|(keys, what)| (keys.replace("{prefix}", prefix), what))
        .collect();
    let width = rows.iter().map(|(keys, _)| keys.chars().count()).max().unwrap_or(0);
    let mut out = String::new();
    for line in text.lines() {
        if let Some(title) = line.strip_prefix("# ") {
            out.push_str(&format!(" {}\n", title.replace("{prefix}", prefix)));
        }
    }
    for (keys, what) in rows {
        out.push_str(&format!(" {keys:<width$}  {what}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEYS: &str = "# tx-ide keys\nC-h/j/k/l\tmove\n{prefix} t\tpicker\n";

    #[test]
    fn keys_render_with_the_real_prefix_in_aligned_columns() {
        assert_eq!(
            render_keys(KEYS, Some("C-a")),
            " tx-ide keys\n C-h/j/k/l  move\n C-a t      picker\n"
        );
    }

    #[test]
    fn the_title_gets_the_prefix_too() {
        assert!(render_keys("# keys · {prefix} t = prefix then t\n", Some("C-a"))
            .starts_with(" keys · C-a t = prefix then t\n"));
    }

    #[test]
    fn keys_fall_back_to_the_word_prefix() {
        for prefix in [None, Some("None"), Some("")] {
            assert!(render_keys(KEYS, prefix).contains(" prefix t   picker\n"));
        }
    }
}
