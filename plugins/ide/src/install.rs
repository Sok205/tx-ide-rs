//! The `ide` plugin's updater, `ui attach` and `ui view`: compose the core's server config from
//! the key fragments `tx update` published (on `tx update`) and give the core's own back when
//! the plugin is retired, enter the IDE's server, and open, list or close views from a shell.

use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tx_client::tmux::{Ui, UiError};
use tx_client::views::{self, ViewKind, ViewRequest};
use tx_core::core::DEFAULT_TMUX_CONF;
use tx_core::plugin::manifest::RESOURCE_KINDS;
use tx_core::updates::Outcome;
use tx_core::{TxCore, Updater, Verb, VerbError, VerbIo};
use tx_plugin::bundle::KEYS_INSTALLED;
use tx_plugin::sdk::TX_BIN_ENV;
use tx_plugin::setup::ROLES_DIR;

use crate::view_verb;

const HOME_SESSION: &str = "home";
const IDE_FRAGMENT: &str = "ide";
/// Under the data directory: the key fragments `tx update` published, `<id>.tmux` each.
const KEYS_DIR: &str = "keys";
const FRAGMENT_EXTENSION: &str = "tmux";
const LEGACY_HINT: &str = "legacy bindings may remain; restart the server to drop them";
/// The server options the composed config sets in its own first lines (see [`conf_text`]); with
/// `TX_BIN` in the server's environment, what a retire takes back next to the fragments' keys.
/// `@tx-bin` is also how the updater recognises a server whose bindings were never recorded.
const CONF_OPTIONS: [&str; 3] = ["@tx-bin", "@tx-data", "@tx-arguments"];
/// What a retire says: the config is core's again, and what a running server keeps of the IDE.
const RESET: &str = "tmux.conf reset to the core default";
const RESTART_HINT: &str = "restart the server to drop the rest of the IDE's look";
/// The home plugin's verb that puts the home TUI into the view's pane; absent when the plugin
/// is disabled.
const HOME_INSTALL_VERB: &str = "_home-install";
/// The resource kind of a role: `tx update` publishes an `agents/…` name under `<data>/roles/`,
/// since `<data>/agents/` holds the agent records.
const ROLES_RESOURCE: &str = "agents";

/// `@tx-data` locates `bin/` and `keys/`; `@tx-arguments` is what every `tx` a key fragment
/// runs carries (the core's `cli_arguments()`, words split by spaces: `install` refuses unsafe
/// paths). `TX_BIN` in the server's environment is the same `tx` as `@tx-bin`: what a plugin
/// program started from a key, a popup or a pane builds its command lines with.
/// `detach-on-destroy on` detaches the clients of an ending session: a client nested in a
/// view's pane goes, and the pane falls back to a shell. The terminal is kept in tmux by its own
/// `no-detach-on-destroy` flag, which `ui attach` sets.
///
/// `fragments` are `(id, text)` pairs in the order they are run. Each runs through the
/// interpreter its first line names, never as an executable file: on macOS the first direct run
/// of a freshly written executable is assessed by the system one file at a time, 0.4 s alone and
/// seconds when many run at once, while the same file run by an interpreter is not.
fn conf_text(exe: &Path, data: &Path, arguments: &[String], fragments: &[(&str, &str)]) -> String {
    let data = data.display();
    let exe = exe.display();
    let arguments = arguments.join(" ");
    let mut text = format!(
        "# tx-core server config, written by `tx update`. Re-run to update.\n\
         set -s exit-empty on\n\
         set -g history-limit 50000\n\
         set -g detach-on-destroy on\n\
         set -g @tx-bin \"{exe}\"\n\
         set -g @tx-data \"{data}\"\n\
         set -g @tx-arguments \"{arguments}\"\n\
         set-environment -g TX_BIN \"{exe}\"\n\
         source-file \"{data}/conf/tmux.conf\"\n",
    );
    for (id, fragment) in fragments {
        let interpreter = fragment
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("#!"))
            .map(|words| format!("{} ", words.trim()))
            .unwrap_or_default();
        text.push_str(&format!(
            "run-shell \"{interpreter}{data}/keys/{id}.tmux\"\n"
        ));
    }
    text
}

