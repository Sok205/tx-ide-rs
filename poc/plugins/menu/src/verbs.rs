//! The verbs. `_menu-open` (prefix+m) shows `_menu-tui` in a popup on the firing client and
//! waits for it; once the popup has closed (tmux opens no second popup over a first) it runs
//! the chosen action. `_menu-first` (the client-attached hook) opens it once per server. Every
//! action is another plugin's `txc` verb or a tmux command (D3).

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;

use tx_core::{TxCore, Verb, VerbError, VerbIo};
use tx_ui::tmux::{Ui, shell_quote};

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
        open(core, &ui, &pane, &client)
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
        open(core, &Ui::from_core(core), pane, client)
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
        open(core, &ui, pane, client)
    }
}

/// `_menu-tui <pane> <client> <choice-file>`: the menu itself, inside the popup.
struct Tui;

impl Verb for Tui {
    fn name(&self) -> &'static str {
        TUI_VERB
    }

    fn usage(&self) -> &'static str {
        "_menu-tui <pane> <client> <file>   the menu in the popup; writes the choice (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane, _client, file] = argv else {
            return usage(io, "_menu-tui <pane> <client> <file>");
        };
        let ui = Ui::from_core(core);
        let mut menu = Menu::new(data::context(core, &ui, pane)?);
        if let Some(action) = run::run(&mut menu).map_err(failed)? {
            std::fs::write(file, action.encode()).map_err(|e| failed(format!("{file}: {e}")))?;
        }
        Ok(0)
    }
}

/// A client whose tty is one of this server's panes: the nested attach inside a view.
fn is_nested(ui: &Ui, client: &str) -> bool {
    let (_, clients) = ui.quiet(&["list-clients", "-F", "#{client_name}\t#{client_tty}"]);
    let Some(tty) = clients.lines().find_map(|line| {
        line.split_once('\t')
            .filter(|(name, _)| *name == client)
            .map(|(_, tty)| tty.to_owned())
    }) else {
        return false;
    };
    let (_, ttys) = ui.quiet(&["list-panes", "-a", "-F", "#{pane_tty}"]);
    ttys.lines().any(|pane_tty| pane_tty == tty)
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

/// How to run this `txc` against this state dir.
struct Txc {
    exe: PathBuf,
    state: PathBuf,
}

impl Txc {
    fn current(core: &TxCore) -> Result<Self, VerbError> {
        Ok(Txc {
            exe: std::env::current_exe().map_err(|e| failed(format!("current_exe: {e}")))?,
            state: core.config().state_dir.clone(),
        })
    }

    /// A shell command line, for popups.
    fn shell(&self, args: &[&str]) -> String {
        let mut words = vec![
            shell_quote(&self.exe.to_string_lossy()),
            "--state-dir".to_owned(),
            shell_quote(&self.state.to_string_lossy()),
        ];
        words.extend(args.iter().map(|arg| shell_quote(arg)));
        words.join(" ")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(&self.exe);
        command
            .arg("--state-dir")
            .arg(&self.state)
            .args(args)
            .stdin(Stdio::null());
        command
    }
}

fn open(core: &TxCore, ui: &Ui, pane: &str, client: &str) -> Result<i32, VerbError> {
    let txc = Txc::current(core)?;
    let choice = tempfile::Builder::new()
        .prefix("tx-menu-")
        .tempfile()
        .map_err(|e| failed(format!("choice file: {e}")))?
        .into_temp_path();
    let command = txc.shell(&[TUI_VERB, pane, client, &choice.to_string_lossy()]);
    ui.run(&[
        "display-popup",
        "-c",
        client,
        "-E",
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
        width: "118",
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
        Action::Resume(id) => {
            let message = match output(txc.command(&["resume", id])) {
                Ok(out) => out.lines().next().unwrap_or("resumed").to_owned(),
                Err(error) => error,
            };
            status(ui, client, &message);
        }
    }
}

fn switch(ui: &Ui, client: &str, session: &str) {
    if let Err(error) = ui.run(&["switch-client", "-c", client, "-t", &format!("={session}")]) {
        status(ui, client, &format!("tx menu: {error}"));
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
    fn popup_commands_are_shell_quoted() {
        let txc = Txc {
            exe: PathBuf::from("/opt/txc"),
            state: PathBuf::from("/s/state"),
        };
        assert_eq!(
            txc.shell(&["_new-agent", "%3", "/dev/ttys001"]),
            "'/opt/txc' --state-dir '/s/state' '_new-agent' '%3' '/dev/ttys001'"
        );
    }
}
