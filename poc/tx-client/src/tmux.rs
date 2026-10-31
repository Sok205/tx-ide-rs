//! The UI-side tmux helper. Every call is `tmux -S <socket> -f <conf> …` against the core's
//! private server; the core's own `Tmux` is never reached into. Parsers are pure.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tx_core::TxCore;

use crate::views::{KIND_OPTION, VIEW_OPTION, ViewKind};

const BINARY: &str = "tmux";
const LABEL_REV_OPTION: &str = "@tx-label-rev";
const STATUS_MILLISECONDS: &str = "5000";

#[derive(Debug, thiserror::Error)]
pub enum UiError {
    #[error("tmux {args} failed: {stderr}")]
    Failed { args: String, stderr: String },
    #[error("could not run tmux: {0}")]
    Spawn(std::io::Error),
}

impl UiError {
    /// tmux found no server behind the socket (none started yet, or it quit with its last
    /// session): no sessions. Core's rule (`tx-core/src/tmux.rs`, `is_no_server`, `server_quit`).
    pub fn is_no_server(&self) -> bool {
        let UiError::Failed { stderr, .. } = self else {
            return false;
        };
        stderr.starts_with("no server running on ")
            || (stderr.starts_with("error connecting to ")
                && stderr.ends_with("(No such file or directory)"))
            || stderr == "server exited unexpectedly"
    }
}

/// One pane that surfaces a nested session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub host: String,
    pub window_index: String,
    pub window_name: String,
    pub pane_id: String,
    pub pane_index: String,
}

#[derive(Clone, Debug)]
pub struct Ui {
    socket: PathBuf,
    conf: PathBuf,
}

