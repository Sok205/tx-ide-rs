//! `_fork-pane`: the prefix+f seam. In the key's popup (bin/tmux-fork-tx-session) it asks for the
//! fork's name (blank: the `<source>-fork` default) and hands the fork to a background
//! `run-shell`, so the popup closes at once; the background half forks the tx session shown in
//! the firing pane through `TxCore::fork_agent` (a new worktree, the source's engine, the
//! fixed first prompt below) and reports the outcome on the firing client's status line.

use std::path::Path;
use std::sync::Arc;

use tx_client::prompt::prompt_with_default;
use tx_client::render::{ACCENT_ANSI, BOLD, RESET};
use tx_client::tmux::{Ui, shell_quote};
use tx_core::{Agent, CoreError, ForkOptions, TxCore, Verb, VerbError, VerbIo};

/// What a fork made from the key is first told; `{source}` is the source's name. Core has no
/// default prompt, and the popup asks only for a name.
pub const FORK_PROMPT: &str = "You are a fork of {source}. Introduce yourself.";

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

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv {
            [pane, client] => ask(core, pane, client),
            [pane, client, name] => fork(core, pane, client, name),
            _ => {
                let _ = writeln!(io.stderr, "usage: tx _fork-pane PANE CLIENT [NAME]");
                Ok(2)
            }
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
    let command = background_command(
        &tx_plugin::sdk::tx_bin(),
        &core.config().cli_arguments(),
        pane,
        client,
        name.trim(),
    );
    ui.run_shell_background(&command)
        .map_err(|e| VerbError::Failed(e.to_string()))?;
    Ok(0)
}

/// What tmux runs later for the background half. It starts with `tx`, whatever program this
/// code runs in: `run-shell` gives it neither this program nor its environment. A nonzero exit
/// would make `run-shell` echo the whole command; the outcome is already on the status line.
fn background_command(
    tx: &Path,
    arguments: &[String],
    pane: &str,
    client: &str,
    name: &str,
) -> String {
    let arguments: Vec<String> = arguments.iter().map(|word| shell_quote(word)).collect();
    format!(
        "{} {} _fork-pane {} {} {} || true",
        shell_quote(&tx.display().to_string()),
        arguments.join(" "),
        shell_quote(pane),
        shell_quote(client),
        shell_quote(name),
    )
}

/// The background half: fork and report on the firing client's status line.
fn fork(core: &TxCore, pane: &str, client: &str, name: &str) -> Result<i32, VerbError> {
    let ui = Ui::from_core(core);
    let Some(source) = source_in_pane(core, &ui, pane, client)? else {
        return Ok(1);
    };
    let mut options = ForkOptions::new(FORK_PROMPT.replace("{source}", &source.name));
    if !name.is_empty() {
        options = options.name(name);
    }
    match core.fork_agent(&source.id, options) {
        Ok(forked) => {
            let forked = core.get_agent(&forked)?;
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
) -> Result<Option<Agent>, VerbError> {
    let target = ui
        .inner_for_pane(pane)
        .or_else(|| ui.display_message("#{session_name}", Some(pane)))
        .filter(|target| !ui.is_view(target));
    let source = match target.map(|target| core.resolve(&target)) {
        Some(Ok(id)) => Some(core.get_agent(&id)?),
        Some(Err(CoreError::NotFound(_))) | None => None,
        Some(Err(error)) => return Err(error.into()),
    };
    if source.is_none() {
        ui.display_status(client, "prefix+f: no tx session in this pane");
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_background_command_starts_with_the_tx_binary() {
        let command = background_command(
            Path::new("/opt/my tx/tx"),
            &["--config-dir".to_owned(), "/c".to_owned()],
            "%3",
            "/dev/ttys001",
            "c2",
        );
        assert_eq!(
            command,
            "'/opt/my tx/tx' '--config-dir' '/c' _fork-pane '%3' '/dev/ttys001' 'c2' || true"
        );
    }
}