/// What a key fragment declares on top-level comment lines: `# keys: X Y` (prefix-table keys),
/// `# unset: option …` and `# unhook: hook …`. `none` stands for "nothing". Several lines of one
/// kind add up.
#[derive(Debug, Default, PartialEq, Eq)]
struct Declared {
    keys: Vec<String>,
    unset: Vec<String>,
    unhook: Vec<String>,
}

fn parse_declarations(text: &str) -> Declared {
    let mut declared = Declared::default();
    for line in text.lines() {
        let (target, rest) = if let Some(rest) = line.strip_prefix("# keys:") {
            (&mut declared.keys, rest)
        } else if let Some(rest) = line.strip_prefix("# unset:") {
            (&mut declared.unset, rest)
        } else if let Some(rest) = line.strip_prefix("# unhook:") {
            (&mut declared.unhook, rest)
        } else {
            continue;
        };
        target.extend(
            rest.split_whitespace()
                .filter(|word| *word != "none")
                .map(str::to_owned),
        );
    }
    declared
}

/// `<data>/keys/.installed`: one line per key fragment, tab-separated fields
/// `<id>\tkeys=<k k…>\tunset=<o o…>\tunhook=<h h…>`; a group may be empty.
fn render_installed(entries: &[(String, Declared)]) -> String {
    entries
        .iter()
        .map(|(id, d)| {
            format!(
                "{id}\tkeys={}\tunset={}\tunhook={}\n",
                d.keys.join(" "),
                d.unset.join(" "),
                d.unhook.join(" ")
            )
        })
        .collect()
}

fn parse_installed(text: &str) -> Vec<(String, Declared)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let id = fields.next().filter(|id| !id.is_empty())?;
            let mut declared = Declared::default();
            for field in fields {
                let words = |rest: &str| rest.split_whitespace().map(str::to_owned).collect();
                if let Some(rest) = field.strip_prefix("keys=") {
                    declared.keys = words(rest);
                } else if let Some(rest) = field.strip_prefix("unset=") {
                    declared.unset = words(rest);
                } else if let Some(rest) = field.strip_prefix("unhook=") {
                    declared.unhook = words(rest);
                }
            }
            Some((id.to_owned(), declared))
        })
        .collect()
}

/// Paths are spliced into double-quoted tmux config lines and into shell command strings, so
/// anything the config parser or a shell would interpret is refused.
fn is_unsafe_path(path: &Path) -> bool {
    path.to_string_lossy()
        .chars()
        .any(|c| c.is_whitespace() || "\"'$#;`\\".contains(c))
}

/// The socket path in a `$TMUX` value (`<socket>,<pid>,<session>`).
fn tmux_env_socket(env: Option<&str>) -> Option<&str> {
    env.and_then(|value| value.split(',').next())
        .filter(|socket| !socket.is_empty())
}

/// Takes back, in a running server, what a key fragment declared: its prefix keys are unbound,
/// its options and its hooks unset.
fn take_back(ui: &Ui, declared: &Declared) {
    for key in &declared.keys {
        ui.quiet(&["unbind", key]);
    }
    for option in &declared.unset {
        ui.quiet(&["set", "-gu", option]);
    }
    for hook in &declared.unhook {
        ui.quiet(&["set-hook", "-gu", hook]);
    }
}

/// The words of `before` that `now` no longer lists; all of them when the fragment is gone.
fn dropped(before: &[String], now: Option<&[String]>) -> Vec<String> {
    before
        .iter()
        .filter(|word| !now.is_some_and(|now| now.contains(word)))
        .cloned()
        .collect()
}

fn fail(io: &mut VerbIo<'_>, message: &str) -> Result<i32, VerbError> {
    let _ = writeln!(io.stderr, "{message}");
    Ok(1)
}

fn usage(io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
    let _ = writeln!(io.stderr, "usage: tx ui attach | view <open|ls|close> …");
    Ok(2)
}

pub struct UiVerb;

impl Verb for UiVerb {
    fn name(&self) -> &'static str {
        "ui"
    }

    fn usage(&self) -> &'static str {
        "ui attach | view <open|ls|close>   enter the IDE's server; manage views"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv {
            [sub] if sub == "attach" => attach(core, io),
            [sub, rest @ ..] if sub == "view" => view_verb::run(core, io, rest),
            _ => usage(io),
        }
    }
}

