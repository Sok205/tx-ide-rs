//! The floor: the home pane's TUI comes back whenever what runs there ends. tmux holds it, not a
//! parent process: `remain-on-exit` keeps the dead pane, the pane's own `pane-died` hook runs
//! `_home-perform` for it, and that respawns the pane with the choice the TUI left in its choice
//! file, else with the TUI. Other plugins' `respawn-pane -k` into the pane does not fire the hook;
//! their program's end does.

use std::path::{Path, PathBuf};

use tx_client::tmux::{Ui, UiError};

/// Foreground commands of a home pane that the TUI may replace: a shell. A nested client
/// (`tmux`) is left alone: the operator is inside a session, and the floor comes back when they
/// leave it.
pub const HOSTING_COMMANDS: [&str; 5] = ["zsh", "bash", "sh", "fish", "dash"];

/// What `pane`'s files are named after: its digits (`%12` → `12`), so no pane id can reach
/// outside the plugin's directory.
fn pane_key(pane: &str) -> String {
    pane.chars().filter(char::is_ascii_digit).collect()
}

/// Where the TUI in `pane` leaves its choice for `_home-perform`.
pub fn choice_path(directory: &Path, pane: &str) -> PathBuf {
    directory.join(format!("choice.{}", pane_key(pane)))
}

/// Where `_home-perform` records the session it nested into `pane`, for the TUI's return.
pub fn last_path(directory: &Path, pane: &str) -> PathBuf {
    directory.join(format!("last.{}", pane_key(pane)))
}

/// Whether the TUI takes over a pane: it is dead, or it runs a shell (an older install's home
/// after its nested attach ended). A nested client, an editor or any other program is left alone.
pub fn takes_floor(dead: bool, command: &str) -> bool {
    dead || HOSTING_COMMANDS.contains(&command)
}

/// `text` as one word for tmux's command parser: double-quoted, with the characters that stay
/// special inside double quotes escaped (`$` would expand an environment variable).
pub fn tmux_quote(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        if matches!(character, '\\' | '"' | '$') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

/// The `pane-died` hook: `_home-perform` for the dead pane, in the background, its output
/// dropped (its errors go to the status line). `tx` is the shell-quoted `tx` command line;
/// `run-shell` expands formats, so its `#` are doubled.
pub fn hook_command(tx: &str) -> String {
    let shell = format!(
        "{} _home-perform #{{hook_pane}} >/dev/null 2>&1",
        tx.replace('#', "##")
    );
    format!("run-shell -b {}", tmux_quote(&shell))
}

/// The floor on `session`'s pane: `remain-on-exit` and the hook as pane options (a split pane
/// in the window keeps its own life), and the TUI (`tui(pane)`, a pane command) in the pane when
/// [`takes_floor`].
pub fn install(
    ui: &Ui,
    session: &str,
    hook: &str,
    tui: impl FnOnce(&str) -> String,
) -> Result<(), UiError> {
    let target = format!("={session}:");
    let pane = ui.run(&[
        "display-message",
        "-p",
        "-t",
        &target,
        "#{pane_id}\t#{pane_dead}\t#{pane_current_command}",
    ])?;
    let mut fields = pane.trim_end_matches('\n').splitn(3, '\t');
    let (Some(pane), Some(dead), Some(command)) = (fields.next(), fields.next(), fields.next())
    else {
        return Err(UiError::Failed {
            args: format!("display-message -t {target}"),
            stderr: format!("unexpected output {pane:?}"),
        });
    };
    ui.run(&["set-option", "-p", "-t", pane, "remain-on-exit", "on"])?;
    ui.run(&["set-hook", "-p", "-t", pane, "pane-died", hook])?;
    if takes_floor(dead == "1", command) {
        ui.respawn_pane(pane, &tui(pane))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_files_are_named_after_the_pane_digits_only() {
        let directory = Path::new("/d/plugins/home");
        assert_eq!(
            choice_path(directory, "%12"),
            Path::new("/d/plugins/home/choice.12")
        );
        assert_eq!(
            last_path(directory, "%3"),
            Path::new("/d/plugins/home/last.3")
        );
        assert_eq!(
            choice_path(directory, "%../../etc/x1/2"),
            Path::new("/d/plugins/home/choice.12")
        );
    }

    #[test]
    fn the_tui_takes_a_dead_pane_or_a_shell_only() {
        assert!(takes_floor(true, "nvim"));
        assert!(takes_floor(false, "zsh"));
        assert!(!takes_floor(false, "tmux"));
        assert!(!takes_floor(false, "nvim"));
        assert!(!takes_floor(false, "tx"));
        assert!(!takes_floor(false, ""));
    }

    #[test]
    fn the_hook_quotes_for_tmux_and_doubles_format_hashes() {
        assert_eq!(
            hook_command("'/o p/tx' '--data-dir' '/d$x/it'\\''s#1'"),
            r#"run-shell -b "'/o p/tx' '--data-dir' '/d\$x/it'\\''s##1' _home-perform #{hook_pane} >/dev/null 2>&1""#
        );
        assert_eq!(tmux_quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
