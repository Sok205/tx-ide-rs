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

/// The real-terminal client that ultimately shows `client`, with its session.
pub fn outer_client(ui: &Ui, client: &str) -> Option<(String, String)> {
    let (_, clients) = ui.quiet(&[
        "list-clients",
        "-F",
        "#{client_name}\t#{client_tty}\t#{client_session}\t#{client_activity}",
    ]);
    let (_, panes) = ui.quiet(&["list-panes", "-a", "-F", "#{pane_tty}\t#{session_name}"]);
    walk_to_outer_client(&clients, &panes, client)
}

/// A client whose tty is some pane's tty is nested in that pane, so the walk ascends to the most
/// recently active client of that pane's session until the tty belongs to no pane. `None` when
/// the chain ends without a terminal client.
pub fn walk_to_outer_client(clients: &str, panes: &str, client: &str) -> Option<(String, String)> {
    let clients: Vec<(&str, &str, &str, i64)> = clients
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            let [name, tty, session, activity] = fields.as_slice() else {
                return None;
            };
            Some((*name, *tty, *session, activity.trim().parse().unwrap_or(0)))
        })
        .collect();
    let pane_sessions: Vec<(&str, &str)> = panes
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .collect();
    let mut current = clients.iter().find(|(name, ..)| *name == client)?;
    for _ in 0..16 {
        let Some((_, hosting_session)) = pane_sessions.iter().find(|(tty, _)| *tty == current.1)
        else {
            return Some((current.0.to_owned(), current.2.to_owned()));
        };
        current = clients
            .iter()
            .filter(|(.., session, _)| session == hosting_session)
            .max_by_key(|(.., activity)| *activity)?;
    }
    None
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
    let (outer, outer_session) =
        outer_client(ui, client).ok_or_else(|| "no terminal client to move".to_owned())?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outer_client_walks_up_nested_attachments() {
        let clients = "/dev/ttys001\t/dev/ttys001\tViews\t100\n\
                       /dev/ttys002\t/dev/ttys002\tinner\t200\n\
                       /dev/ttys003\t/dev/ttys003\tdeep\t300\n\
                       /dev/ttys004\t/dev/ttys004\tViews\t50\n";
        let panes = "/dev/ttys002\tViews\n/dev/ttys003\tinner\n/dev/ttys009\tdeep\n";
        assert_eq!(
            walk_to_outer_client(clients, panes, "/dev/ttys003"),
            Some(("/dev/ttys001".to_owned(), "Views".to_owned()))
        );
        assert_eq!(
            walk_to_outer_client(clients, panes, "/dev/ttys001"),
            Some(("/dev/ttys001".to_owned(), "Views".to_owned()))
        );
        let orphan = "/dev/ttys003\t/dev/ttys003\tdeep\t300\n";
        assert_eq!(walk_to_outer_client(orphan, panes, "/dev/ttys003"), None);
        assert_eq!(walk_to_outer_client(clients, panes, "/dev/ttys404"), None);
        let looped = "/dev/ttys005\tViews\n";
        let self_hosted = "/dev/ttys005\t/dev/ttys005\tViews\t1\n";
        assert_eq!(
            walk_to_outer_client(self_hosted, looped, "/dev/ttys005"),
            None
        );
    }
}