/// Where `tx update` publishes the resource `name`: `<data>/<name>`, an `agents/…` name under
/// `<data>/roles/`. `None` for a name no resource can have: not `<kind>/<path>` with one of the
/// kinds `tx update` publishes, or holding a `..` segment.
fn published_path(data: &Path, name: &str) -> Option<PathBuf> {
    let (kind, path) = name.split_once('/')?;
    if !RESOURCE_KINDS.contains(&kind) || path.split('/').any(|segment| segment == "..") {
        return None;
    }
    Some(if kind == ROLES_RESOURCE {
        data.join(format!("{ROLES_DIR}/{path}"))
    } else {
        data.join(name)
    })
}

/// `_resource <name>`: a resource as `tx update` published it, whichever plugin it is of, for
/// tests and debugging.
pub struct ResourceVerb;

impl Verb for ResourceVerb {
    fn name(&self) -> &'static str {
        "_resource"
    }

    fn usage(&self) -> &'static str {
        "_resource NAME   print a plugin resource (internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [name] = argv else {
            let _ = writeln!(io.stderr, "usage: tx _resource NAME");
            return Ok(2);
        };
        let missing = format!("tx _resource: no such resource: {name}");
        let Some(path) = published_path(&core.config().directories.data, name) else {
            return fail(io, &missing);
        };
        match std::fs::read(&path) {
            Ok(bytes) => {
                io.stdout
                    .write_all(&bytes)
                    .map_err(|e| VerbError::Failed(format!("write: {e}")))?;
                Ok(0)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => fail(io, &missing),
            Err(error) => Err(io_failed(&path)(error)),
        }
    }
}

fn io_failed(path: &Path) -> impl FnOnce(io::Error) -> VerbError {
    let path = path.to_path_buf();
    move |e| VerbError::Failed(format!("{}: {e}", path.display()))
}

/// The key fragments `tx update` published under `<data>/keys`, as `(id, text)` pairs in the
/// order the server config runs them: `ide` first, the others by name.
fn published_fragments(data: &Path) -> Result<Vec<(String, String)>, VerbError> {
    let directory = data.join(KEYS_DIR);
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(io_failed(&directory)(error)),
    };
    let mut fragments = Vec::new();
    for entry in entries {
        let path = entry.map_err(io_failed(&directory))?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == FRAGMENT_EXTENSION)
            && let Some(id) = path.file_stem()
        {
            let text = std::fs::read(&path).map_err(io_failed(&path))?;
            fragments.push((
                id.to_string_lossy().into_owned(),
                String::from_utf8_lossy(&text).into_owned(),
            ));
        }
    }
    fragments.sort_by(|(one, _), (other, _)| {
        (one != IDE_FRAGMENT, one).cmp(&(other != IDE_FRAGMENT, other))
    });
    Ok(fragments)
}

/// The `ide` plugin's part of `tx update`: composes the server config from the key fragments
/// `tx update` published, and reloads a running server. Retired, it gives the config back to
/// core.
pub struct IdeUpdater;

impl Updater for IdeUpdater {
    fn update(&self, core: &TxCore, io: &mut VerbIo<'_>) -> Result<Outcome, VerbError> {
        install(core, io)?;
        Ok(Outcome::default())
    }

    fn retire(&self, core: &TxCore, io: &mut VerbIo<'_>) -> Result<Outcome, VerbError> {
        retire(core, io)?;
        Ok(Outcome::default())
    }
}

/// Takes back what [`install`] generated, once the plugin stopped being active. The record of
/// the fragments the config was composed from, `<data>/keys/.installed`, says there is
/// something to take back: without it nothing is done and nothing printed, so a retire is safe
/// to repeat. With it, `<data>/tmux.conf` is core's default again; a running server loses every
/// key, option and hook the record lists, the options and the `TX_BIN` the composed config
/// itself set, and loads that config; the record goes last, so a retire that failed half way is
/// tried again by the next `tx update`. The server is then a plain core server to the updater:
/// enabling the plugin again finds no trace of an unrecorded install. What the fragments set
/// without declaring it (the theme, the root-table keys) stays until the server restarts.
fn retire(core: &TxCore, io: &mut VerbIo<'_>) -> Result<(), VerbError> {
    let installed = core.config().directories.data.join(KEYS_INSTALLED);
    let entries = match std::fs::read_to_string(&installed) {
        Ok(text) => parse_installed(&text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_failed(&installed)(error)),
    };
    let conf = core.config().tmux_conf_path();
    std::fs::write(&conf, DEFAULT_TMUX_CONF).map_err(io_failed(&conf))?;
    let mut out = format!("{RESET}\n");
    let ui = Ui::from_core(core);
    if ui.quiet(&["list-sessions"]).0 {
        for (_, declared) in &entries {
            take_back(&ui, declared);
        }
        for option in CONF_OPTIONS {
            ui.quiet(&["set", "-gu", option]);
        }
        ui.quiet(&["set-environment", "-gu", TX_BIN_ENV]);
        ui.run(&["source-file", &conf.to_string_lossy()])
            .map_err(|e| VerbError::Failed(e.to_string()))?;
        out.push_str(RESTART_HINT);
        out.push('\n');
    }
    std::fs::remove_file(&installed).map_err(io_failed(&installed))?;
    io.stdout
        .write_all(out.as_bytes())
        .map_err(|e| VerbError::Failed(format!("write: {e}")))
}

