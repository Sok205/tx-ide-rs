//! Port of `lib/tx/tmux.py`: the single adapter wrapping every tmux subprocess call.
//!
//! Deviations from the reference (FIX quirks, `PARITY-CONTRACT.md`):
//! - Q27: calls that address a session BY NAME use exact targets — `=name` for session-typed
//!   targets (`has-session`, `kill-session`, `rename-session`, `switch-client`) and `=name:` for
//!   pane-typed ones (`show-options` / `set-option` behind `get_tx_id` /
//!   `set_tx_view` / `is_view`; tmux rejects a bare `=name` there). Generic target-taking methods
//!   (`set_option`, `send_keys`, `display_message`, …) pass the caller's target through untouched.
//! - Q30: `@remote-session` is read at PANE scope (`show-options -p`).

use std::ffi::{OsStr, OsString};
use std::io;
use std::process::{Command, Stdio};

use crate::session::Location;

/// Injected into every spawned session so colours render in a detached `new-session -d`.
pub const TRUECOLOR_ENV: [(&str, &str); 2] =
    [("COLORTERM", "truecolor"), ("TERM", "xterm-256color")];

/// tmux rejects a client command past one imsg; commands over this conservative half-limit go
/// through a launch script instead.
pub const MAX_COMMAND_BYTES: usize = 8192;

/// Ordered attribute list rendered by [`format_envelope`] (the Python's ordered dict).
pub type EnvelopeAttrs = Vec<(String, String)>;

#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    /// A tmux invocation expected to succeed exited nonzero.
    #[error("tmux {args} failed: {stderr}")]
    Failed { args: String, stderr: String },
    #[error("could not run {binary}: {source}")]
    Spawn { binary: String, source: io::Error },
    #[error("invalid literal for int() with base 10: '{0}'")]
    BadPid(String),
}

/// The process environment tmux.py consults, captured once at the edge.
#[derive(Clone, Debug, Default)]
pub struct TmuxEnv {
    /// `$TMUX` (set inside a tmux client).
    pub tmux: Option<String>,
}

impl TmuxEnv {
    pub fn from_process() -> Self {
        Self {
            tmux: std::env::var("TMUX").ok(),
        }
    }

    fn inside_tmux(&self) -> bool {
        self.tmux.as_deref().is_some_and(|value| !value.is_empty())
    }
}

#[derive(Clone, Debug)]
pub struct Tmux {
    binary: OsString,
    env: TmuxEnv,
}

/// An exact session target for session-typed `-t` (`has-session`, `kill-session`, …).
fn exact_session(name: &str) -> String {
    format!("={name}")
}

/// An exact session target for pane-typed `-t` (`show-options`, `set-option`).
fn exact_session_pane(name: &str) -> String {
    format!("={name}:")
}

impl Tmux {
    pub fn new(binary: impl Into<OsString>, env: TmuxEnv) -> Self {
        Self {
            binary: binary.into(),
            env,
        }
    }

    pub fn binary(&self) -> &OsStr {
        &self.binary
    }

