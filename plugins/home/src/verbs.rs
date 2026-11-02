//! The verbs. The floor: `_home-install <session>` puts the TUI into the home view's pane and
//! keeps it there (see [`floor`]); `_home-tui` is the TUI, which leaves its choice in a file and
//! exits; `_home-perform <pane>` (the `pane-died` hook) acts on that choice and gives the pane its
//! next program: the nested attach of the chosen session, else the TUI again. `_home-open`
//! (prefix+m) shows the TUI in a popup on the firing client and runs its choice once the popup
//! has closed (tmux opens no second popup over a first); `_home-first` (the client-attached
//! hook) opens it once per server. Every action is another plugin's `tx` verb or a tmux command
//! (D3).

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use tx_client::tmux::{Ui, shell_quote};
use tx_core::{TxCore, Verb, VerbError, VerbIo};

use crate::data;
use crate::floor;
use crate::model::{Action, AssistantState, Context, Home};
use crate::run::{self, Options};

pub const OPEN_VERB: &str = "_home-open";
pub const FIRST_VERB: &str = "_home-first";
pub const TUI_VERB: &str = "_home-tui";
pub const INSTALL_VERB: &str = "_home-install";
pub const PERFORM_VERB: &str = "_home-perform";
/// Server option: the start menu has been shown on this server.
pub const SHOWN_OPTION: &str = "@tx-home-shown";
const STATUS_MILLISECONDS: &str = "4000";
const ASSISTANT_VERB: &str = "assistant";

pub fn verbs() -> Vec<Arc<dyn Verb>> {
    vec![
        Arc::new(HomeVerb),
        Arc::new(Open),
        Arc::new(First),
        Arc::new(Tui),
        Arc::new(Install),
        Arc::new(Perform),
    ]
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

fn usage(io: &mut VerbIo<'_>, text: &str) -> Result<i32, VerbError> {
    let _ = writeln!(io.stderr, "usage: tx {text}");
    Ok(2)
}

/// `<data>/plugins/home`: the choice and last-session files.
fn plugin_directory(core: &TxCore) -> PathBuf {
    core.config().plugin_data_dir(crate::PLUGIN_ID)
}

/// `home`: open the start menu on the terminal this runs in.
struct HomeVerb;

impl Verb for HomeVerb {
    fn name(&self) -> &'static str {
        "home"
    }

    fn usage(&self) -> &'static str {
        "home   the tx-ide start menu (prefix+m)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        if !argv.is_empty() {
            return usage(io, "home");
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
                "tx home: run it inside tx-ide (`tx ui attach`), or press prefix+m there"
            );
            return Ok(1);
        };
        open(core, &ui, &pane, &client, false)
    }
}

/// `_home-open <pane> <client>`: prefix+m.
struct Open;

impl Verb for Open {
    fn name(&self) -> &'static str {
        OPEN_VERB
    }

    fn usage(&self) -> &'static str {
        "_home-open <pane> <client>   the home in a popup, then its choice (prefix+m, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane, client] = argv else {
            return usage(io, "_home-open <pane> <client>");
        };
        let ui = Ui::from_core(core);
        reported(&ui, client, open(core, &ui, pane, client, false))
    }
}

/// `_home-first <pane> <client>`: the client-attached hook.
struct First;

impl Verb for First {
    fn name(&self) -> &'static str {
        FIRST_VERB
    }

    fn usage(&self) -> &'static str {
        "_home-first <pane> <client>   the start menu, once per server (client-attached hook, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane, client] = argv else {
            return usage(io, "_home-first <pane> <client>");
        };
        let ui = Ui::from_core(core);
        if is_nested(&ui, client) || !claim_first(&ui) {
            return Ok(0);
        }
        reported(&ui, client, open(core, &ui, pane, client, true))
    }
}

const TUI_USAGE: &str =
    "_home-tui <pane> [--first] [--returned-from SESSION] [--popup] [--choice FILE]";

#[derive(Debug, PartialEq)]
struct TuiArguments {
    pane: String,
    first: bool,
    popup: bool,
    returned_from: Option<String>,
    choice: Option<PathBuf>,
}