/// An exact session target for pane-typed `-t` (`show-options`, `set-option`).
fn exact_session_pane(name: &str) -> String {
    format!("={name}:")
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

impl Ui {
    pub fn from_core(core: &TxCore) -> Self {
        Self::new(core.socket_path().to_path_buf(), core.config().tmux_conf_path())
    }

    /// Bound to a socket and config of the caller's choosing (scratch servers in tests).
    pub fn new(socket: PathBuf, conf: PathBuf) -> Self {
        Self { socket, conf }
    }

    pub fn argv(&self, args: &[&str]) -> Vec<OsString> {
        let mut argv: Vec<OsString> = vec![
            "-S".into(),
            self.socket.clone().into_os_string(),
            "-f".into(),
            self.conf.clone().into_os_string(),
        ];
        argv.extend(args.iter().map(OsString::from));
        argv
    }

    pub fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(BINARY);
        command.args(self.argv(args)).stdin(Stdio::null());
        command
    }

    pub fn run(&self, args: &[&str]) -> Result<String, UiError> {
        let output = self.command(args).output().map_err(UiError::Spawn)?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        Err(UiError::Failed {
            args: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }

    /// `(succeeded, stdout)`; a tmux that cannot be spawned is `(false, "")`.
    pub fn quiet(&self, args: &[&str]) -> (bool, String) {
        match self.command(args).output() {
            Ok(output) => (
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
            ),
            Err(_) => (false, String::new()),
        }
    }

    /// Whether this process runs inside a client of the core's server (`$TMUX`'s socket).
    pub fn inside(&self) -> bool {
        self.inside_with(std::env::var("TMUX").ok().as_deref())
    }

    fn inside_with(&self, tmux_env: Option<&str>) -> bool {
        tmux_env
            .and_then(|value| value.split(',').next())
            .is_some_and(|socket| !socket.is_empty() && Path::new(socket) == self.socket)
    }

    pub fn display_message(&self, fmt: &str, target: Option<&str>) -> Option<String> {
        let mut args = vec!["display-message"];
        if let Some(target) = target {
            args.extend(["-t", target]);
        }
        args.extend(["-p", fmt]);
        non_empty(self.quiet(&args).1.trim())
    }

    pub fn has_session(&self, name: &str) -> bool {
        self.quiet(&["has-session", "-t", &format!("={name}")]).0
    }

    /// Any view: a session tx-client owns, never a tx session a person can act on.
    pub fn is_view(&self, name: &str) -> bool {
        self.session_option(name, VIEW_OPTION).as_deref() == Some("1")
    }

    fn view_kind(&self, name: &str) -> Option<ViewKind> {
        self.session_option(name, KIND_OPTION)?.parse().ok()
    }

    /// The home base (`@tx-view-kind home`).
    pub fn is_home(&self, name: &str) -> bool {
        self.view_kind(name) == Some(ViewKind::Home)
    }

    /// A view whose shell panes may be replaced by a nested attach (the picker's Enter).
    pub fn hosts_nested_attaches(&self, name: &str) -> bool {
        self.view_kind(name)
            .is_some_and(ViewKind::hosts_nested_attaches)
    }

    pub fn session_option(&self, name: &str, option: &str) -> Option<String> {
        let target = exact_session_pane(name);
        non_empty(
            self.quiet(&["show-options", "-vqt", &target, option])
                .1
                .trim(),
        )
    }

    pub fn set_session_option(&self, name: &str, option: &str, value: &str) -> Result<(), UiError> {
        self.run(&["set-option", "-t", &exact_session_pane(name), option, value])
            .map(drop)
    }

    pub fn pane_option(&self, pane: &str, option: &str) -> Option<String> {
        non_empty(
            self.quiet(&["show-options", "-p", "-vqt", pane, option])
                .1
                .trim(),
        )
    }

    /// Session name → `$N` id for every live session; empty when there is no server.
    pub fn live_targets(&self) -> HashMap<String, String> {
        let (_, out) = self.quiet(&["list-sessions", "-F", "#{session_name}\t#{session_id}"]);
        out.lines()
            .filter_map(|line| line.split_once('\t'))
            .map(|(name, id)| (name.to_owned(), id.to_owned()))
            .collect()
    }

    fn attachment_join(&self) -> Vec<(String, Location)> {
        let (_, clients) = self.quiet(&["list-clients", "-F", "#{client_tty}\t#{client_session}"]);
        let (_, panes) = self.quiet(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_tty}\t#{session_name}\t#{window_index}\t#{window_name}\t#{pane_id}\t#{pane_index}",
        ]);
        parse_attachment_join(&clients, &panes)
    }

    /// Inner session name → every pane surfacing it, in (host, window, pane) order; entries keep
    /// first-seen order.
    pub fn attachment_map(&self) -> Vec<(String, Vec<Location>)> {
        group_attachments(self.attachment_join())
    }

    /// Every pane surfacing `name`; empty when detached or attached from a bare terminal.
    pub fn attached_to(&self, name: &str) -> Vec<Location> {
        self.attachment_map()
            .into_iter()
            .find(|(inner, _)| inner == name)
            .map(|(_, locations)| locations)
            .unwrap_or_default()
    }

    /// Which inner session is nested in this pane (the join, inverted).
    pub fn inner_for_pane(&self, pane_id: &str) -> Option<String> {
        self.attachment_join()
            .into_iter()
            .find(|(_, location)| location.pane_id == pane_id)
            .map(|(inner, _)| inner)
    }

    /// A pane hosting `name` as `host:window.pane`, preferring a host in the comma-separated
    /// `prefer`, else the first.
    pub fn pane_for_session(&self, name: &str, prefer: &str) -> Option<String> {
        let preferred: Vec<&str> = prefer.split(',').filter(|item| !item.is_empty()).collect();
        let locations = self.attached_to(name);
        let chosen = locations
            .iter()
            .find(|loc| preferred.contains(&loc.host.as_str()))
            .or_else(|| locations.first())?;
        Some(format!(
            "{}:{}.{}",
            chosen.host, chosen.window_index, chosen.pane_index
        ))
    }

    /// The session this process runs in; `None` outside the core's server.
    pub fn current_session_name(&self) -> Option<String> {
        if !self.inside() {
            return None;
        }
        self.display_message("#S", None)
    }

    pub fn current_pane_id(&self) -> Option<String> {
        self.display_message("#{pane_id}", None)
    }

    pub fn switch_client(&self, name: &str) -> Result<(), UiError> {
        self.run(&["switch-client", "-t", &format!("={name}")])
            .map(drop)
    }

    pub fn select_window(&self, target: &str) -> bool {
        self.quiet(&["select-window", "-t", target]).0
    }

    pub fn select_pane(&self, target: &str) -> bool {
        self.quiet(&["select-pane", "-t", target]).0
    }

    /// Replace a pane's process with `command` (`respawn-pane -k`).
    pub fn respawn_pane(&self, pane: &str, command: &str) -> Result<(), UiError> {
        self.run(&["respawn-pane", "-k", "-t", pane, command])
            .map(drop)
    }

    /// `run-shell -b`, with `#` doubled so tmux runs `command` verbatim rather than
    /// expanding it as a format.
    pub fn run_shell_background(&self, command: &str) -> Result<(), UiError> {
        self.run(&["run-shell", "-b", &command.replace('#', "##")])
            .map(drop)
    }

    /// Echo `msg` to stderr and show it on `client`'s status line (best effort).
    pub fn display_status(&self, client: &str, msg: &str) {
        eprintln!("{msg}");
        self.quiet(&[
            "display-message",
            "-c",
            client,
            "-d",
            STATUS_MILLISECONDS,
            msg,
        ]);
    }

    /// Redraw the pane-border labels now: tmux caches a `#()` job by its command text, so
    /// bumping `@tx-label-rev` (part of the label's command) forces a fresh one.
    pub fn refresh_labels(&self) {
        let revision = tx_core::events::now().to_string();
        self.quiet(&["set-option", "-g", LABEL_REV_OPTION, &revision]);
        let (_, clients) = self.quiet(&["list-clients", "-F", "#{client_name}"]);
        for client in clients.lines() {
            self.quiet(&["refresh-client", "-t", client]);
        }
    }

    /// Replace this process with `tmux attach-session -t target`, `$TMUX` removed. Returns only
    /// when the exec failed.
    pub fn attach_exec(&self, target: &str) -> std::io::Error {
        self.command(&["attach-session", "-t", target])
            .stdin(Stdio::inherit())
            .env_remove("TMUX")
            .exec()
    }

    /// A bash script that nest-attaches `target`, then falls back to a shell.
    pub fn nest_attach_script(&self, target: &str) -> String {
        format!(
            "set -m; TMUX= tmux -S {} attach -t {}; exec ${{SHELL:-zsh}}",
            shell_quote(&self.socket.to_string_lossy()),
            shell_quote(target),
        )
    }

    /// `nest_attach_script` as one shell command for a pane.
    pub fn nest_attach_command(&self, target: &str) -> String {
        format!("bash -c {}", shell_quote(&self.nest_attach_script(target)))
    }
}