    // ----- raw subprocess plumbing ---------------------------------------------------------

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.binary);
        command.args(args).stdin(Stdio::inherit());
        command
    }

    fn spawn_error(&self, source: io::Error) -> TmuxError {
        TmuxError::Spawn {
            binary: self.binary.to_string_lossy().into_owned(),
            source,
        }
    }

    /// Run a tmux call that must succeed; `TmuxError::Failed` on a nonzero exit.
    fn run(&self, args: &[&str]) -> Result<String, TmuxError> {
        let output = self
            .command(args)
            .output()
            .map_err(|source| self.spawn_error(source))?;
        if !output.status.success() {
            return Err(TmuxError::Failed {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Run a call whose nonzero exit is a normal answer; `(success, stdout)`. A tmux binary that
    /// cannot be started is an unsuccessful answer too.
    fn run_quiet(&self, args: &[&str]) -> (bool, String) {
        match self.command(args).output() {
            Ok(output) => (
                output.status.success(),
                String::from_utf8_lossy(&output.stdout).into_owned(),
            ),
            Err(_) => (false, String::new()),
        }
    }

    // ----- session lifecycle ---------------------------------------------------------------

    /// Whether a session named exactly `name` is live.
    pub fn has_session(&self, name: &str) -> bool {
        self.run_quiet(&["has-session", "-t", &exact_session(name)])
            .0
    }

    /// Create a detached session running `command`, returning its first pane's pid. Caller `env`
    /// overrides the truecolor defaults (same key order as `{**TRUECOLOR_ENV, **env}`).
    pub fn new_session(
        &self,
        name: &str,
        cwd: &str,
        command: &str,
        env: &[(String, String)],
    ) -> Result<i64, TmuxError> {
        let mut merged: Vec<(String, String)> = TRUECOLOR_ENV
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        for (key, value) in env {
            match merged.iter_mut().find(|(existing, _)| existing == key) {
                Some(slot) => slot.1.clone_from(value),
                None => merged.push((key.clone(), value.clone())),
            }
        }
        let env_args: Vec<String> = merged
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        let mut args = vec![
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{pane_pid}",
            "-s",
            name,
            "-c",
            cwd,
        ];
        for pair in &env_args {
            args.extend(["-e", pair.as_str()]);
        }
        args.push(command);
        let out = self.run(&args)?;
        let pid = out.trim();
        pid.parse().map_err(|_| TmuxError::BadPid(pid.to_owned()))
    }

    pub fn kill_session(&self, name: &str) -> bool {
        self.run_quiet(&["kill-session", "-t", &exact_session(name)])
            .0
    }

    pub fn rename_session(&self, old: &str, new: &str) -> Result<(), TmuxError> {
        self.run(&["rename-session", "-t", &exact_session(old), new])
            .map(drop)
    }

    /// The single `list-sessions -F fmt` scan (the sole liveness signal). No server → `[]`.
    pub fn list_sessions(&self, fmt: &str) -> Vec<String> {
        let (ok, out) = self.run_quiet(&["list-sessions", "-F", fmt]);
        if !ok {
            return Vec::new();
        }
        py_splitlines(&out)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }

    // ----- keys / options / hooks ----------------------------------------------------------

    /// `literal = false` lets key names through (`"Enter"`); `true` types the string verbatim.
    pub fn send_keys(&self, target: &str, keys: &str, literal: bool) -> Result<(), TmuxError> {
        let mut args = vec!["send-keys", "-t", target];
        if literal {
            args.push("-l");
        }
        args.extend(["--", keys]);
        self.run(&args).map(drop)
    }

    pub fn set_option(
        &self,
        target: &str,
        option: &str,
        value: &str,
        pane: bool,
    ) -> Result<(), TmuxError> {
        let mut args = vec!["set-option", "-t", target];
        if pane {
            args.push("-p");
        }
        args.extend([option, value]);
        self.run(&args).map(drop)
    }

    /// Best-effort `set-option -u` (tolerant: the pane may already be gone).
    pub fn unset_option(&self, target: &str, option: &str, pane: bool) {
        let mut args = vec!["set-option", "-t", target];
        if pane {
            args.push("-p");
        }
        args.extend(["-u", option]);
        self.run_quiet(&args);
    }

    pub fn set_window_option(
        &self,
        target: &str,
        option: &str,
        value: &str,
    ) -> Result<(), TmuxError> {
        self.run(&["set-window-option", "-t", target, option, value])
            .map(drop)
    }

    /// Read one option (`-vq`) at the target's session scope; `None` when unset / empty / no such
    /// target.
    pub fn show_option(&self, target: &str, option: &str) -> Option<String> {
        let (_, out) = self.run_quiet(&["show-options", "-vqt", target, option]);
        non_empty(out.trim())
    }

    /// Read one PANE option (`-p -vq`); `None` when unset / empty / pane gone (Q30).
    pub fn show_pane_option(&self, pane: &str, option: &str) -> Option<String> {
        let (_, out) = self.run_quiet(&["show-options", "-p", "-vqt", pane, option]);
        non_empty(out.trim())
    }

    pub fn get_tx_id(&self, name: &str) -> Option<String> {
        self.show_option(&exact_session_pane(name), "@tx_id")
    }

    /// Stamped only on a session just created under a fresh uuid, so the bare target cannot
    /// prefix-match another session (a prefix would have to contain the whole uuid); bare keeps
    /// the reference's error text (`set-option -t <id> … no such session: <id>`, T-TMUX-01/06).
    pub fn set_tx_id(&self, name: &str, session_id: &str) -> Result<(), TmuxError> {
        self.set_option(name, "@tx_id", session_id, false)
    }

    /// Mark a live session as a view (`@tx_view` is a view's entire durable identity).
    pub fn set_tx_view(&self, name: &str) -> Result<(), TmuxError> {
        self.set_option(&exact_session_pane(name), "@tx_view", "1", false)
    }

    pub fn is_view(&self, name: &str) -> bool {
        self.show_option(&exact_session_pane(name), "@tx_view")
            .as_deref()
            == Some("1")
    }

    pub fn switch_client(&self, name: &str) -> Result<(), TmuxError> {
        self.run(&["switch-client", "-t", &exact_session(name)])
            .map(drop)
    }

    /// `switch-client -t <name>` with the reference's bare target: `tx start` switches to its
    /// fixed `Views` home and its failure text names that argv (T-TMUX-19).
    pub fn switch_client_bare(&self, name: &str) -> Result<(), TmuxError> {
        self.run(&["switch-client", "-t", name]).map(drop)
    }

    // ----- introspection -------------------------------------------------------------------

    /// Expand a tmux format against `target` (or the calling client). `None` outside tmux / on an
    /// empty expansion.
    pub fn display_message(&self, fmt: &str, target: Option<&str>) -> Option<String> {
        let mut args = vec!["display-message"];
        if let Some(target) = target {
            args.extend(["-t", target]);
        }
        args.extend(["-p", fmt]);
        let (_, out) = self.run_quiet(&args);
        non_empty(out.trim())
    }

    /// The session this process runs inside; `None` outside tmux (gated on `$TMUX`, so a plain
    /// terminal never resolves the server's default session).
    pub fn current_session_name(&self) -> Option<String> {
        if !self.env.inside_tmux() {
            return None;
        }
        self.display_message("#S", None)
    }

    pub fn current_pane_id(&self) -> Option<String> {
        self.display_message("#{pane_id}", None)
    }

    pub fn current_pane_path(&self) -> Option<String> {
        self.display_message("#{pane_current_path}", None)
    }

    // ----- interactive attach ----------------------------------------------------------------

    /// Focus a window; false when the target is gone.
    pub fn select_window(&self, target: &str) -> bool {
        self.run_quiet(&["select-window", "-t", target]).0
    }

    /// Focus a pane within its window; false when the target is gone.
    pub fn select_pane(&self, target: &str) -> bool {
        self.run_quiet(&["select-pane", "-t", target]).0
    }

    /// Replace a pane's process with `command` (`respawn-pane -k`).
    pub fn respawn_pane(&self, pane_id: &str, command: &str) -> Result<(), TmuxError> {
        self.run(&["respawn-pane", "-k", "-t", pane_id, command])
            .map(drop)
    }

    /// Attach `name` in the foreground from outside tmux: real stdio, `$TMUX` removed. Returns
    /// the attach exit code (-1 when killed by a signal).
    pub fn attach_session(&self, name: &str) -> Result<i32, TmuxError> {
        let status = Command::new(&self.binary)
            .args(["attach", "-t", name])
            .env_remove("TMUX")
            .status()
            .map_err(|source| self.spawn_error(source))?;
        Ok(status.code().unwrap_or(-1))
    }

    /// A pane hosting `name` as `session:window.pane`, preferring a host in the comma-separated
    /// `prefer` set; the `@remote-session` pane is the fallback. `None` when no pane hosts it.
    pub fn pane_for_session(&self, name: &str, prefer: &str) -> Option<String> {
        let preferred: Vec<&str> = prefer.split(',').filter(|item| !item.is_empty()).collect();
        let locations = self.attached_to(name);
        let chosen = locations
            .iter()
            .find(|loc| preferred.contains(&loc.host.as_str()))
            .or_else(|| locations.first());
        match chosen {
            Some(loc) => Some(format!(
                "{}:{}.{}",
                loc.host, loc.window_index, loc.pane_index
            )),
            None => self.remote_pane_for_session(name),
        }
    }

    fn remote_pane_for_session(&self, name: &str) -> Option<String> {
        let (_, panes) = self.run_quiet(&[
            "list-panes",
            "-a",
            "-F",
            "#{@remote-session}\t#{session_name}\t#{window_index}\t#{pane_index}",
        ]);
        parse_remote_pane(&panes, name)
    }

    // ----- attachment topology ---------------------------------------------------------------

    fn attachment_join(&self) -> Vec<(String, Location)> {
        let (_, clients) =
            self.run_quiet(&["list-clients", "-F", "#{client_tty}\t#{client_session}"]);
        let (_, panes) = self.run_quiet(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_tty}\t#{session_name}\t#{window_index}\t#{window_name}\t#{pane_id}\t#{pane_index}",
        ]);
        parse_attachment_join(&clients, &panes)
    }

    /// inner-session-name → every pane surfacing it, each list in (host, window, pane) order.
    /// Entries keep first-seen order (the Python dict's insertion order).
    pub fn attachment_map(&self) -> Vec<(String, Vec<Location>)> {
        group_attachments(self.attachment_join())
    }

    /// Every pane surfacing `name`; empty when detached / bare-terminal attached.
    pub fn attached_to(&self, name: &str) -> Vec<Location> {
        self.attachment_map()
            .into_iter()
            .find(|(inner, _)| inner == name)
            .map(|(_, locations)| locations)
            .unwrap_or_default()
    }

    /// The inner session nest-attached in the active pane of the most recently active outer
    /// client's session. `None` for every "nothing is focused" case.
    pub fn focused_session_name(&self) -> Option<String> {
        let (_, clients) = self.run_quiet(&[
            "list-clients",
            "-F",
            "#{client_tty}\t#{client_session}\t#{client_activity}",
        ]);
        let (_, panes) = self.run_quiet(&[
            "list-panes",
            "-a",
            "-F",
            "#{pane_tty}\t#{session_name}\t#{window_active}\t#{pane_active}",
        ]);
        parse_focused_session(&clients, &panes)
    }

    /// Which inner session is nested in this pane (the join, inverted).
    pub fn inner_for_pane(&self, pane_id: &str) -> Option<String> {
        self.attachment_join()
            .into_iter()
            .find(|(_, location)| location.pane_id == pane_id)
            .map(|(inner, _)| inner)
    }

    /// The firing pane's location attributes for the tx-assistant envelope. `None` when the
    /// expansion is empty (outside tmux). A pane gone on tmux 3.4 still expands to empty fields
    /// (Q37 PARITY). An expansion that does not split into exactly seven fields (a `|` in a
    /// title) is `None` where the reference would crash.
    pub fn focus_attrs(&self, pane_id: &str) -> Option<EnvelopeAttrs> {
        let line = self.display_message(FOCUS_FORMAT, Some(pane_id))?;
        let mut attrs = parse_focus_line(&line, pane_id)?;
        if let Some(remote) = self.show_pane_option(pane_id, "@remote-session") {
            attrs.push(("inner-remote".to_owned(), "1".to_owned()));
            attrs.push(("inner-session-name".to_owned(), remote));
        } else if let Some(inner) = self.inner_for_pane(pane_id) {
            attrs.push(("inner-session-name".to_owned(), inner));
        }
        Some(attrs)
    }

    /// The `<tx-command-prompt …/>` envelope from pure-tmux fields; empty when the pane is gone.
    pub fn focus_envelope(&self, pane_id: &str) -> String {
        self.focus_attrs(pane_id)
            .map(|attrs| format_envelope(&attrs))
            .unwrap_or_default()
    }
}

const FOCUS_FORMAT: &str = "#{session_name}|#{window_index}|#{window_name}|#{pane_index}|#{pane_title}|#{pane_current_command}|#{pane_current_path}";

/// Render ordered attributes as a self-closing `<tx-command-prompt …/>` tag.
pub fn format_envelope<K: AsRef<str>, V: AsRef<str>>(attrs: &[(K, V)]) -> String {
    let mut out = String::from("<tx-command-prompt");
    for (key, value) in attrs {
        out.push(' ');
        out.push_str(key.as_ref());
        out.push_str("='");
        out.push_str(&xml_escape(value.as_ref()));
        out.push('\'');
    }
    out.push_str("/>");
    out
}

fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('\'', "&apos;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

/// Python `str.splitlines()` (no keepends).
fn py_splitlines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let Some((index, ch)) = rest.char_indices().find(|(_, ch)| is_line_break(*ch)) else {
            let line = rest;
            rest = "";
            return Some(line);
        };
        let line = &rest[..index];
        let mut next = index + ch.len_utf8();
        if ch == '\r' && rest[next..].starts_with('\n') {
            next += 1;
        }
        rest = &rest[next..];
        Some(line)
    })
}

