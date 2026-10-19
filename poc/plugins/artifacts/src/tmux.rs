//! The tmux calls prefix+a needs beyond `tx_ui::tmux::Ui` (popups, the outer-client walk,
//! focusing a hosting pane), ported from main's `tmux.rs`. All go through `Ui::run` / `Ui::quiet`,
//! so every call carries the core's `-S` and `-f`. Parsers are pure.

use tx_ui::tmux::{Ui, UiError, shell_quote};

pub const ACCENT_HEX: &str = "#7aa2f7";
pub const FG_HEX: &str = "#c0caf5";
pub const DIM_FG_HEX: &str = "#a9b1d6";
pub const BG_DEEP_HEX: &str = "#15161e";

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

/// The argv of a `width`×`height` popup centred on `client`, in the picker's palette.
pub fn popup_args(
    client: &str,
    width: &str,
    height: &str,
    title: &str,
    command: &str,
) -> Vec<String> {
    let style = format!("bg={BG_DEEP_HEX},fg={DIM_FG_HEX}");
    let border = format!("bg={BG_DEEP_HEX},fg={ACCENT_HEX}");
    let title = format!("#[fg={ACCENT_HEX},bold]{title}#[default]");
    [
        "display-popup",
        "-c",
        client,
        "-E",
        "-x",
        "C",
        "-y",
        "C",
        "-w",
        width,
        "-h",
        height,
        "-b",
        "rounded",
        "-s",
        &style,
        "-S",
        &border,
        "-T",
        &title,
        command,
    ]
    .iter()
    .map(|word| (*word).to_owned())
    .collect()
}

/// Opens the popup running `command`; returns when it closes.
pub fn display_popup(
    ui: &Ui,
    client: &str,
    width: &str,
    height: &str,
    title: &str,
    command: &str,
) -> Result<(), UiError> {
    let args = popup_args(client, width, height, title, command);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    ui.run(&args).map(drop)
}

/// The popup's attach command: this server's socket, `$TMUX` cleared, no fallback shell, so
/// detaching closes the popup.
pub fn popup_attach_command(socket: &str, session: &str) -> String {
    format!(
        "TMUX= tmux -S {} attach -t {}",
        shell_quote(socket),
        shell_quote(session)
    )
}

/// Nest-attaches `session` in a popup covering most of `client`'s terminal. While it is open the
/// session's status line shows `footer`; it is hidden again when the popup closes.
pub fn popup_attach(
    ui: &Ui,
    socket: &str,
    client: &str,
    title: &str,
    footer: &str,
    session: &str,
) -> Result<(), UiError> {
    let status_style = format!("bg={BG_DEEP_HEX},fg={DIM_FG_HEX}");
    ui.set_session_option(session, "status", "on")?;
    ui.set_session_option(session, "status-style", &status_style)?;
    ui.set_session_option(session, "status-format[0]", footer)?;
    let command = popup_attach_command(socket, session);
    let shown = display_popup(ui, client, "90%", "90%", title, &command);
    for option in ["status", "status-style", "status-format[0]"] {
        ui.quiet(&["set-option", "-u", "-t", &format!("={session}:"), option]);
    }
    shown
}

/// Runs `command` on the server in the background (`run-shell -b`), outliving the caller.
pub fn run_shell_background(ui: &Ui, command: &str) -> Result<(), UiError> {
    ui.run(&["run-shell", "-b", command]).map(drop)
}

/// Focus the pane hosting `session` for the terminal `client` is shown on. `client` may be a
/// nested client, so the client to move is its outer one. A pane in the outer client's session is
/// selected in place; one in another plain session gets the outer client switched to it; one in
/// another view is reported, not switched to. `Err` is the reason, for the user.
pub fn focus_hosting_pane(ui: &Ui, client: &str, session: &str) -> Result<(), String> {
    let (outer, outer_session) =
        outer_client(ui, client).ok_or_else(|| "no terminal client to move".to_owned())?;
    let pane = ui
        .pane_for_session(session, &outer_session)
        .ok_or_else(|| "not attached to any pane".to_owned())?;
    let host = pane.split(':').next().unwrap_or_default();
    let focused = if host == outer_session {
        ui.select_window(&pane) && ui.select_pane(&pane)
    } else if ui.is_view(host) {
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

    #[test]
    fn the_popup_attach_names_the_core_socket() {
        assert_eq!(
            popup_attach_command("/tmp/s p/tmux.sock", "plan-md"),
            "TMUX= tmux -S '/tmp/s p/tmux.sock' attach -t 'plan-md'"
        );
    }

    #[test]
    fn popups_are_centred_and_run_the_command_last() {
        let args = popup_args("/dev/ttys1", "40", "10", " a ", "cmd");
        assert_eq!(args[..4], ["display-popup", "-c", "/dev/ttys1", "-E"]);
        assert_eq!(args.last().unwrap(), "cmd");
    }
}