fn install(core: &TxCore, io: &mut VerbIo<'_>) -> Result<(), VerbError> {
    // Canonical: the config holds one spelling of the binary, whatever way `tx` was invoked.
    let exe = tx_plugin::sdk::tx_bin()
        .canonicalize()
        .map_err(|e| VerbError::Failed(format!("cannot resolve the tx binary: {e}")))?;
    let config = core.config();
    let data = &config.directories.data;
    let arguments = config.cli_arguments();
    if let Some(path) = std::iter::once(exe.as_path())
        .chain(arguments.iter().map(Path::new))
        .find(|path| is_unsafe_path(path))
    {
        return Err(VerbError::Failed(format!(
            "path contains unsafe characters: {}",
            path.display()
        )));
    }
    let previous = std::fs::read_to_string(data.join(KEYS_INSTALLED))
        .ok()
        .map(|text| parse_installed(&text));
    let texts = published_fragments(data)?;
    let fragments: Vec<(String, Declared)> = texts
        .iter()
        .map(|(id, text)| (id.clone(), parse_declarations(text)))
        .collect();
    let run_order: Vec<(&str, &str)> = texts
        .iter()
        .map(|(id, text)| (id.as_str(), text.as_str()))
        .collect();
    let conf = core.config().tmux_conf_path();
    std::fs::write(&conf, conf_text(&exe, data, &arguments, &run_order))
        .map_err(io_failed(&conf))?;
    let installed = data.join(KEYS_INSTALLED);
    let write_installed =
        || std::fs::write(&installed, render_installed(&fragments)).map_err(io_failed(&installed));

    let mut out = format!("{}\n", conf.display());
    let ui = Ui::from_core(core);
    if ui.quiet(&["list-sessions"]).0 {
        let legacy_conf_loaded = previous.is_none() && {
            let (ok, value) = ui.quiet(&["show-option", "-gv", "@tx-bin"]);
            ok && !value.trim().is_empty()
        };
        for (id, before) in previous.iter().flatten() {
            let now = fragments.iter().find(|(now, _)| now == id).map(|(_, d)| d);
            take_back(
                &ui,
                &Declared {
                    keys: dropped(&before.keys, now.map(|d| d.keys.as_slice())),
                    unset: dropped(&before.unset, now.map(|d| d.unset.as_slice())),
                    unhook: dropped(&before.unhook, now.map(|d| d.unhook.as_slice())),
                },
            );
        }
        ui.run(&["source-file", &conf.to_string_lossy()])
            .map_err(|e| VerbError::Failed(e.to_string()))?;
        write_installed()?;
        out.push_str("reloaded\n");
        if previous.is_none() && legacy_conf_loaded {
            out.push_str(LEGACY_HINT);
            out.push('\n');
        }
    } else {
        write_installed()?;
    }
    io.stdout
        .write_all(out.as_bytes())
        .map_err(|e| VerbError::Failed(format!("write: {e}")))?;
    Ok(())
}

