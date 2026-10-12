//! `_fork-pane`: the prefix+f seam. In the key's popup (bin/tmux-fork-tx-session) it asks for the
//! fork's name (blank: the `<source>-fork` default) and hands the fork to a background
//! `run-shell`, so the popup closes at once; the background half forks the tx session shown in
//! the firing pane and reports the outcome on the firing client's status line.

use cordis::BoxError;

use crate::app::Command;
use crate::palette::{ACCENT_ANSI, BOLD, RESET};
use crate::session::Session;
use crate::shlex::shlex_join;

use super::chat::Deps;
use super::common::bin_tx;
use super::listing::prompt_with_default;

pub struct ForkPane(pub(super) Deps);

impl Command for ForkPane {
    fn name(&self) -> &'static str {
        "_fork-pane"
    }
    fn summary(&self) -> &'static str {
        "Internal: prefix+f — ask for a fork name in the popup, then fork the pane's tx session \
         in the background. Usage: _fork-pane PANE CLIENT [NAME]"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        match argv {
            [pane, client] => self.ask(pane, client),
            [pane, client, name] => self.fork(pane, client, name),
            _ => {
                eprintln!("usage: tx _fork-pane PANE CLIENT [NAME]");
                Ok(2)
            }
        }
    }
}

impl ForkPane {
    /// The popup half: ask for the name, then hand the fork to the server and exit.
    fn ask(&self, pane: &str, client: &str) -> Result<i32, BoxError> {
        let Some(source) = self.source_in_pane(pane, client)? else {
            return Ok(1);
        };
        let label = format!(
            "Fork {ACCENT_ANSI}{BOLD}{}{RESET} as (default: {}-fork)",
            source.name, source.name
        );
        let Some(name) = prompt_with_default(&label, "")? else {
            return Ok(0);
        };
        // `run-shell` runs in the server's environment: bake the home into the command.
        let home = format!("TX_IDE_HOME={}", self.0.home.root().display());
        let tx = bin_tx(&self.0.env).display().to_string();
        let command = shlex_join(&["env", &home, &tx, "_fork-pane", pane, client, name.trim()]);
        self.0.service.tmux().run_shell_background(&command)?;
        Ok(0)
    }

    /// The background half: fork and report on the firing client's status line.
    fn fork(&self, pane: &str, client: &str, name: &str) -> Result<i32, BoxError> {
        let Some(source) = self.source_in_pane(pane, client)? else {
            return Ok(1);
        };
        let tmux = self.0.service.tmux();
        match self.0.ops().fork(&source.id, Some(name), false, None) {
            Ok(forked) => {
                tmux.display_status(client, &format!("forked {} → {}", source.name, forked.name));
                Ok(0)
            }
            Err(error) => {
                tmux.display_status(client, &format!("fork of {} failed: {error}", source.name));
                Ok(1)
            }
        }
    }

    /// The session nested in the pane, else the pane's own session (a direct attach); `None`
    /// (reported on the status line) for a view pane or an untracked session.
    fn source_in_pane(&self, pane: &str, client: &str) -> Result<Option<Session>, BoxError> {
        let tmux = self.0.service.tmux();
        let target = tmux
            .inner_for_pane(pane)
            .or_else(|| tmux.display_message("#{session_name}", Some(pane)))
            .filter(|target| !tmux.is_view(target));
        let source = target
            .map(|target| self.0.service.get(&target))
            .transpose()?
            .flatten();
        if source.is_none() {
            tmux.display_status(client, "prefix+f: no tx session in this pane");
        }
        Ok(source)
    }
}