fn tui_arguments(argv: &[String]) -> Option<TuiArguments> {
    let (pane, flags) = argv.split_first()?;
    let mut arguments = TuiArguments {
        pane: pane.clone(),
        first: false,
        popup: false,
        returned_from: None,
        choice: None,
    };
    let mut flags = flags.iter();
    while let Some(flag) = flags.next() {
        match flag.as_str() {
            "--first" => arguments.first = true,
            "--popup" => arguments.popup = true,
            "--returned-from" => arguments.returned_from = Some(flags.next()?.clone()),
            "--choice" => arguments.choice = Some(PathBuf::from(flags.next()?)),
            _ => return None,
        }
    }
    Some(arguments)
}

/// `_home-tui <pane> …`: the home itself, on the terminal it runs in (the home pane, or the
/// prefix+m popup). Leaves the chosen action in the choice file, or no file when nothing was
/// chosen. `--first`: opened by an attach, so it starts with the grace period and, on the floor,
/// starts the assistant when there is none.
struct Tui;

impl Verb for Tui {
    fn name(&self) -> &'static str {
        TUI_VERB
    }

    fn usage(&self) -> &'static str {
        "_home-tui <pane> [--first] [--returned-from SESSION] [--popup] [--choice FILE]   the home TUI; writes the choice (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some(arguments) = tui_arguments(argv) else {
            return usage(io, TUI_USAGE);
        };
        let choice = arguments
            .choice
            .clone()
            .unwrap_or_else(|| floor::choice_path(&plugin_directory(core), &arguments.pane));
        if arguments.first && !arguments.popup {
            warm_when_absent(core, io)?;
        }
        let mut home = if arguments.popup {
            Home::new_popup(Context::default())
        } else {
            Home::new(Context::default())
        };
        let options = Options {
            first: arguments.first,
            popup: arguments.popup,
            returned_from: arguments.returned_from,
        };
        let chosen = run::run(core, &mut home, &options)
            .map_err(|error| error.to_string())
            .and_then(|action| record_choice(&choice, action.as_ref()));
        let Err(error) = chosen else {
            return Ok(0);
        };
        let _ = writeln!(io.stderr, "tx home: {error}");
        if let Err(error) = record_choice(&choice, None) {
            let _ = writeln!(io.stderr, "tx home: {error}");
        }
        // On the floor the pane would die and the TUI come straight back: the operator reads
        // why first. The popup stays open on a failure by itself.
        if !arguments.popup {
            let _ = writeln!(io.stderr, "press Enter to start the home again");
            let _ = BufReader::new(&mut io.stdin).read_line(&mut String::new());
        }
        Ok(1)
    }
}

/// `tx assistant warm`, detached and forgotten, when the assistant plugin is on and has no
/// assistant at all (a stopped one waits for `r`).
fn warm_when_absent(core: &TxCore, io: &mut VerbIo<'_>) -> Result<(), VerbError> {
    if core.verb(ASSISTANT_VERB).is_none() || data::assistant_state(core) != AssistantState::Absent
    {
        return Ok(());
    }
    let tx = Txc::current(core)?;
    let mut warm = tx.command(&[ASSISTANT_VERB, "warm"]);
    warm.stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    // Its outcome shows in the header: the record, or the failure line `warm` leaves.
    if let Err(error) = warm.spawn() {
        let _ = writeln!(io.stderr, "tx home: assistant warm: {error}");
    }
    Ok(())
}

/// `file` holds `action`, or is gone when there is none.
fn record_choice(file: &Path, action: Option<&Action>) -> Result<(), String> {
    let describe = |error: std::io::Error| format!("{}: {error}", file.display());
    match action {
        Some(action) => {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent).map_err(describe)?;
            }
            std::fs::write(file, action.encode()).map_err(describe)
        }
        None => match std::fs::remove_file(file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(describe(error)),
            _ => Ok(()),
        },
    }
}

