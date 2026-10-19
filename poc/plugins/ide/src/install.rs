//! `ui install` and `ui attach`: compose the core's server config from plugin resources and
//! enter the IDE's server.

use std::collections::HashSet;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tx_core::{Resource, TxCore, Verb, VerbError, VerbIo};

use tx_ui::tmux::Ui;

const HOME_SESSION: &str = "home";
const INSTALLED: &str = "keys/.installed";
const IDE_FRAGMENT: &str = "ide";
const LEGACY_HINT: &str = "legacy bindings may remain; restart the server to drop them";

fn conf_text(exe: &Path, state: &Path, others: &[&str]) -> String {
    let state = state.display();
    let mut text = format!(
        "# tx-core server config, written by `txc ui install`. Re-run to update.\n\
         set -s exit-empty on\n\
         set -g history-limit 50000\n\
         set -g detach-on-destroy off\n\
         set -g @tx-bin \"{}\"\n\
         set -g @tx-state \"{state}\"\n\
         source-file \"{state}/conf/tmux.conf\"\n\
         run-shell \"{state}/keys/{IDE_FRAGMENT}.tmux\"\n",
        exe.display(),
    );
    for id in others {
        text.push_str(&format!("run-shell \"{state}/keys/{id}.tmux\"\n"));
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

/// `<state>/keys/.installed`: one line per key fragment, tab-separated fields
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

fn fail(io: &mut VerbIo<'_>, message: &str) -> Result<i32, VerbError> {
    let _ = writeln!(io.stderr, "{message}");
    Ok(1)
}

fn usage(io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
    let _ = writeln!(io.stderr, "usage: txc ui install | attach");
    Ok(2)
}

pub struct UiVerb;

impl Verb for UiVerb {
    fn name(&self) -> &'static str {
        "ui"
    }

    fn usage(&self) -> &'static str {
        "ui install | attach   write the tmux config from plugin resources; enter the IDE's server"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv {
            [sub] if sub == "attach" => attach(core, io),
            [sub] if sub == "install" => install(core, io),
            _ => usage(io),
        }
    }
}

/// `_resource <name>`: the bytes of an active plugin's resource, for tests and debugging.
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
            let _ = writeln!(io.stderr, "usage: txc _resource NAME");
            return Ok(2);
        };
        match core.resources("").into_iter().find(|r| &r.name == name) {
            Some(resource) => {
                io.stdout
                    .write_all(resource.bytes)
                    .map_err(|e| VerbError::Failed(format!("write: {e}")))?;
                Ok(0)
            }
            None => fail(io, &format!("txc _resource: no such resource: {name}")),
        }
    }
}

fn io_failed(path: &Path) -> impl FnOnce(io::Error) -> VerbError {
    let path = path.to_path_buf();
    move |e| VerbError::Failed(format!("{}: {e}", path.display()))
}

/// Writes every `bin/`, `keys/` and `conf/` resource under `state` and deletes files in those
/// directories that no resource accounts for. `bin/` and `keys/` files are executable: scripts,
/// and fragments that `run-shell` executes.
fn materialise(state: &Path, resources: &[Resource]) -> Result<(), VerbError> {
    let mut keep: HashSet<PathBuf> = HashSet::from([state.join(INSTALLED)]);
    for resource in resources {
        let path = state.join(&resource.name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_failed(parent))?;
        }
        std::fs::write(&path, resource.bytes).map_err(io_failed(&path))?;
        let mode = if resource.name.starts_with("conf/") {
            0o644
        } else {
            0o755
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            .map_err(io_failed(&path))?;
        keep.insert(path);
    }
    for kind in ["bin", "keys", "conf"] {
        prune(&state.join(kind), &keep).map_err(io_failed(&state.join(kind)))?;
    }
    Ok(())
}

fn prune(dir: &Path, keep: &HashSet<PathBuf>) -> io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            prune(&path, keep)?;
        } else if !keep.contains(&path) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

