//! Re-invoking this `txc` from tmux binds, popups, `run-shell` and detached children: the same
//! binary against the same state dir and, when one is set, the same tmux socket. tmux runs those
//! commands in the server's environment, so nothing may be left to `$TX_TMUX_SOCKET`.

use std::path::{Path, PathBuf};
use std::process::Command;

use tx_core::{CoreConfig, TxCore, VerbError};

use crate::tmux::shell_quote;

/// This binary.
pub fn exe() -> Result<PathBuf, VerbError> {
    std::env::current_exe().map_err(|error| VerbError::Failed(format!("current_exe: {error}")))
}

/// `--state-dir <state> [--socket <socket>]`, the globals every re-invocation carries.
fn globals(config: &CoreConfig) -> Vec<&Path> {
    let mut words = vec![Path::new("--state-dir"), config.state_dir.as_path()];
    if let Some(socket) = &config.tmux_socket {
        words.extend([Path::new("--socket"), socket.as_path()]);
    }
    words
}

/// `exe <globals> args…` as one shell-quoted line, for tmux binds, popups and `run-shell`.
pub fn shell_line(exe: &Path, config: &CoreConfig, args: &[&str]) -> String {
    let mut words = vec![shell_quote(&exe.to_string_lossy())];
    words.extend(
        globals(config)
            .iter()
            .map(|word| shell_quote(&word.to_string_lossy())),
    );
    words.extend(args.iter().map(|arg| shell_quote(arg)));
    words.join(" ")
}

/// `exe <globals> args…` as a command to spawn.
pub fn command(exe: &Path, config: &CoreConfig, args: &[&str]) -> Command {
    let mut command = Command::new(exe);
    command.args(globals(config)).args(args);
    command
}

/// [`shell_line`] for this binary and `core`'s configuration.
pub fn shell(core: &TxCore, args: &[&str]) -> Result<String, VerbError> {
    Ok(shell_line(&exe()?, core.config(), args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_socket_travels_with_the_state_dir_and_everything_is_quoted() {
        let plain = CoreConfig::new("/s/my state");
        assert_eq!(
            shell_line(Path::new("/opt/txc"), &plain, &["_new-agent", "%3"]),
            "'/opt/txc' '--state-dir' '/s/my state' '_new-agent' '%3'"
        );
        let socket = CoreConfig::new("/s").tmux_socket("/k/tmux.sock");
        assert_eq!(
            shell_line(Path::new("/opt/txc"), &socket, &[]),
            "'/opt/txc' '--state-dir' '/s' '--socket' '/k/tmux.sock'"
        );
        let spawned = command(Path::new("/opt/txc"), &socket, &["ls"]);
        let args: Vec<_> = spawned.get_args().collect();
        assert_eq!(
            args,
            ["--state-dir", "/s", "--socket", "/k/tmux.sock", "ls"]
        );
    }
}
