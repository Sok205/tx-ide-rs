//! Showing a session on the operator's terminal, for any plugin: focus the pane already showing
//! it, let the operator pick a pane to attach it into (attach-to), or nest-attach it in a popup.
//! [`Ui::show_session`] is safe to call from inside the caller's own popup: focus and attach-to
//! act at once (attach-to's key table outlives the popup), and the popup waits in a background
//! `run-shell` until the caller's popup has gone, since a client shows one popup at a time.
//!
//! Attach-to is the key table [`TABLE`] (`plugins/ide/keys/ide.tmux`): h/j/k/l, the arrows, a pane number
//! or a click select a pane under a gold active border, Enter runs `tx _attach-into`, Esc / q
//! leave; any other key stays in it.

use crate::palette::{DIM_FG_HEX, FG_HEX, WARN_HEX};
use crate::tmux::{Ui, popup_args, shell_quote};

/// The key table attach-to picks a pane in.
pub const TABLE: &str = "tx-attach-to";
/// Global option: the session attach-to attaches; the table's Enter reads it.
pub const SESSION_OPTION: &str = "@tx-attach-to-session";
/// Attach-to's message, a format; `plugins/ide/keys/ide.tmux` shows the same text after every key.
pub const MESSAGE: &str = "attach #{@tx-attach-to-session}: h/j/k/l, a number or a click picks a pane, Enter attaches here, Esc cancels";
const POPUP_SIZE: &str = "90%";

/// How a session is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Show {
    /// Move the terminal to the pane already showing it.
    Focus,
    /// The operator picks a pane, the session is attached there.
    AttachTo,
    /// Nest-attach it in a popup over the terminal.
    Popup,
}

impl Show {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "focus" => Some(Self::Focus),
            "attach-to" => Some(Self::AttachTo),
            "popup" => Some(Self::Popup),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Focus => "focus",
            Self::AttachTo => "attach-to",
            Self::Popup => "popup",
        }
    }
}

impl Ui {
    /// Show `session` on the terminal `client` (possibly a nested client) is shown on. `pane` is
    /// where the caller was opened, the pane attach-to starts on (else the terminal's current
    /// one). `Err` is the reason, for the user.
    pub fn show_session(
        &self,
        client: &str,
        show: Show,
        session: &str,
        pane: Option<&str>,
    ) -> Result<(), String> {
        match show {
            Show::Focus => self.focus_session(client, session),
            Show::AttachTo => self.start_attach_to(client, session, pane),
            Show::Popup => self.deferred_popup_attach(client, session),
        }
    }

    fn outer_or_err(&self, client: &str) -> Result<(String, String), String> {
        self.outer_client(client)
            .ok_or_else(|| "no terminal client".to_owned())
    }

    /// Attach-to: the terminal's next keys pick the pane `session` is attached into.
    fn start_attach_to(
        &self,
        client: &str,
        session: &str,
        pane: Option<&str>,
    ) -> Result<(), String> {
        let (outer, outer_session) = self.outer_or_err(client)?;
        let window = match pane {
            Some(pane) => {
                self.select_window(pane);
                self.select_pane(pane);
                pane.to_owned()
            }
            None => format!("={outer_session}:"),
        };
        let border = format!("fg={WARN_HEX},bold");
        self.run(&[
            "set-option",
            "-w",
            "-t",
            &window,
            "pane-active-border-style",
            &border,
        ])
        .and_then(|_| self.run(&["set-option", "-g", SESSION_OPTION, session]))
        .and_then(|_| self.run(&["switch-client", "-c", &outer, "-T", TABLE]))
        .and_then(|_| self.run(&["display-message", "-c", &outer, "-d", "0", MESSAGE]))
        .map(drop)
        .map_err(|error| error.to_string())
    }

    /// `session` in a popup over the terminal, opened from a background `run-shell` so that it
    /// waits for a popup the caller may be running in; its footer says how to leave, and goes
    /// with the popup.
    fn deferred_popup_attach(&self, client: &str, session: &str) -> Result<(), String> {
        let (outer, _) = self.outer_or_err(client)?;
        let prefix = self
            .quiet(&["show-option", "-gv", "prefix"])
            .1
            .trim()
            .to_owned();
        let footer =
            format!(" #[fg={FG_HEX}]{prefix} d#[fg={DIM_FG_HEX}] close, the session keeps running");
        let target = format!("={session}:");
        let shown = format!("={session}");
        let options = [
            ("status", "on".to_owned()),
            (
                "status-style",
                format!("bg={},fg={DIM_FG_HEX}", crate::palette::BG_DEEP_HEX),
            ),
            ("status-format[0]", footer),
        ];
        for (option, value) in &options {
            self.run(&["set-option", "-t", &target, option, value])
                .map_err(|error| error.to_string())?;
        }
        let attach = crate::tmux::popup_attach_command(&self.socket_text(), &shown);
        let popup = popup_args(
            &outer,
            POPUP_SIZE,
            POPUP_SIZE,
            &format!(" {session} "),
            &attach,
        );
        let popup: Vec<&str> = popup.iter().map(String::as_str).collect();
        let mut script = vec![self.shell_command(&popup)];
        // The whole `status-format` array: unsetting only `[0]` blanks the global status line.
        for option in ["status", "status-style", "status-format"] {
            script.push(self.shell_command(&["set-option", "-u", "-t", &target, option]));
        }
        self.run_shell_background(&script.join("; "))
            .map_err(|error| error.to_string())
    }

    /// `tmux -S <socket> -f <conf> <args>` as one shell command.
    fn shell_command(&self, args: &[&str]) -> String {
        std::iter::once("tmux".to_owned())
            .chain(
                self.argv(args)
                    .iter()
                    .map(|word| shell_quote(&word.to_string_lossy())),
            )
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAGMENT: &str = include_str!("../../plugins/ide/keys/ide.tmux");

    #[test]
    fn modes_round_trip() {
        for show in [Show::Focus, Show::AttachTo, Show::Popup] {
            assert_eq!(Show::parse(show.name()), Some(show));
        }
        assert_eq!(Show::parse("attach"), None);
    }

    #[test]
    fn the_fragment_binds_the_table_and_shows_the_same_message() {
        assert!(FRAGMENT.contains(MESSAGE));
        assert!(FRAGMENT.contains(&format!("bind -T {TABLE} Enter")));
        assert!(FRAGMENT.contains(&format!("bind -T {TABLE} Any")));
        assert!(FRAGMENT.contains(&format!("# unset: {SESSION_OPTION}")));
    }
}
