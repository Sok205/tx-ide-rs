//! The tmux calls prefix+a needs beyond `tx_client::tmux::Ui` (the outer-client walk,
//! focusing a hosting pane), ported from main's `tmux.rs`. All go through `Ui::run` / `Ui::quiet`,
//! so every call carries the core's `-S` and `-f`. Parsers are pure.

use tx_client::tmux::{Ui, UiError};

pub use tx_client::tmux::{DIM_FG_HEX, FG_HEX};

/// `client`'s terminal width in columns; `None` when there is no such client.
pub fn client_width(ui: &Ui, client: &str) -> Option<usize> {
    let (_, out) = ui.quiet(&["display-message", "-p", "-c", client, "#{client_width}"]);
    out.trim().parse().ok()
}

/// A global server option; `None` when unset or there is no server.
pub fn global_option(ui: &Ui, option: &str) -> Option<String> {
    let (_, out) = ui.quiet(&["show-options", "-gvq", option]);
    let out = out.trim();
    (!out.is_empty()).then(|| out.to_owned())
}

/// The pane whose tty a nested client runs on (a client's name is its tty): the pane that
/// client paints into. `None` for a terminal client.
pub fn pane_hosting_client(ui: &Ui, client: &str) -> Option<String> {
    let (_, panes) = ui.quiet(&["list-panes", "-a", "-F", "#{pane_tty}\t#{pane_id}"]);
    panes
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find(|(tty, _)| *tty == client)
        .map(|(_, pane_id)| pane_id.to_owned())
}

/// Runs `command` on the server in the background (`run-shell -b`), outliving the caller.
pub fn run_shell_background(ui: &Ui, command: &str) -> Result<(), UiError> {
    ui.run(&["run-shell", "-b", command]).map(drop)
}

/// Focus the pane hosting `session` for the terminal `client` is shown on. `client` may be a
/// nested client, so the client to move is its outer one. A pane in the outer client's session is
/// selected in place; one in another plain session gets the outer client switched to it; one in
/// another home base (a view that hosts nested attaches: home, tutor) is reported, not switched
/// to. `Err` is the reason, for the user.
pub fn focus_hosting_pane(ui: &Ui, client: &str, session: &str) -> Result<(), String> {
    let (outer, outer_session) = ui
        .outer_client(client)
        .ok_or_else(|| "no terminal client to move".to_owned())?;
    let pane = ui
        .pane_for_session(session, &outer_session)
        .ok_or_else(|| "not attached to any pane".to_owned())?;
    let host = pane.split(':').next().unwrap_or_default();
    let focused = if host == outer_session {
        ui.select_window(&pane) && ui.select_pane(&pane)
    } else if ui.hosts_nested_attaches(host) {
        return Err(format!("attached in view {host}, not in {outer_session}"));
    } else {
        ui.quiet(&["switch-client", "-c", &outer, "-t", &format!("={pane}")])
            .0
    };
    if focused {
        Ok(())
    } else {
        Err(format!("tmux could not focus {pane}"))
    }
}
