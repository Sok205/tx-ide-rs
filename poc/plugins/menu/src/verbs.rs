//! The verbs. `_menu-open` (prefix+m) shows `_menu-tui` in a popup on the firing client and
//! waits for it; once the popup has closed (tmux opens no second popup over a first) it runs
//! the chosen action. `_menu-first` (the client-attached hook) opens it once per server. Every
//! action is another plugin's `txc` verb or a tmux command (D3).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use tx_client::tmux::{Ui, shell_quote};
use tx_core::{TxCore, Verb, VerbError, VerbIo};

use crate::data;
use crate::model::{Action, Menu};
use crate::run;

pub const OPEN_VERB: &str = "_menu-open";
pub const FIRST_VERB: &str = "_menu-first";
pub const TUI_VERB: &str = "_menu-tui";
/// Server option: the start menu has been shown on this server.
pub const SHOWN_OPTION: &str = "@tx-menu-shown";
const STATUS_MILLISECONDS: &str = "4000";

pub fn verbs() -> Vec<Arc<dyn Verb>> {
    vec![
        Arc::new(MenuVerb),
        Arc::new(Open),
        Arc::new(First),
        Arc::new(Tui),
    ]
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

fn usage(io: &mut VerbIo<'_>, text: &str) -> Result<i32, VerbError> {
    let _ = writeln!(io.stderr, "usage: txc {text}");
    Ok(2)
}

/// `menu`: open the start menu on the terminal this runs in.
struct MenuVerb;

impl Verb for MenuVerb {
    fn name(&self) -> &'static str {
        "menu"
    }

    fn usage(&self) -> &'static str {
        "menu   the tx-ide start menu (prefix+m)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        if !argv.is_empty() {
            return usage(io, "menu");
        }
        let ui = Ui::from_core(core);
        let here = ui.inside().then(|| {
            (
                ui.current_pane_id(),
                ui.display_message("#{client_name}", None),
            )
        });
        let Some((Some(pane), Some(client))) = here else {
            let _ = writeln!(
                io.stderr,
                "txc menu: run it inside tx-ide (`txc ui attach`), or press prefix+m there"
            );
            return Ok(1);
        };
        open(core, &ui, &pane, &client, false)
    }
}

/// `_menu-open <pane> <client>`: prefix+m.
struct Open;

impl Verb for Open {
    fn name(&self) -> &'static str {
        OPEN_VERB
    }

    fn usage(&self) -> &'static str {
        "_menu-open <pane> <client>   the start menu in a popup, then its choice (prefix+m, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane, client] = argv else {
            return usage(io, "_menu-open <pane> <client>");
        };
        let ui = Ui::from_core(core);
        reported(&ui, client, open(core, &ui, pane, client, false))
    }
}

/// `_menu-first <pane> <client>`: the client-attached hook.
struct First;

impl Verb for First {
    fn name(&self) -> &'static str {
        FIRST_VERB
    }

    fn usage(&self) -> &'static str {
        "_menu-first <pane> <client>   the start menu, once per server (client-attached hook, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane, client] = argv else {
            return usage(io, "_menu-first <pane> <client>");
        };
        let ui = Ui::from_core(core);
        if is_nested(&ui, client) || !claim_first(&ui) {
            return Ok(0);
        }
        reported(&ui, client, open(core, &ui, pane, client, true))
    }
}

/// `_menu-tui <pane> <client> <choice-file> [--first]`: the menu itself, inside the popup.
/// `--first`: opened by the first attach, so it starts with the grace period.
struct Tui;

impl Verb for Tui {
    fn name(&self) -> &'static str {
        TUI_VERB
    }

    fn usage(&self) -> &'static str {
        "_menu-tui <pane> <client> <file> [--first]   the menu in the popup; writes the choice (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let (pane, file, first) = match argv {
            [pane, _client, file] => (pane, file, false),
            [pane, _client, file, flag] if flag == "--first" => (pane, file, true),
            _ => return usage(io, "_menu-tui <pane> <client> <file> [--first]"),
        };
        let ui = Ui::from_core(core);
        let mut menu = Menu::new(data::context(core, &ui, pane)?);
        if let Some(action) = run::run(&mut menu, first).map_err(failed)? {
            std::fs::write(file, action.encode()).map_err(|e| failed(format!("{file}: {e}")))?;
        }
        Ok(0)
    }
}

