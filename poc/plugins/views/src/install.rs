//! `ui install` and `ui attach`: wire the IDE's tmux config into the core's server.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tx_core::{TxCore, Verb, VerbError, VerbIo};

use crate::tmux::Ui;

const FRAGMENT: &str = "tmux/tx-ide.tmux";
const HOME_SESSION: &str = "home";

fn conf_text(exe: &Path, state: &Path, dir: &Path) -> String {
    format!(
        "# tx-core server config, written by `txc ui install`. Re-run to update.\n\
         set -s exit-empty on\n\
         set -g history-limit 50000\n\
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

fn attach(core: &TxCore, io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".to_owned());
    let ui = Ui::from_core(core);
    let _ = io.stdout.flush();
    let error = ui
        .command(&["new-session", "-A", "-s", HOME_SESSION, "-c", &home])
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
             set -g @tx-bin \"/opt/txc\"\n\
             set -g @tx-state \"/s/state\"\n\
             source-file \"/r/poc/tmux/tmux.conf\"\n\
             run-shell \"/r/poc/tmux/tx-ide.tmux\"\n"
        );
    }
}