fn is_line_break(ch: char) -> bool {
    matches!(
        ch,
        '\n' | '\r'
            | '\x0b'
            | '\x0c'
            | '\x1c'
            | '\x1d'
            | '\x1e'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// Python `str.zfill(width)`.
fn zfill(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len >= width {
        return value.to_owned();
    }
    let pad = "0".repeat(width - len);
    match value.strip_prefix(['+', '-']) {
        Some(digits) => format!("{}{pad}{digits}", &value[..1]),
        None => format!("{pad}{value}"),
    }
}

fn parse_remote_pane(panes: &str, name: &str) -> Option<String> {
    py_splitlines(panes).find_map(|line| {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            [remote, session_name, window_index, pane_index] if *remote == name => {
                Some(format!("{session_name}:{window_index}.{pane_index}"))
            }
            _ => None,
        }
    })
}

/// Insert-or-overwrite into an insertion-ordered association list (a small Python dict).
fn upsert<'a>(map: &mut Vec<(&'a str, &'a str)>, key: &'a str, value: &'a str) {
    match map.iter_mut().find(|(existing, _)| *existing == key) {
        Some(slot) => slot.1 = value,
        None => map.push((key, value)),
    }
}

fn lookup<'a>(map: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    map.iter()
        .find(|(existing, _)| *existing == key)
        .map(|(_, value)| *value)
}