/// `(inner session, hosting pane)` for every pane whose tty is a client's tty. A pane hosting
/// its own session is dropped.
pub fn parse_attachment_join(clients: &str, panes: &str) -> Vec<(String, Location)> {
    let mut session_by_tty: HashMap<&str, &str> = HashMap::new();
    for line in clients.lines() {
        if let Some((tty, session)) = line.split_once('\t')
            && !tty.is_empty()
        {
            session_by_tty.insert(tty, session);
        }
    }
    let mut out = Vec::new();
    for line in panes.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let [
            pane_tty,
            host,
            window_index,
            window_name,
            pane_id,
            pane_index,
        ] = fields.as_slice()
        else {
            continue;
        };
        if let Some(inner) = session_by_tty.get(pane_tty)
            && !inner.is_empty()
            && inner != host
        {
            out.push((
                (*inner).to_owned(),
                Location {
                    host: (*host).to_owned(),
                    window_index: (*window_index).to_owned(),
                    window_name: (*window_name).to_owned(),
                    pane_id: (*pane_id).to_owned(),
                    pane_index: (*pane_index).to_owned(),
                },
            ));
        }
    }
    out
}

fn group_attachments(join: Vec<(String, Location)>) -> Vec<(String, Vec<Location>)> {
    let mut mapping: Vec<(String, Vec<Location>)> = Vec::new();
    for (inner, location) in join {
        match mapping.iter_mut().find(|(name, _)| *name == inner) {
            Some((_, locations)) => locations.push(location),
            None => mapping.push((inner, vec![location])),
        }
    }
    for (_, locations) in &mut mapping {
        locations.sort_by_cached_key(|loc| {
            (
                loc.host.clone(),
                format!("{:0>8}", loc.window_index),
                format!("{:0>8}", loc.pane_index),
            )
        });
    }
    mapping
}