/// The menu's own failure, on the client's status line: the hook and the bind swallow output.
fn reported(ui: &Ui, client: &str, result: Result<i32, VerbError>) -> Result<i32, VerbError> {
    if let Err(error) = &result {
        status(ui, client, &format!("tx menu: {error}"));
    }
    result
}

/// Whether `client` should not count as a first attach: it is nested (its tty is one of this
/// server's panes: the attach inside a view), or it is already gone.
fn skips_first(clients: &str, pane_ttys: &str, client: &str) -> bool {
    let Some(tty) = clients.lines().find_map(|line| {
        line.split_once('\t')
            .filter(|(name, _)| *name == client)
            .map(|(_, tty)| tty)
    }) else {
        return true;
    };
    pane_ttys.lines().any(|pane_tty| pane_tty == tty)
}

fn is_nested(ui: &Ui, client: &str) -> bool {
    let (_, clients) = ui.quiet(&["list-clients", "-F", "#{client_name}\t#{client_tty}"]);
    let (_, ttys) = ui.quiet(&["list-panes", "-a", "-F", "#{pane_tty}"]);
    skips_first(&clients, &ttys, client)
}

/// Test-and-set of [`SHOWN_OPTION`] in one tmux command, so two first clients cannot both win.
fn claim_first(ui: &Ui) -> bool {
    let (ok, out) = ui.quiet(&[
        "if-shell",
        "-F",
        &format!("#{{{SHOWN_OPTION}}}"),
        "display-message -p taken",
        &format!("set-option -g {SHOWN_OPTION} 1 ; display-message -p mine"),
    ]);
    ok && out.trim() == "mine"
}

/// How to run this `txc` against this core.
struct Txc {
    exe: PathBuf,
    /// The core's `cli_arguments()`.
    arguments: Vec<String>,
}

impl Txc {
    fn current(core: &TxCore) -> Result<Self, VerbError> {
        Ok(Txc {
            exe: std::env::current_exe().map_err(|e| failed(format!("current_exe: {e}")))?,
            arguments: core.config().cli_arguments(),
        })
    }

    /// A shell command line, for popups.
    fn shell(&self, args: &[&str]) -> String {
        let mut words = vec![shell_quote(&self.exe.to_string_lossy())];
        words.extend(self.arguments.iter().map(|word| shell_quote(word)));
        words.extend(args.iter().map(|arg| shell_quote(arg)));
        words.join(" ")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.exe);
        command
            .args(&self.arguments)
            .args(args)
            .stdin(Stdio::null());
        command
    }
}

fn open(core: &TxCore, ui: &Ui, pane: &str, client: &str, first: bool) -> Result<i32, VerbError> {
    let txc = Txc::current(core)?;
    let choice = tempfile::Builder::new()
        .prefix("tx-menu-")
        .tempfile()
        .map_err(|e| failed(format!("choice file: {e}")))?
        .into_temp_path();
    let choice_path = choice.to_string_lossy();
    let mut args = vec![TUI_VERB, pane, client, &choice_path];
    if first {
        args.push("--first");
    }
    let command = txc.shell(&args);
    // -EE: a menu that fails keeps its popup open, showing why.
    ui.run(&[
        "display-popup",
        "-c",
        client,
        "-EE",
        "-w",
        "80%",
        "-h",
        "80%",
        "-x",
        "C",
        "-y",
        "C",
        "-b",
        "rounded",
        "-S",
        "fg=#7aa2f7",
        "-T",
        " tx-ide ",
        &command,
    ])
    .map_err(failed)?;
    let text = std::fs::read_to_string(&choice).unwrap_or_default();
    if let Some(action) = Action::decode(&text) {
        perform(ui, &txc, pane, client, &action);
    }
    Ok(0)
}

fn status(ui: &Ui, client: &str, message: &str) {
    ui.quiet(&[
        "display-message",
        "-c",
        client,
        "-d",
        STATUS_MILLISECONDS,
        message,
    ]);
}