/// The contents of `file`, which is gone afterwards; `None` when there was none.
fn take(file: &Path) -> Result<Option<String>, String> {
    let describe = |error: std::io::Error| format!("{}: {error}", file.display());
    match std::fs::read_to_string(file) {
        Ok(text) => {
            std::fs::remove_file(file).map_err(describe)?;
            Ok(Some(text))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(describe(error)),
    }
}

/// `_home-install <session>`: the floor on a home view (see [`floor::install`]). It never
/// fails an attach: a failure goes to stderr and the status line of the session's clients.
struct Install;

impl Verb for Install {
    fn name(&self) -> &'static str {
        INSTALL_VERB
    }

    fn usage(&self) -> &'static str {
        "_home-install <session>   keep the home TUI in the view's pane (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [session] = argv else {
            return usage(io, "_home-install <session>");
        };
        let ui = Ui::from_core(core);
        let installed = Txc::current(core).and_then(|tx| {
            floor::install(&ui, session, &floor::hook_command(&tx.shell(&[])), |pane| {
                tx.shell(&[TUI_VERB, pane, "--first"])
            })
            .map_err(failed)
        });
        if let Err(error) = installed {
            let message = format!("tx home: {error}");
            let _ = writeln!(io.stderr, "{message}");
            for client in clients_on(&ui, session) {
                status(&ui, &client, &message);
            }
        }
        Ok(0)
    }
}

/// `_home-perform <pane>`: the `pane-died` hook. Runs the choice the TUI left for `pane`, else
/// brings the TUI back with the session the pane last nested selected.
struct Perform;

impl Verb for Perform {
    fn name(&self) -> &'static str {
        PERFORM_VERB
    }

    fn usage(&self) -> &'static str {
        "_home-perform <pane>   run the home's choice in its pane, else the home again (pane-died hook, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane] = argv else {
            return usage(io, "_home-perform <pane>");
        };
        let ui = Ui::from_core(core);
        let tx = Txc::current(core)?;
        let directory = plugin_directory(core);
        let session = ui.display_message("#{session_name}", Some(pane));
        let performer = Performer {
            core,
            ui: &ui,
            tx: &tx,
            place: Place::Floor {
                pane,
                client: session.and_then(|session| clients_on(&ui, &session).into_iter().next()),
                directory: &directory,
            },
        };
        let choice = take(&floor::choice_path(&directory, pane)).unwrap_or_else(|error| {
            performer.status(&format!("tx home: {error}"));
            None
        });
        match choice.as_deref().and_then(Action::decode) {
            Some(action) => performer.perform(&action),
            None => {
                let last = take(&floor::last_path(&directory, pane)).unwrap_or_else(|error| {
                    performer.status(&format!("tx home: {error}"));
                    None
                });
                performer.back(last.as_deref().map(str::trim));
            }
        }
        Ok(0)
    }
}

/// The clients attached to `session`, in tmux's order.
fn clients_on(ui: &Ui, session: &str) -> Vec<String> {
    let (_, clients) = ui.quiet(&["list-clients", "-F", "#{client_session}\t#{client_name}"]);
    clients
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(attached, _)| *attached == session)
        .map(|(_, client)| client.to_owned())
        .collect()
}

/// The menu's own failure, on the client's status line: the hook and the bind swallow output.
fn reported(ui: &Ui, client: &str, result: Result<i32, VerbError>) -> Result<i32, VerbError> {
    if let Err(error) = &result {
        status(ui, client, &format!("tx home: {error}"));
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

/// How to run this `tx` against this core.
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
    let tx = Txc::current(core)?;
    let choice = tempfile::Builder::new()
        .prefix("tx-home-")
        .tempfile()
        .map_err(|e| failed(format!("choice file: {e}")))?
        .into_temp_path();
    let choice_path = choice.to_string_lossy();
    let mut args = vec![TUI_VERB, pane, "--popup", "--choice", &choice_path];
    if first {
        args.push("--first");
    }
    let command = tx.shell(&args);
    // -EE: a home that fails keeps its popup open, showing why.
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
        let performer = Performer {
            core,
            ui,
            tx: &tx,
            place: Place::Popup { pane, client },
        };
        performer.perform(&action);
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
        status(ui, client, &format!("tx home: {error}"));
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

/// Where a chosen action lands.
enum Place<'a> {
    /// The home pane, from `_home-perform`: a session nests into it; every other action gives
    /// it the TUI back. `client` shows the pane's session, when one does.
    Floor {
        pane: &'a str,
        client: Option<String>,
        directory: &'a Path,
    },
    /// The prefix+m popup's client: a session is switched to; the floor stays as it is.
    Popup { pane: &'a str, client: &'a str },
}

struct Performer<'a> {
    core: &'a TxCore,
    ui: &'a Ui,
    tx: &'a Txc,
    place: Place<'a>,
}

