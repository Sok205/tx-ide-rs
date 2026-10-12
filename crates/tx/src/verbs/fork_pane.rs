//! `_fork-pane`: the prefix+f seam, run inside the key's popup (bin/tmux-fork-tx-session). It
//! asks for the fork's name (prefilled `<source>-fork`), forks the tx session shown in the firing
//! pane through the `fork` verb, and reports the outcome on the firing client's status line —
//! the popup is gone by then.

use std::rc::{Rc, Weak};

use cordis::BoxError;

use crate::app::{Command, CommandTable};
use crate::palette::{ACCENT_ANSI, BOLD, RESET};
use crate::service::SessionService;
use crate::session::Session;

use super::listing::prompt_with_default;

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
        "Internal: prefix+f — fork the pane's tx session (name asked in the popup). \
         Usage: _fork-pane PANE CLIENT"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let [pane, client] = argv else {
            eprintln!("usage: tx _fork-pane PANE CLIENT");
            return Ok(2);
        };
        let tmux = self.service.tmux();
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
            tmux.display_status(client, "prefix+f: no tx session in this pane");
            return Ok(1);
        };
        let Some(fork) = self.commands.upgrade().and_then(|table| table.get("fork")) else {
            tmux.display_status(client, "prefix+f: the fork verb is disabled");
            return Ok(1);
        };
        let label = format!("Fork {ACCENT_ANSI}{BOLD}{}{RESET} as", source.name);
        let Some(name) = prompt_with_default(&label, &format!("{}-fork", source.name))? else {
            return Ok(0);
        };
        let name = name.trim();
        let earlier_forks: Vec<String> = self
            .forks_of(&source.id)
            .into_iter()
            .map(|fork| fork.id)
            .collect();
        match fork.run(&fork_argv(&source.id, name)) {
            Ok(0) => {
                let forked = self
                    .forks_of(&source.id)
                    .into_iter()
                    .find(|fork| !earlier_forks.contains(&fork.id))
                    .expect("the fork verb records the new session");
                tmux.display_status(client, &format!("forked {} → {}", source.name, forked.name));
                Ok(0)
            }
            Ok(code) => {
                tmux.display_status(
                    client,
                    &format!("fork of {} failed (exit {code})", source.name),
                );
                Ok(code)
            }
            Err(error) => {
                tmux.display_status(client, &format!("fork of {} failed: {error}", source.name));
                Ok(1)
            }
        }
    }
}

impl ForkPane {
    /// The sessions forked (parented) from `source_id`.
    fn forks_of(&self, source_id: &str) -> Vec<Session> {
        self.service
            .store()
            .all()
            .into_iter()
            .filter(|session| session.parent.as_deref() == Some(source_id))
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_name_leaves_the_fork_verb_default() {
        assert_eq!(fork_argv("abc123", ""), ["abc123"]);
        assert_eq!(fork_argv("abc123", "api-alt"), ["abc123", "api-alt"]);
    }
}