struct Popup<'a> {
    width: &'a str,
    height: &'a str,
    y: &'a str,
    title: &'a str,
    /// The form popups (new agent, fork) wear the spawn / chat binds' heavy yellow frame.
    form: bool,
}

fn popup(ui: &Ui, client: &str, spec: &Popup<'_>, command: &str) {
    let mut args = vec![
        "display-popup",
        "-c",
        client,
        "-E",
        "-w",
        spec.width,
        "-h",
        spec.height,
        "-x",
        "C",
        "-y",
        spec.y,
    ];
    if spec.form {
        args.extend(["-b", "heavy", "-S", "fg=yellow,bold"]);
    }
    args.extend(["-T", spec.title, command]);
    if let Err(error) = ui.run(&args) {
        status(ui, client, &format!("tx menu: {error}"));
    }
}

/// Runs `command`; `Ok(stdout)` on success, else the last stderr line.
fn output(mut command: Command) -> Result<String, String> {
    match command.output() {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            Err(stderr
                .lines()
                .rfind(|line| !line.trim().is_empty())
                .unwrap_or("failed")
                .to_owned())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn perform(ui: &Ui, txc: &Txc, pane: &str, client: &str, action: &Action) {
    let wide = |title| Popup {
        width: "121",
        height: "30",
        y: "1",
        title,
        form: false,
    };
    match action {
        Action::NewSession => popup(
            ui,
            client,
            &Popup {
                width: "94",
                height: "23",
                y: "C",
                title: " New agent ",
                form: true,
            },
            &txc.shell(&["_new-agent", pane, client]),
        ),
        Action::Attach => popup(ui, client, &wide(" tx "), &txc.shell(&["pick"])),
        Action::Fork => popup(
            ui,
            client,
            &Popup {
                width: "80",
                height: "4",
                y: "C",
                title: " Fork session ",
                form: true,
            },
            &txc.shell(&["_fork-pane", pane, client]),
        ),
        Action::Timeline => popup(
            ui,
            client,
            &wide(" timeline "),
            &txc.shell(&["_timeline-attach"]),
        ),
        Action::Artifacts => match artifact_target(ui, pane, client) {
            Some((session, nested)) => {
                if let Err(error) = output(txc.command(&["_artifact-menu", &session, &nested])) {
                    status(ui, client, &format!("artifacts: {error}"));
                }
            }
            None => status(ui, client, "artifacts: no session in this pane"),
        },
        Action::Tutor => {
            let mut command = txc.command(&["tutor", "start", "--detach"]);
            command.env_remove("TMUX");
            match output(command) {
                Ok(out) => match tutor_view(&out) {
                    Some(view) => switch(ui, client, view),
                    None => status(ui, client, out.lines().last().unwrap_or("tutor started")),
                },
                Err(error) => status(ui, client, &format!("tutor: {error}")),
            }
        }
        Action::Assistant => match output(txc.command(&["assistant", "warm"])) {
            Ok(name) => switch(ui, client, name.trim()),
            Err(error) => status(ui, client, &format!("assistant: {error}")),
        },
        Action::Resume(id) => match output(txc.command(&["respawn", id])) {
            Ok(out) => match respawned_name(&out) {
                Some(name) => {
                    status(ui, client, &format!("respawned '{name}'"));
                    show(ui, pane, client, name);
                }
                None => status(ui, client, "respawned"),
            },
            Err(error) => status(ui, client, &format!("respawn: {error}")),
        },
    }
}

fn switch(ui: &Ui, client: &str, session: &str) {
    if let Err(error) = ui.run(&["switch-client", "-c", client, "-t", &format!("={session}")]) {
        status(ui, client, &format!("tx menu: {error}"));
    }
}

/// `respawn` prints `<id>\t<name>`: the agent comes back in place, under its own name.
fn respawned_name(stdout: &str) -> Option<&str> {
    let (_, name) = stdout.lines().next()?.split_once('\t')?;
    (!name.is_empty()).then_some(name)
}

/// Foreground commands of a pane the picker's Enter would replace by a nested attach.
const HOSTING_COMMANDS: [&str; 6] = ["zsh", "bash", "sh", "fish", "dash", "tmux"];

/// The picker's Enter: nest into the firing pane when it is the shell (or nested client) of a
/// view that hosts nested attaches (the home base, the tutor).
fn nests_here(pane_hosts: bool, pane_command: Option<&str>) -> bool {
    pane_hosts && pane_command.is_some_and(|command| HOSTING_COMMANDS.contains(&command))
}

/// Shows session `name` the way the picker's Enter does: nested into the firing pane, else
/// the client switched to it.
fn show(ui: &Ui, pane: &str, client: &str, name: &str) {
    let hosts = ui
        .display_message("#{session_name}", Some(pane))
        .is_some_and(|session| ui.hosts_nested_attaches(&session));
    let command = ui.display_message("#{pane_current_command}", Some(pane));
    match ui.live_targets().get(name) {
        Some(target) if nests_here(hosts, command.as_deref()) => {
            if let Err(error) = ui.respawn_pane(pane, &ui.nest_attach_command(target)) {
                status(ui, client, &format!("tx menu: {error}"));
            }
        }
        _ => switch(ui, client, name),
    }
}

/// `tutor start --detach` ends with `<view> is ready: tmux … attach -t =<view>`.
fn tutor_view(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .find_map(|line| line.split_once(" is ready: ").map(|(view, _)| view.trim()))
        .filter(|view| !view.is_empty())
}

/// What prefix+a would see in `pane`: the nested session and the nested client drawing it, or
/// the pane's own session and the firing client.
fn artifact_target(ui: &Ui, pane: &str, client: &str) -> Option<(String, String)> {
    if let Some(inner) = ui.inner_for_pane(pane) {
        let tty = ui.display_message("#{pane_tty}", Some(pane))?;
        let (_, clients) = ui.quiet(&["list-clients", "-F", "#{client_tty}\t#{client_name}"]);
        let nested = clients.lines().find_map(|line| {
            line.split_once('\t')
                .filter(|(client_tty, _)| *client_tty == tty)
                .map(|(_, name)| name.to_owned())
        })?;
        return Some((inner, nested));
    }
    let session = ui.display_message("#{session_name}", Some(pane))?;
    Some((session, client.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tutor_view_is_read_from_the_ready_line() {
        let out = "Scaffolded /h/tx-tutor/rust\ntutor-rust view created.\n\
                   tutor-rust is ready: tmux -S '/s/tmux.sock' attach -t =tutor-rust\n";
        assert_eq!(tutor_view(out), Some("tutor-rust"));
        assert_eq!(tutor_view("tutor-rust view created.\n"), None);
    }

    #[test]
    fn a_gone_or_nested_client_is_no_first_attach() {
        let clients = "/dev/ttys001\t/dev/ttys001\n/dev/ttys009\t/dev/ttys009\n";
        let panes = "/dev/ttys009\n/dev/ttys010\n";
        assert!(!skips_first(clients, panes, "/dev/ttys001"));
        assert!(skips_first(clients, panes, "/dev/ttys009"), "nested");
        assert!(skips_first(clients, panes, "/dev/ttys002"), "gone");
        assert!(skips_first("", "", "/dev/ttys001"), "no server");
    }

    #[test]
    fn the_respawned_name_is_read_from_respawns_line() {
        assert_eq!(respawned_name("0f3a-id\tfixer\n"), Some("fixer"));
        assert_eq!(respawned_name("0f3a-id\t\n"), None);
        assert_eq!(respawned_name("something else\n"), None);
    }

    #[test]
    fn the_resumed_session_nests_only_into_a_views_shell() {
        assert!(nests_here(true, Some("zsh")));
        assert!(nests_here(true, Some("tmux")));
        assert!(!nests_here(true, Some("nvim")));
        assert!(!nests_here(false, Some("zsh")));
        assert!(!nests_here(true, None));
    }

    #[test]
    fn popup_commands_are_shell_quoted() {
        let txc = Txc {
            exe: PathBuf::from("/opt/txc"),
            arguments: vec!["--state-dir".to_owned(), "/s/state".to_owned()],
        };
        assert_eq!(
            txc.shell(&["_new-agent", "%3", "/dev/ttys001"]),
            "'/opt/txc' '--state-dir' '/s/state' '_new-agent' '%3' '/dev/ttys001'"
        );
    }
}