fn parse_attachment_join(clients: &str, panes: &str) -> Vec<(String, Location)> {
    let mut session_by_tty = Vec::new();
    for line in py_splitlines(clients) {
        if let Some((client_tty, client_session)) = line.split_once('\t')
            && !client_tty.is_empty()
        {
            upsert(&mut session_by_tty, client_tty, client_session);
        }
    }
    let mut out = Vec::new();
    for line in py_splitlines(panes) {
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
        // Python: `if inner and inner != host` — the second guards the self-attach loop.
        if let Some(inner) = lookup(&session_by_tty, pane_tty)
            && !inner.is_empty()
            && inner != *host
        {
            out.push((
                inner.to_owned(),
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
                zfill(&loc.window_index, 8),
                zfill(&loc.pane_index, 8),
            )
        });
    }
    mapping
}

fn parse_focused_session(clients: &str, panes: &str) -> Option<String> {
    let mut session_by_tty = Vec::new();
    let mut clients_by_recency: Vec<(i64, &str, &str)> = Vec::new();
    for line in py_splitlines(clients) {
        let fields: Vec<&str> = line.split('\t').collect();
        let [client_tty, client_session, activity] = fields.as_slice() else {
            continue;
        };
        upsert(&mut session_by_tty, client_tty, client_session);
        // Python: `int(activity or 0)`; a non-numeric stamp (never emitted by tmux) counts as 0.
        let activity = activity.trim().parse().unwrap_or(0);
        clients_by_recency.push((activity, client_tty, client_session));
    }

    let mut pane_ttys: Vec<&str> = Vec::new();
    let mut active_pane_tty_by_host = Vec::new();
    for line in py_splitlines(panes) {
        let fields: Vec<&str> = line.split('\t').collect();
        let [pane_tty, host, window_active, pane_active] = fields.as_slice() else {
            continue;
        };
        pane_ttys.push(pane_tty);
        if *window_active == "1" && *pane_active == "1" {
            upsert(&mut active_pane_tty_by_host, host, pane_tty);
        }
    }

    // Python `max` keeps the FIRST maximal element.
    let mut focused: Option<(i64, &str)> = None;
    for (activity, client_tty, client_session) in clients_by_recency {
        if pane_ttys.contains(&client_tty) {
            continue;
        }
        if focused.is_none_or(|(best, _)| activity > best) {
            focused = Some((activity, client_session));
        }
    }
    let (_, focused_host) = focused?;
    let focused_pane_tty = lookup(&active_pane_tty_by_host, focused_host)?;
    lookup(&session_by_tty, focused_pane_tty).map(str::to_owned)
}