/// POSIX single-quote.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui() -> Ui {
        Ui {
            socket: PathBuf::from("/s/tmux.sock"),
            conf: PathBuf::from("/s/tmux.conf"),
        }
    }

    fn loc(host: &str, window_index: &str, pane_id: &str, pane_index: &str) -> Location {
        Location {
            host: host.to_owned(),
            window_index: window_index.to_owned(),
            window_name: "w".to_owned(),
            pane_id: pane_id.to_owned(),
            pane_index: pane_index.to_owned(),
        }
    }

    #[test]
    fn argv_is_bound_to_socket_and_conf() {
        assert_eq!(
            ui().argv(&["list-sessions", "-F", "#S"]),
            [
                "-S",
                "/s/tmux.sock",
                "-f",
                "/s/tmux.conf",
                "list-sessions",
                "-F",
                "#S"
            ]
            .map(OsString::from)
        );
    }

    #[test]
    fn join_matches_client_tty_to_pane_tty() {
        let clients = "/dev/ttys1\tinner\n/dev/ttys9\tV\n";
        let panes = "/dev/ttys1\tV\t2\tmain\t%7\t1\n\
                     /dev/ttys5\tV\t0\tother\t%1\t0\n\
                     /dev/ttys6\tW\t0\tx\t%2\t0\n";
        assert_eq!(
            parse_attachment_join(clients, panes),
            vec![(
                "inner".to_owned(),
                Location {
                    host: "V".to_owned(),
                    window_index: "2".to_owned(),
                    window_name: "main".to_owned(),
                    pane_id: "%7".to_owned(),
                    pane_index: "1".to_owned(),
                }
            )]
        );
    }

    #[test]
    fn join_skips_self_hosting() {
        let clients = "/dev/ttys1\tV\n";
        let panes = "/dev/ttys1\tV\t0\tw\t%1\t0\n";
        assert!(parse_attachment_join(clients, panes).is_empty());
    }

    #[test]
    fn join_ignores_malformed_lines() {
        let clients = "notab\n\tempty\n/dev/ttys3\t\n/dev/ttys1\tinner\n";
        let panes = "short\tline\n\
                     /dev/ttys3\tV\t0\tw\t%3\t2\n\
                     /dev/ttys1\tV\t0\tw\t%1\t0\n\
                     /dev/ttys1\tV\t0\tw\t%1\t0\textra\n";
        assert_eq!(
            parse_attachment_join(clients, panes),
            vec![("inner".to_owned(), loc("V", "0", "%1", "0"))]
        );
    }

    #[test]
    fn attachment_map_orders_by_host_window_pane() {
        let join = vec![
            ("s".to_owned(), loc("B", "0", "%1", "0")),
            ("s".to_owned(), loc("A", "10", "%2", "0")),
            ("s".to_owned(), loc("A", "9", "%3", "2")),
            ("s".to_owned(), loc("A", "9", "%4", "10")),
            ("t".to_owned(), loc("A", "0", "%5", "0")),
        ];
        let map = group_attachments(join);
        assert_eq!(map.len(), 2);
        assert_eq!(map[0].0, "s");
        let order: Vec<&str> = map[0].1.iter().map(|l| l.pane_id.as_str()).collect();
        assert_eq!(order, ["%3", "%4", "%2", "%1"]);
        assert_eq!(map[1].0, "t");
    }

    #[test]
    fn shell_quote_wraps_and_escapes() {
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn inside_requires_matching_socket() {
        let ui = ui();
        assert!(ui.inside_with(Some("/s/tmux.sock,123,0")));
        assert!(!ui.inside_with(Some("/other/tmux.sock,123,0")));
        assert!(!ui.inside_with(Some("")));
        assert!(!ui.inside_with(None));
    }

    #[test]
    fn run_shell_background_runs_the_command_verbatim() {
        let dir = tempfile::Builder::new()
            .prefix("txui")
            .tempdir_in("/tmp")
            .unwrap();
        let conf = dir.path().join("tmux.conf");
        std::fs::write(&conf, "").unwrap();
        let ui = Ui {
            socket: dir.path().join("tmux.sock"),
            conf,
        };
        if ui.run(&["new-session", "-d", "-s", "t"]).is_err() {
            eprintln!("tmux not available; skipping");
            return;
        }
        let out = dir.path().join("out");
        let command = format!(
            "printf %s {} > {}",
            shell_quote("a#S#{pane_id}##b"),
            out.display()
        );
        let result = ui.run_shell_background(&command);
        let mut text = String::new();
        for _ in 0..50 {
            text = std::fs::read_to_string(&out).unwrap_or_default();
            if !text.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        ui.quiet(&["kill-server"]);
        result.unwrap();
        assert_eq!(text, "a#S#{pane_id}##b");
    }

    #[test]
    fn nest_attach_command_quotes_socket_and_target() {
        assert_eq!(
            ui().nest_attach_command("it's"),
            r#"bash -c 'set -m; TMUX= tmux -S '\''/s/tmux.sock'\'' attach -t '\''it'\''\'\'''\''s'\''; exec ${SHELL:-zsh}'"#
        );
    }
}
