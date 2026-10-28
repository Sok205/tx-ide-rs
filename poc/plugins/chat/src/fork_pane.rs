//! `_fork-pane`: the prefix+f seam. In the key's popup (bin/tmux-fork-tx-session) it asks for the
//! fork's name (blank: the `<source>-fork` default) and hands the fork to a background
//! `run-shell`, so the popup closes at once; the background half forks the tx session shown in
//! the firing pane and reports the outcome on the firing client's status line.

use std::sync::Arc;

use tx_core::{CoreError, Session, TxCore, Verb, VerbError, VerbIo};
use tx_ui::prompt::prompt_with_default;
use tx_ui::render::{ACCENT_ANSI, BOLD, RESET};
use tx_ui::tmux::Ui;

use crate::ops::ops;

pub fn verbs() -> Vec<Arc<dyn Verb>> {
    vec![Arc::new(ForkPane)]
}

struct ForkPane;

impl Verb for ForkPane {
    fn name(&self) -> &'static str {
        "_fork-pane"
    }

    fn usage(&self) -> &'static str {
        "_fork-pane PANE CLIENT [NAME]   prefix+f: fork the pane's tx session (internal)"
    }

    fn run(&self, core: &TxCore, _io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv {
            [pane, client] => ask(core, pane, client),
            [pane, client, name] => fork(core, pane, client, name),
            _ => Err(self.usage_error()),
        }
    }
}

/// The popup half: ask for the name, then hand the fork to the server and exit.
fn ask(core: &TxCore, pane: &str, client: &str) -> Result<i32, VerbError> {
    let ui = Ui::from_core(core);
    let Some(source) = source_in_pane(core, &ui, pane, client)? else {
        return Ok(1);
    };
    let label = format!(
        "Fork {ACCENT_ANSI}{BOLD}{}{RESET} as (default: {}-fork)",
        source.name, source.name
    );
    let name = prompt_with_default(&label, "")
        .map_err(|e| VerbError::Failed(format!("could not run fzf: {e}")))?;
    let Some(name) = name else {
        return Ok(0);
    };
    // A nonzero exit would make `run-shell` echo the whole command; the outcome is already on
    // the status line.
    let fork = tx_ui::txc::shell(core, &["_fork-pane", pane, client, name.trim()])?;
    let command = format!("{fork} || true");
    ui.run_shell_background(&command)
        .map_err(VerbError::failed)?;
    Ok(0)
}

/// The background half: fork and report on the firing client's status line.
fn fork(core: &TxCore, pane: &str, client: &str, name: &str) -> Result<i32, VerbError> {
    let ui = Ui::from_core(core);
    let Some(source) = source_in_pane(core, &ui, pane, client)? else {
        return Ok(1);
    };
    let forked = ops(core)?.fork(source.id.as_str(), Some(name));
    match forked {
        Ok(forked) => {
            ui.display_status(client, &format!("forked {} → {}", source.name, forked.name));
            Ok(0)
        }
        Err(error) => {
            ui.display_status(client, &format!("fork of {} failed: {error}", source.name));
            Ok(1)
        }
    }
}

/// The session nested in the pane, else the pane's own session (a direct attach); `None`
/// (reported on the status line) for a view pane or an untracked session.
fn source_in_pane(
    core: &TxCore,
    ui: &Ui,
    pane: &str,
    client: &str,
) -> Result<Option<Session>, VerbError> {
    let target = ui
        .inner_for_pane(pane)
        .or_else(|| ui.display_message("#{session_name}", Some(pane)))
        .filter(|target| !ui.is_view(target));
    let source = match target.map(|target| core.resolve_session(&target)) {
        Some(Ok(session)) => Some(session),
        Some(Err(CoreError::NotFound(_))) | None => None,
        Some(Err(error)) => return Err(error.into()),
    };
    if source.is_none() {
        ui.display_status(client, "prefix+f: no tx session in this pane");
    }
    Ok(source)
}
