//! `_fork-pane`: the prefix+F seam. The key's tmux prompt leaves the fork's name (blank: the
//! `fork` verb's `<source>-fork` default) in a buffer; this forks the tx session shown in the
//! firing pane through the `fork` verb, tells the tx-assistant, and reports on the firing
//! client's status line.

use std::rc::{Rc, Weak};

use cordis::BoxError;

use crate::app::{Command, CommandTable};
use crate::service::SessionService;

/// The buffer prefix+F's prompt writes (tmux/tx-ide.tmux).
const BUFFER: &str = "tx-fork-name";
const ASSISTANT: &str = "tx-assistant";

pub struct ForkPane {
    pub service: Rc<SessionService>,
    /// Weak: the table owns the verbs.
    pub commands: Weak<CommandTable>,
}

impl Command for ForkPane {
    fn name(&self) -> &'static str {
        "_fork-pane"
    }
    fn summary(&self) -> &'static str {
        "Internal: prefix+F — fork the pane's tx session (name from the tx-fork-name buffer), \
         tell the tx-assistant. Usage: _fork-pane PANE CLIENT"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let [pane, client] = argv else {
            eprintln!("usage: tx _fork-pane PANE CLIENT");
            return Ok(2);
        };
        let tmux = self.service.tmux();
        let name = tmux
            .show_buffer(BUFFER)
            .unwrap_or_default()
            .trim()
            .to_owned();
        tmux.delete_buffer(BUFFER);
        // The session nested in the pane, else the pane's own session (a direct attach).
        let target = tmux
            .inner_for_pane(pane)
            .or_else(|| tmux.display_message("#{session_name}", Some(pane)))
            .filter(|target| !tmux.is_view(target));
        let Some(source) = target
            .map(|target| self.service.get(&target))
            .transpose()?
            .flatten()
        else {
            tmux.display_status(client, "prefix+F: no tx session in this pane");
            return Ok(1);
        };
        let Some(fork) = self.commands.upgrade().and_then(|table| table.get("fork")) else {
            tmux.display_status(client, "prefix+F: the fork verb is disabled");
            return Ok(1);
        };
        match fork.run(&fork_argv(&source.id, &name)) {
            Ok(0) => {}
            Ok(code) => {
                tmux.display_status(client, &format!("prefix+F: fork failed (exit {code})"));
                return Ok(code);
            }
            Err(error) => {
                tmux.display_status(client, &format!("prefix+F: {error}"));
                return Ok(1);
            }
        }
        let status = match self
            .service
            .send_message(ASSISTANT, &notice(&source.name, &name))
        {
            Ok(()) => format!("forked {}; tx-assistant notified", source.name),
            Err(_) => format!(
                "forked {} (tx-assistant not running — not notified)",
                source.name
            ),
        };
        tmux.display_status(client, &status);
        Ok(0)
    }
}

/// `tx fork` on the source's stable id; a blank name leaves the verb's default.
fn fork_argv(source_id: &str, name: &str) -> Vec<String> {
    let mut argv = vec![source_id.to_owned()];
    if !name.is_empty() {
        argv.push(name.to_owned());
    }
    argv
}

/// What the tx-assistant is told.
fn notice(source: &str, name: &str) -> String {
    if name.is_empty() {
        format!("forked session {source} (default fork name)")
    } else {
        format!("forked session {source} as {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_name_leaves_the_fork_verb_default() {
        assert_eq!(fork_argv("abc123", ""), ["abc123"]);
        assert_eq!(fork_argv("abc123", "api-alt"), ["abc123", "api-alt"]);
    }

    #[test]
    fn the_assistant_hears_what_was_forked() {
        assert_eq!(notice("api", "api-alt"), "forked session api as api-alt");
        assert_eq!(notice("api", ""), "forked session api (default fork name)");
    }
}
