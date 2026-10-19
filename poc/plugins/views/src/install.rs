//! `ui install` and `ui attach`: wire the IDE's tmux config into the core's server.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tx_core::{TxCore, Verb, VerbError, VerbIo};

use tx_ui::tmux::Ui;

const FRAGMENT: &str = "tmux/tx-ide.tmux";
const HOME_SESSION: &str = "home";

fn conf_text(exe: &Path, state: &Path, dir: &Path) -> String {
    format!(
        "# tx-core server config, written by `txc ui install`. Re-run to update.\n\
         set -s exit-empty on\n\
         set -g history-limit 50000\n\
         set -g detach-on-destroy off\n\
         set -g @tx-bin \"{}\"\n\
         set -g @tx-state \"{}\"\n\
         source-file \"{dir}/tmux/tmux.conf\"\n\
         run-shell \"{dir}/tmux/tx-ide.tmux\"\n",
        exe.display(),
        state.display(),
        dir = dir.display(),
    )
}

/// The first ancestor of the running binary that holds the tmux fragment.
fn find_poc_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    exe.ancestors()
        .find(|dir| dir.join(FRAGMENT).is_file())
        .map(Path::to_path_buf)
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
    let _ = writeln!(io.stderr, "usage: txc ui install [--poc DIR] | attach");
    Ok(2)
}

pub struct UiVerb;

impl Verb for UiVerb {
    fn name(&self) -> &'static str {
        "ui"
    }

    fn usage(&self) -> &'static str {
        "ui install [--poc DIR] | attach   write the tmux config; enter the IDE's server"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv {
            [sub] if sub == "attach" => attach(core, io),
            [sub, rest @ ..] if sub == "install" => install(core, io, rest),
            _ => usage(io),
        }
    }
}

fn install(core: &TxCore, io: &mut VerbIo<'_>, args: &[String]) -> Result<i32, VerbError> {
    let dir = match args {
        [] => find_poc_dir(),
        [flag, dir] if flag == "--poc" => Some(PathBuf::from(dir)),
        _ => return usage(io),
    };
    let Some(dir) = dir else {
        return fail(
            io,
            "txc ui install: cannot find the poc directory (pass --poc DIR)",
        );
    };
    let Some(dir) = dir
        .canonicalize()
        .ok()
        .filter(|dir| dir.join(FRAGMENT).is_file())
    else {
        return fail(
            io,
            &format!("txc ui install: no {FRAGMENT} under {}", dir.display()),
        );
    };
    let exe = std::env::current_exe()
        .and_then(|exe| exe.canonicalize())
        .map_err(|e| VerbError::Failed(format!("cannot resolve the txc binary: {e}")))?;
    let state = &core.config().state_dir;
    if let Some(path) = [exe.as_path(), state.as_path(), dir.as_path()]
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
    let conf = core.config().tmux_conf_path();
    std::fs::write(&conf, conf_text(&exe, state, &dir))
        .map_err(|e| VerbError::Failed(format!("write {}: {e}", conf.display())))?;
    let mut out = format!("{}\n", conf.display());
    let ui = Ui::from_core(core);
    if ui.quiet(&["list-sessions"]).0 {
        ui.run(&["source-file", &conf.to_string_lossy()])
            .map_err(|e| VerbError::Failed(e.to_string()))?;
        out.push_str("reloaded\n");
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
                Path::new("/r/poc")
            ),
            "# tx-core server config, written by `txc ui install`. Re-run to update.\n\
             set -s exit-empty on\n\
             set -g history-limit 50000\n\
             set -g detach-on-destroy off\n\
             set -g @tx-bin \"/opt/txc\"\n\
             set -g @tx-state \"/s/state\"\n\
             source-file \"/r/poc/tmux/tmux.conf\"\n\
             run-shell \"/r/poc/tmux/tx-ide.tmux\"\n"
        );
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