fn parse_focus_line(line: &str, pane_id: &str) -> Option<EnvelopeAttrs> {
    let fields: Vec<&str> = line.split('|').collect();
    let [
        session_name,
        window_index,
        window_name,
        pane_index,
        pane_title,
        pane_command,
        pane_path,
    ] = fields.as_slice()
    else {
        return None;
    };
    Some(
        [
            ("session-name", *session_name),
            ("window-index", *window_index),
            ("window-name", *window_name),
            ("pane-id", pane_id),
            ("pane-index", *pane_index),
            ("pane-title", *pane_title),
            ("pane-cmd", *pane_command),
            ("pane-path", *pane_path),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(host: &str, window_index: &str, pane_id: &str, pane_index: &str) -> Location {
        Location {
            host: host.into(),
            window_index: window_index.into(),
            window_name: "w".into(),
            pane_id: pane_id.into(),
            pane_index: pane_index.into(),
        }
    }

    #[test]
    fn switch_client_failure_names_the_target_form() {
        let tmux = Tmux::new("/usr/bin/false", TmuxEnv::default());
        let bare = tmux.switch_client_bare("Views").unwrap_err().to_string();
        assert_eq!(bare, "tmux switch-client -t Views failed: ");
        let exact = tmux.switch_client("Views").unwrap_err().to_string();
        assert_eq!(exact, "tmux switch-client -t =Views failed: ");
    }

    #[test]
    fn envelope_escapes_and_keeps_order() {
        // python: format_envelope({'b': "it's <b>&x", 'a': 'a"b'})
        let attrs = [("b", "it's <b>&x"), ("a", "a\"b")];
        assert_eq!(
            format_envelope(&attrs),
            "<tx-command-prompt b='it&apos;s &lt;b&gt;&amp;x' a='a\"b'/>"
        );
        assert_eq!(format_envelope::<&str, &str>(&[]), "<tx-command-prompt/>");
    }

    #[test]
    fn splitlines_matches_python() {
        // python: 'a\r\nb\rc\x0bd\n\ne f\n'.splitlines()
        let lines: Vec<&str> = py_splitlines("a\r\nb\rc\x0bd\n\ne\u{2028}f\n").collect();
        assert_eq!(lines, ["a", "b", "c", "d", "", "e", "f"]);
        assert_eq!(py_splitlines("").count(), 0);
        assert_eq!(py_splitlines("\n").collect::<Vec<_>>(), [""]);
    }

    #[test]
    fn zfill_matches_python() {
        assert_eq!(zfill("3", 8), "00000003");
        assert_eq!(zfill("-3", 4), "-003");
        assert_eq!(zfill("123456789", 8), "123456789");
        assert_eq!(zfill("", 2), "00");
    }

    #[test]
    fn join_skips_self_attach_and_bad_lines() {
        let clients = "/dev/ttys1\tinner\n/dev/ttys2\tV\nnotab\n\tempty\n/dev/ttys3\t\n";
        let panes = "/dev/ttys1\tV\t0\tw\t%1\t0\n\
                     /dev/ttys2\tV\t0\tw\t%2\t1\n\
                     /dev/ttys3\tV\t0\tw\t%3\t2\n\
                     short\tline\n";
        let join = parse_attachment_join(clients, panes);
        assert_eq!(join, vec![("inner".to_owned(), loc("V", "0", "%1", "0"))]);
    }

    #[test]
    fn map_sorts_numerically() {
        let join = vec![
            ("s".to_owned(), loc("B", "0", "%1", "0")),
            ("s".to_owned(), loc("A", "10", "%2", "0")),
            ("s".to_owned(), loc("A", "9", "%3", "2")),
            ("t".to_owned(), loc("A", "0", "%4", "0")),
        ];
        let map = group_attachments(join);
        let order: Vec<&str> = map[0].1.iter().map(|l| l.pane_id.as_str()).collect();
        assert_eq!(map[0].0, "s");
        assert_eq!(order, ["%3", "%2", "%1"]);
        assert_eq!(map[1].0, "t");
    }

    #[test]
    fn remote_pane_lookup() {
        let panes = "\tV\t0\t0\nhost1\tV\t1\t2\nhost1\tW\t0\t0\n";
        assert_eq!(parse_remote_pane(panes, "host1").as_deref(), Some("V:1.2"));
        assert_eq!(parse_remote_pane(panes, "nope"), None);
    }

    #[test]
    fn focused_session_picks_first_most_recent_outer_client() {
        // outer clients /dev/o1 (V, 5) and /dev/o2 (W, 5): the first maximum wins → V → inner.
        let clients = "/dev/o1\tV\t5\n/dev/o2\tW\t5\n/dev/p1\tinner\t9\n/dev/p2\tother\t1\n";
        let panes = "/dev/p1\tV\t1\t1\n/dev/p2\tW\t1\t1\n/dev/x\tV\t0\t1\n";
        assert_eq!(
            parse_focused_session(clients, panes).as_deref(),
            Some("inner")
        );
        assert_eq!(parse_focused_session("/dev/p1\tinner\t9\n", panes), None);
        assert_eq!(parse_focused_session("/dev/o\tZ\t1\n", panes), None);
        assert_eq!(
            parse_focused_session("/dev/o\tV\t1\n", "/dev/q\tV\t1\t1\n"),
            None
        );
    }

    #[test]
    fn focus_line_parsing() {
        let attrs = parse_focus_line("V|0|w0|1|t|tmux|/tmp", "%5").expect("seven fields");
        assert_eq!(
            format_envelope(&attrs),
            "<tx-command-prompt session-name='V' window-index='0' window-name='w0' pane-id='%5' \
             pane-index='1' pane-title='t' pane-cmd='tmux' pane-path='/tmp'/>"
        );
        // Q37: a gone pane on tmux 3.4 expands to empty fields.
        assert!(parse_focus_line("||||||", "%9").is_some());
        assert!(parse_focus_line("a|b", "%9").is_none());
    }

    #[test]
    fn error_text() {
        let err = TmuxError::Failed {
            args: "set-option -t =d: @tx_id x".into(),
            stderr: "no such session: d".into(),
        };
        assert_eq!(
            err.to_string(),
            "tmux set-option -t =d: @tx_id x failed: no such session: d"
        );
    }

    /// A private tmux server behind a wrapper script (the kit's shape); killed on drop.
    struct Server {
        _dir: tempfile::TempDir,
        wrapper: std::path::PathBuf,
    }

    impl Server {
        fn start() -> Option<Self> {
            use std::os::unix::fs::PermissionsExt;
            Command::new("tmux").arg("-V").output().ok()?;
            let dir = tempfile::Builder::new()
                .prefix("txt")
                .tempdir_in("/tmp")
                .ok()?;
            let wrapper = dir.path().join("tmux");
            let script = format!(
                "#!/bin/sh\nTMUX_TMPDIR='{}' exec tmux -L t -f /dev/null \"$@\"\n",
                dir.path().display()
            );
            std::fs::write(&wrapper, script).ok()?;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).ok()?;
            Some(Self { _dir: dir, wrapper })
        }

        fn tmux(&self) -> Tmux {
            Tmux::new(&self.wrapper, TmuxEnv::default())
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = Command::new(&self.wrapper).arg("kill-server").output();
        }
    }

    #[test]
    fn adapter_against_private_server() {
        let Some(server) = Server::start() else {
            eprintln!("tmux not available; skipping");
            return;
        };
        let tmux = server.tmux();
        assert!(tmux.list_sessions("#{session_name}").is_empty());
        assert!(!tmux.has_session("work"));

        let uuid = "b0000000-0000-4000-8000-000000000001";
        let pid = tmux
            .new_session(
                uuid,
                "/tmp",
                "sleep 60",
                &[("TERM".into(), "screen".into())],
            )
            .expect("new-session");
        assert!(pid > 0);
        tmux.set_tx_id(uuid, uuid).expect("stamp");
        tmux.new_session("work2", "/tmp", "sleep 60", &[])
            .expect("new-session");

        // Q27: neither a name prefix nor a hex prefix resolves another session.
        assert!(!tmux.has_session("work"));
        assert_eq!(tmux.get_tx_id("b"), None);
        assert_eq!(tmux.get_tx_id(uuid).as_deref(), Some(uuid));
        assert!(tmux.set_tx_view("wor").is_err());
        assert!(!tmux.kill_session("wor"));
        tmux.set_tx_view("work2").expect("view");
        assert!(tmux.is_view("work2"));
        assert!(!tmux.is_view(uuid));

        let mut names = tmux.list_sessions("#{session_name}");
        names.sort();
        assert_eq!(names, [uuid, "work2"]);

        let pane = tmux
            .display_message("#{pane_id}", Some("work2"))
            .expect("pane id");
        assert!(pane.starts_with('%'));
        assert_eq!(tmux.current_session_name(), None);

        // Q30: the remote marker is read at pane scope.
        tmux.set_option(&pane, "@remote-session", "host1", true)
            .expect("pane option");
        let envelope = tmux.focus_envelope(&pane);
        assert!(
            envelope.starts_with("<tx-command-prompt session-name='work2' "),
            "{envelope}"
        );
        assert!(
            envelope.ends_with("inner-remote='1' inner-session-name='host1'/>"),
            "{envelope}"
        );
        assert_eq!(
            tmux.pane_for_session("host1", "").as_deref(),
            Some("work2:0.0")
        );
        tmux.unset_option(&pane, "@remote-session", true);
        assert!(!tmux.focus_envelope(&pane).contains("inner-"));
        assert_eq!(tmux.pane_for_session("host1", ""), None);

        let err = tmux
            .send_keys("nosuch", "x", true)
            .expect_err("missing target");
        assert!(
            err.to_string()
                .starts_with("tmux send-keys -t nosuch -l -- x failed: "),
            "{err}"
        );

        tmux.rename_session("work2", "work3").expect("rename");
        assert!(tmux.has_session("work3"));
        assert!(tmux.kill_session("work3"));
        assert!(!tmux.has_session("work3"));
    }
}