fn install(core: &TxCore, io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|e| VerbError::Failed(format!("cannot resolve the txc binary: {e}")))?;
    let state = &core.config().state_dir;
    if let Some(path) = [exe.as_path(), state.as_path()]
        .into_iter()
        .find(|path| is_unsafe_path(path))
    {
        return fail(
            io,
            &format!(
                "txc ui install: path contains unsafe characters: {}",
                path.display()
            ),
        );
    }
    let previous = std::fs::read_to_string(state.join(INSTALLED))
        .ok()
        .map(|text| parse_installed(&text));
    let resources: Vec<Resource> = ["bin/", "keys/", "conf/"]
        .into_iter()
        .flat_map(|prefix| core.resources(prefix))
        .collect();
    materialise(state, &resources)?;

    let fragments: Vec<(String, Declared)> = resources
        .iter()
        .filter_map(|r| {
            let id = r.name.strip_prefix("keys/")?.strip_suffix(".tmux")?;
            let text = String::from_utf8_lossy(r.bytes);
            Some((id.to_owned(), parse_declarations(&text)))
        })
        .collect();
    let others: Vec<&str> = fragments
        .iter()
        .map(|(id, _)| id.as_str())
        .filter(|id| *id != IDE_FRAGMENT)
        .collect();
    let conf = core.config().tmux_conf_path();
    std::fs::write(&conf, conf_text(&exe, state, &others)).map_err(io_failed(&conf))?;
    let installed = state.join(INSTALLED);
    std::fs::write(&installed, render_installed(&fragments)).map_err(io_failed(&installed))?;

    let mut out = format!("{}\n", conf.display());
    let ui = Ui::from_core(core);
    if ui.quiet(&["list-sessions"]).0 {
        let gone = previous
            .iter()
            .flatten()
            .filter(|(id, _)| !fragments.iter().any(|(now, _)| now == id));
        for (_, declared) in gone {
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
        ui.run(&["source-file", &conf.to_string_lossy()])
            .map_err(|e| VerbError::Failed(e.to_string()))?;
        out.push_str("reloaded\n");
        if previous.is_none() {
            out.push_str(LEGACY_HINT);
            out.push('\n');
        }
    }
    io.stdout
        .write_all(out.as_bytes())
        .map_err(|e| VerbError::Failed(format!("write: {e}")))?;
    Ok(0)
}

/// Creates the `home` view on the core's server unless it is already there.
fn ensure_home(ui: &Ui) -> Result<(), VerbError> {
    if ui.has_session(HOME_SESSION) {
        return Ok(());
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
    let target = format!("={HOME_SESSION}:");
    let steps: [&[&str]; 4] = [
        &["new-session", "-d", "-s", HOME_SESSION, "-c", &home],
        &["set-option", "-t", &target, "@tx-view", "1"],
        &["set-option", "-t", &target, "status", "on"],
        &[
            "set-window-option",
            "-t",
            &target,
            "pane-border-status",
            "top",
        ],
    ];
    for step in steps {
        ui.run(step).map_err(|e| VerbError::Failed(e.to_string()))?;
    }
    Ok(())
}

fn attach(core: &TxCore, io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
    let ui = Ui::from_core(core);
    ensure_home(&ui)?;
    let env = std::env::var("TMUX").ok();
    match tmux_env_socket(env.as_deref()) {
        Some(socket) if Path::new(socket) == core.socket_path() => {
            ui.run(&["switch-client", "-t", &format!("={HOME_SESSION}")])
                .map_err(|e| VerbError::Failed(e.to_string()))?;
            return Ok(0);
        }
        Some(socket) => {
            let _ = writeln!(
                io.stderr,
                "txc ui attach: nesting inside another tmux (socket {socket}); its prefix wins \u{2014} press the prefix twice to reach the IDE's keys"
            );
        }
        None => {}
    }
    let _ = io.stdout.flush();
    let error = ui
        .command(&["attach-session", "-t", &format!("={HOME_SESSION}")])
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
                Path::new("/opt/txc"),
                Path::new("/s/state"),
                &["kill", "labels"]
            ),
            "# tx-core server config, written by `txc ui install`. Re-run to update.\n\
             set -s exit-empty on\n\
             set -g history-limit 50000\n\
             set -g detach-on-destroy off\n\
             set -g @tx-bin \"/opt/txc\"\n\
             set -g @tx-state \"/s/state\"\n\
             source-file \"/s/state/conf/tmux.conf\"\n\
             run-shell \"/s/state/keys/ide.tmux\"\n\
             run-shell \"/s/state/keys/kill.tmux\"\n\
             run-shell \"/s/state/keys/labels.tmux\"\n"
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

    #[test]
    fn unsafe_path_predicate() {
        for bad in [
            "/a b/x", "/a\tb", "/a\"b", "/a'b", "/a$b", "/a#b", "/a;b", "/a`b", "/a\\b", "/a\nb",
        ] {
            assert!(is_unsafe_path(Path::new(bad)), "{bad:?}");
        }
        for good in [
            "/opt/txc",
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