impl Performer<'_> {
    fn pane(&self) -> &str {
        match &self.place {
            Place::Floor { pane, .. } | Place::Popup { pane, .. } => pane,
        }
    }

    fn client(&self) -> Option<&str> {
        match &self.place {
            Place::Floor { client, .. } => client.as_deref(),
            Place::Popup { client, .. } => Some(client),
        }
    }

    /// On the client's status line; with no client nobody would see it.
    fn status(&self, message: &str) {
        if let Some(client) = self.client() {
            status(self.ui, client, message);
        }
    }

    fn popup(&self, spec: &Popup<'_>, command: &str) {
        if let Some(client) = self.client() {
            popup(self.ui, client, spec, command);
        }
    }

    fn switch(&self, session: &str) {
        if let Some(client) = self.client() {
            switch(self.ui, client, session);
        }
    }

    /// The name of the agent with this id, for the row the TUI comes back on.
    fn name_of(&self, id: &str) -> Option<String> {
        let agent_id = self.core.resolve(id).ok()?;
        self.core.get_agent(&agent_id).ok().map(|agent| agent.name)
    }

    /// The floor's pane back to the TUI, `returned_from`'s row selected. The popup leaves the
    /// floor as it is.
    fn back(&self, returned_from: Option<&str>) {
        let Place::Floor { pane, .. } = &self.place else {
            return;
        };
        let mut args = vec![TUI_VERB, pane];
        if let Some(name) = returned_from {
            args.extend(["--returned-from", name]);
        }
        if let Err(error) = self.ui.respawn_pane(pane, &self.tx.shell(&args)) {
            self.status(&format!("tx home: {error}"));
        }
    }

    /// Session `name`: nested into the floor's pane alone (its end brings the TUI back, on that
    /// row), or the popup's client switched to it.
    fn show(&self, name: &str) {
        let Place::Floor {
            pane, directory, ..
        } = &self.place
        else {
            return self.switch(name);
        };
        let Some(target) = self.ui.live_targets().remove(name) else {
            self.status(&format!("tx home: no session {name}"));
            return self.back(None);
        };
        // Before the attach: a nested attach that ends at once reads it straight away.
        let last = floor::last_path(directory, pane);
        let recorded = std::fs::create_dir_all(directory)
            .and_then(|()| std::fs::write(&last, name))
            .map_err(|error| format!("{}: {error}", last.display()));
        if let Err(error) = recorded {
            self.status(&format!("tx home: {error}"));
        }
        let nest = self.ui.nest_attach_only_command(&target);
        if let Err(error) = self.ui.respawn_pane(pane, &nest) {
            self.status(&format!("tx home: {error}"));
            self.back(Some(name));
        }
    }

    fn perform(&self, action: &Action) {
        let tx = self.tx;
        match action {
            Action::Attach(name) => self.show(name),
            Action::Resume(id) => match output(tx.command(&["respawn", id])) {
                Ok(out) => match respawned_name(&out) {
                    Some(name) => {
                        self.status(&format!("respawned '{name}'"));
                        self.show(name);
                    }
                    None => {
                        self.status("respawned");
                        self.back(None);
                    }
                },
                Err(error) => {
                    self.status(&format!("respawn: {error}"));
                    self.back(self.name_of(id).as_deref());
                }
            },
            Action::Ask(text) => {
                // `assistant ask` prints nothing; `warm` names the session to show.
                let asked = output(tx.command(&[ASSISTANT_VERB, "ask", text]))
                    .and_then(|_| output(tx.command(&[ASSISTANT_VERB, "warm"])));
                match asked {
                    Ok(name) => self.show(name.trim()),
                    Err(error) => {
                        self.status(&format!("assistant: {error}"));
                        self.back(None);
                    }
                }
            }
            Action::New(directory) => {
                self.back(None);
                let Some(client) = self.client() else {
                    return;
                };
                self.popup(
                    &Popup {
                        width: "94",
                        height: "23",
                        y: "C",
                        title: " New agent ",
                        form: true,
                    },
                    &tx.shell(&[
                        "_new-agent",
                        self.pane(),
                        client,
                        "--cwd",
                        &directory.to_string_lossy(),
                    ]),
                );
            }
            Action::Stop(id) => {
                self.back(self.name_of(id).as_deref());
                match output(tx.command(&["stop", id])) {
                    Ok(_) => self.status("stopped"),
                    Err(error) => self.status(&format!("stop: {error}")),
                }
            }
            Action::Artifacts(id) => {
                self.back(self.name_of(id).as_deref());
                let Some(client) = self.client() else {
                    return;
                };
                if let Err(error) = output(tx.command(&["_artifact-menu", id, client])) {
                    self.status(&format!("artifacts: {error}"));
                }
            }
            Action::Timeline => {
                self.back(None);
                self.popup(
                    &Popup {
                        width: "118",
                        height: "30",
                        y: "1",
                        title: " timeline ",
                        form: false,
                    },
                    &tx.shell(&["_timeline-attach"]),
                );
            }
            Action::Tutor => {
                self.back(None);
                let mut command = tx.command(&["tutor", "start", "--detach"]);
                command.env_remove("TMUX");
                match output(command) {
                    Ok(out) => match tutor_view(&out) {
                        Some(view) => self.switch(view),
                        None => self.status(out.lines().last().unwrap_or("tutor started")),
                    },
                    Err(error) => self.status(&format!("tutor: {error}")),
                }
            }
        }
    }
}