/// Hands `home` to the `home` plugin's floor (`_home-install`) when the plugin is active: the
/// pane keeps the home TUI, which starts the assistant itself. Its stderr is passed through; a
/// failure is reported and never stops the attach. Without the plugin the home is a plain shell:
/// a floor the plugin left before it was disabled is taken apart.
fn install_floor(core: &TxCore, ui: &Ui, io: &mut VerbIo<'_>) {
    let Some(verb) = core.verb(HOME_INSTALL_VERB) else {
        if let Err(error) = remove_floor(ui) {
            let _ = writeln!(io.stderr, "tx ui attach: home: {error}");
        }
        return;
    };
    let mut stdin = io::empty();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let result = verb.run(
        core,
        &mut VerbIo {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
        &[HOME_SESSION.to_owned()],
    );
    let _ = io.stderr.write_all(&stderr);
    if let Err(error) = result {
        let _ = writeln!(io.stderr, "tx ui attach: home: {error}");
    }
}

/// The floor pane of `home` back to tmux's defaults: its `pane-died` hook would run a verb that
/// is gone, and `remain-on-exit` would keep it dead. A dead one gets the shell.
fn remove_floor(ui: &Ui) -> Result<(), UiError> {
    for pane in views::home_panes(ui, HOME_SESSION)?
        .iter()
        .filter(|pane| pane.floor)
    {
        let id = pane.id.as_str();
        let steps: [&[&str]; 4] = [
            &["set-hook", "-pu", "-t", id, "pane-died"],
            &["set-option", "-p", "-t", id, "remain-on-exit", "off"],
            &["set-option", "-pu", "-t", id, "remain-on-exit-format"],
            &["set-option", "-pu", "-t", id, views::HOME_FLOOR_OPTION],
        ];
        for step in steps {
            ui.run(step)?;
        }
        if pane.dead {
            ui.respawn_pane(id, &views::default_shell())?;
        }
    }
    Ok(())
}

/// Creates the `home` view on the core's server unless it is already there: a shell in a window
/// named `home`, with the status bar and the pane-border labels on.
fn ensure_home(ui: &Ui) -> Result<(), VerbError> {
    if ui.has_session(HOME_SESSION) {
        return Ok(());
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
    views::open(
        ui,
        &ViewRequest {
            name: HOME_SESSION,
            kind: ViewKind::Home,
            program: &views::default_shell(),
            arguments: &[],
            working_directory: Path::new(&home),
            tags: &[],
            parent_id: None,
            bound: None,
        },
    )
    .map_err(|e| VerbError::Failed(e.to_string()))?;
    let target = format!("={HOME_SESSION}:");
    let steps: [&[&str]; 3] = [
        &["set-option", "-t", &target, "status", "on"],
        &[
            "set-window-option",
            "-t",
            &target,
            "pane-border-status",
            "top",
        ],
        &["rename-window", "-t", &target, HOME_SESSION],
    ];
    for step in steps {
        ui.run(step).map_err(|e| VerbError::Failed(e.to_string()))?;
    }
    Ok(())
}

/// The terminal's client flag: when its session ends it moves to another session instead of
/// leaving tmux (`detach-on-destroy on` detaches every other client).
const TERMINAL_FLAG: &str = "no-detach-on-destroy";

fn attach(core: &TxCore, io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
    let ui = Ui::from_core(core);
    ensure_home(&ui)?;
    // Every attach: a home from an older install, or one whose floor was lost, gets it back.
    install_floor(core, &ui, io);
    let env = std::env::var("TMUX").ok();
    match tmux_env_socket(env.as_deref()) {
        Some(socket) if Path::new(socket) == core.socket_path() => {
            let terminal = ui
                .terminal_client()
                .map_err(|e| VerbError::Failed(e.to_string()))?;
            ui.run(&["refresh-client", "-t", &terminal, "-f", TERMINAL_FLAG])
                .and_then(|_| ui.switch_client(HOME_SESSION))
                .map_err(|e| VerbError::Failed(e.to_string()))?;
            return Ok(0);
        }
        Some(socket) => {
            let _ = writeln!(
                io.stderr,
                "tx ui attach: nesting inside another tmux (socket {socket}); its prefix wins \u{2014} press the prefix twice to reach the IDE's keys"
            );
        }
        None => {}
    }
    let _ = io.stdout.flush();
    let error = ui
        .command(&[
            "attach-session",
            "-f",
            TERMINAL_FLAG,
            "-t",
            &format!("={HOME_SESSION}"),
        ])
        .stdin(Stdio::inherit())
        .env_remove("TMUX")
        .exec();
    Err(VerbError::Failed(format!("could not run tmux: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conf_text_is_exact() {
        assert_eq!(
            conf_text(
                Path::new("/opt/tx"),
                Path::new("/s/data"),
                &words(&["--data-dir", "/s/data", "--run-dir", "/r"]),
                &[
                    ("ide", "#!/usr/bin/env bash\n# keys: none\n"),
                    ("kill", "#!/bin/sh\n"),
                    ("labels", "# no interpreter line\n"),
                ]
            ),
            "# tx-core server config, written by `tx update`. Re-run to update.\n\
             set -s exit-empty on\n\
             set -g history-limit 50000\n\
             set -g detach-on-destroy on\n\
             set -g @tx-bin \"/opt/tx\"\n\
             set -g @tx-data \"/s/data\"\n\
             set -g @tx-arguments \"--data-dir /s/data --run-dir /r\"\n\
             set-environment -g TX_BIN \"/opt/tx\"\n\
             source-file \"/s/data/conf/tmux.conf\"\n\
             run-shell \"/usr/bin/env bash /s/data/keys/ide.tmux\"\n\
             run-shell \"/bin/sh /s/data/keys/kill.tmux\"\n\
             run-shell \"/s/data/keys/labels.tmux\"\n"
        );
    }

    #[test]
    fn a_fragment_runs_through_the_interpreter_of_its_first_line() {
        let line = |text| {
            let conf = conf_text(Path::new("/x"), Path::new("/d"), &[], &[("a", text)]);
            conf.lines().last().unwrap().to_owned()
        };
        assert_eq!(
            line("#!/usr/bin/env bash\necho"),
            "run-shell \"/usr/bin/env bash /d/keys/a.tmux\""
        );
        assert_eq!(line("#!/bin/sh\n"), "run-shell \"/bin/sh /d/keys/a.tmux\"");
        assert_eq!(line("#!/bin/sh"), "run-shell \"/bin/sh /d/keys/a.tmux\"");
        assert_eq!(line("echo\n#!/bin/sh\n"), "run-shell \"/d/keys/a.tmux\"");
        assert_eq!(line(""), "run-shell \"/d/keys/a.tmux\"");
    }

    #[test]
    fn the_fragments_are_the_published_files_ide_first_then_by_name() {
        let data = tempfile::tempdir().unwrap();
        assert!(published_fragments(data.path()).unwrap().is_empty());
        let keys = data.path().join(KEYS_DIR);
        std::fs::create_dir_all(&keys).unwrap();
        for (name, text) in [
            ("kill.tmux", "# keys: X\n"),
            ("ide.tmux", "#!/usr/bin/env bash\n"),
            ("artifacts.tmux", "# keys: a\n"),
            // Not fragments: the updater's own record, a file left half written, a stray one.
            (".installed", "ide\tkeys=\tunset=\tunhook=\n"),
            (".kill.tmux.123.tmp", "half\n"),
            ("notes.txt", "x\n"),
        ] {
            std::fs::write(keys.join(name), text).unwrap();
        }
        assert_eq!(
            published_fragments(data.path()).unwrap(),
            [
                ("ide".to_owned(), "#!/usr/bin/env bash\n".to_owned()),
                ("artifacts".to_owned(), "# keys: a\n".to_owned()),
                ("kill".to_owned(), "# keys: X\n".to_owned()),
            ]
        );
    }

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn declarations_are_parsed_from_comment_lines() {
        let text = "#!/usr/bin/env bash\n# keys: s\n# unset: a b\n# unhook: c\nset -u\n\
                    echo '# keys: ignored'\n  # keys: indented is prose\n";
        assert_eq!(
            parse_declarations(text),
            Declared {
                keys: words(&["s"]),
                unset: words(&["a", "b"]),
                unhook: words(&["c"]),
            }
        );
        assert_eq!(
            parse_declarations("# keys: none\n"),
            Declared::default(),
            "none is no key"
        );
        assert_eq!(
            parse_declarations("# keys: X\n# keys: e t\n").keys,
            words(&["X", "e", "t"])
        );
        assert_eq!(parse_declarations(""), Declared::default());
    }

    #[test]
    fn installed_file_round_trips() {
        let entries = vec![
            ("ide".to_owned(), Declared::default()),
            (
                "labels".to_owned(),
                Declared {
                    keys: words(&["s"]),
                    unset: words(&["pane-border-format"]),
                    unhook: words(&["after-new-window"]),
                },
            ),
            (
                "kill".to_owned(),
                Declared {
                    keys: words(&["X", "C-x"]),
                    ..Declared::default()
                },
            ),
        ];
        let text = render_installed(&entries);
        assert_eq!(
            text,
            "ide\tkeys=\tunset=\tunhook=\n\
             labels\tkeys=s\tunset=pane-border-format\tunhook=after-new-window\n\
             kill\tkeys=X C-x\tunset=\tunhook=\n"
        );
        assert_eq!(parse_installed(&text), entries);
        assert!(parse_installed("").is_empty());
        assert!(parse_installed("\n\tkeys=a\n").is_empty());
    }

    /// A core on directories of its own, with no server on its socket.
    fn core() -> (tempfile::TempDir, TxCore) {
        let dir = tempfile::Builder::new()
            .prefix("tx")
            .tempdir_in("/tmp")
            .unwrap();
        let core = TxCore::open(tx_core::CoreConfig::new(tx_core::Directories::under(
            dir.path().join("state"),
        )))
        .unwrap();
        (dir, core)
    }

    /// What the updater's `retire` printed.
    fn retired(core: &TxCore) -> String {
        let mut stdout = Vec::new();
        IdeUpdater
            .retire(
                core,
                &mut VerbIo {
                    stdin: &mut io::empty(),
                    stdout: &mut stdout,
                    stderr: &mut Vec::new(),
                },
            )
            .unwrap();
        String::from_utf8(stdout).unwrap()
    }

    #[test]
    fn retire_gives_the_config_back_to_core_and_drops_the_record() {
        let (_dir, core) = core();
        let conf = core.config().tmux_conf_path();
        let installed = core.config().directories.data.join(KEYS_INSTALLED);
        std::fs::write(&conf, "set -g @tx-bin \"/opt/tx\"\n").unwrap();
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "ide\tkeys=C-h\tunset=\tunhook=\n").unwrap();
        // No server runs: nothing to unbind, and nothing to restart.
        assert_eq!(retired(&core), format!("{RESET}\n"));
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), DEFAULT_TMUX_CONF);
        assert!(!installed.exists());
    }

    #[test]
    fn retire_without_the_record_does_and_says_nothing() {
        let (_dir, core) = core();
        let conf = core.config().tmux_conf_path();
        std::fs::write(&conf, "# the operator's own\n").unwrap();
        assert_eq!(retired(&core), "");
        assert_eq!(
            std::fs::read_to_string(&conf).unwrap(),
            "# the operator's own\n"
        );
    }

    #[test]
    fn a_resource_is_published_under_its_name_and_a_role_under_roles() {
        let data = Path::new("/s/data");
        let published = |name| published_path(data, name);
        assert_eq!(
            published("keys/ide.tmux"),
            Some(PathBuf::from("/s/data/keys/ide.tmux"))
        );
        assert_eq!(
            published("nvim/lua/tx-ask.lua"),
            Some(PathBuf::from("/s/data/nvim/lua/tx-ask.lua"))
        );
        assert_eq!(
            published("agents/COMMON.md"),
            Some(PathBuf::from("/s/data/roles/COMMON.md"))
        );
        // Never outside the directory of its kind.
        assert_eq!(
            published("agents//etc/passwd"),
            Some(PathBuf::from("/s/data/roles//etc/passwd"))
        );
        for name in [
            "",
            "tmux.conf",
            "nope/x",
            "roles/COMMON.md",
            "/etc/passwd",
            "keys/../tmux.conf",
            "agents/../../x",
        ] {
            assert_eq!(published(name), None, "{name:?}");
        }
    }

    #[test]
    fn unsafe_path_predicate() {
        for bad in [
            "/a b/x", "/a\tb", "/a\"b", "/a'b", "/a$b", "/a#b", "/a;b", "/a`b", "/a\\b", "/a\nb",
        ] {
            assert!(is_unsafe_path(Path::new(bad)), "{bad:?}");
        }
        for good in [
            "/opt/tx",
            "/Users/x/Rust-Roverprojects/poc_1/a.b",
            "/s/state",
        ] {
            assert!(!is_unsafe_path(Path::new(good)), "{good:?}");
        }
    }

    #[test]
    fn tmux_env_socket_is_the_text_before_the_first_comma() {
        assert_eq!(tmux_env_socket(Some("/a/b.sock,12,0")), Some("/a/b.sock"));
        assert_eq!(tmux_env_socket(Some("/a/b.sock")), Some("/a/b.sock"));
        assert_eq!(tmux_env_socket(Some("")), None);
        assert_eq!(tmux_env_socket(Some(",1,0")), None);
        assert_eq!(tmux_env_socket(None), None);
    }
}