fn switch(ui: &Ui, client: &str, session: &str) {
    if let Err(error) = ui.run(&["switch-client", "-c", client, "-t", &format!("={session}")]) {
        status(ui, client, &format!("tx home: {error}"));
    }
}

/// `respawn` prints `<id>\t<name>`: the agent comes back in place, under its own name.
fn respawned_name(stdout: &str) -> Option<&str> {
    let (_, name) = stdout.lines().next()?.split_once('\t')?;
    (!name.is_empty()).then_some(name)
}

/// `tutor start --detach` ends with `<view> is ready: tmux … attach -t =<view>`.
fn tutor_view(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .find_map(|line| line.split_once(" is ready: ").map(|(view, _)| view.trim()))
        .filter(|view| !view.is_empty())
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
    fn tui_arguments_take_flags_in_any_order() {
        let argv = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            tui_arguments(&argv(&["%3", "--popup", "--choice", "/t/c", "--first"])),
            Some(TuiArguments {
                pane: "%3".into(),
                first: true,
                popup: true,
                returned_from: None,
                choice: Some(PathBuf::from("/t/c")),
            })
        );
        assert_eq!(
            tui_arguments(&argv(&["%3", "--returned-from", "v#1 it's"]))
                .unwrap()
                .returned_from
                .as_deref(),
            Some("v#1 it's")
        );
        assert_eq!(tui_arguments(&argv(&[])), None);
        assert_eq!(tui_arguments(&argv(&["%3", "--choice"])), None);
        assert_eq!(tui_arguments(&argv(&["%3", "--returned-from"])), None);
        assert_eq!(tui_arguments(&argv(&["%3", "c"])), None);
    }

    #[test]
    fn a_choice_is_written_taken_once_and_cleared_by_no_choice() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plugins/home/choice.3");
        record_choice(&file, Some(&Action::Attach("v#1 it's".into()))).unwrap();
        assert_eq!(take(&file).unwrap().as_deref(), Some("attach v#1 it's"));
        assert_eq!(take(&file).unwrap(), None);
        record_choice(&file, Some(&Action::Timeline)).unwrap();
        record_choice(&file, None).unwrap();
        assert!(!file.exists());
        record_choice(&file, None).unwrap();
    }

    #[test]
    fn popup_commands_are_shell_quoted() {
        let tx = Txc {
            exe: PathBuf::from("/opt/tx"),
            arguments: vec!["--state-dir".to_owned(), "/s/state".to_owned()],
        };
        assert_eq!(
            tx.shell(&["_new-agent", "%3", "/dev/ttys001"]),
            "'/opt/tx' '--state-dir' '/s/state' '_new-agent' '%3' '/dev/ttys001'"
        );
    }
}
