//! `_new-agent`: the prefix+n seam. In the key's popup (bin/tmux-new-agent) it asks for the
//! agent's name, tags and prompt, then hands the spawn to a background `run-shell`, so the popup
//! closes at once; the background half spawns the agent through the `spawn` verb, tells the
//! tx-assistant, and reports on the firing client's status line.

use std::rc::{Rc, Weak};

use cordis::BoxError;

use crate::app::{Command, CommandTable, Env};
use crate::palette::{ACCENT_ANSI, BOLD, RESET};
use crate::service::SessionService;
use crate::shlex::shlex_join;
use crate::storage::Home;

use super::common::{bin_tx, split_tags};
use super::listing::prompt_with_default;

const ASSISTANT: &str = "tx-assistant";
const DEFAULT_TAG: &str = "agent";

pub struct NewAgent {
    pub service: Rc<SessionService>,
    pub env: Rc<Env>,
    pub home: Rc<Home>,
    /// Weak: the table owns the verbs.
    pub commands: Weak<CommandTable>,
}

impl Command for NewAgent {
    fn name(&self) -> &'static str {
        "_new-agent"
    }
    fn summary(&self) -> &'static str {
        "Internal: prefix+n — ask for a new agent's name, tags and prompt in the popup, then spawn \
         it in the background and tell the tx-assistant. Usage: _new-agent PANE CLIENT \
         [NAME TAGS PROMPT]"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        match argv {
            [pane, client] => self.ask(pane, client),
            [pane, client, name, tags, prompt] => self.spawn(pane, client, name, tags, prompt),
            _ => {
                eprintln!("usage: tx _new-agent PANE CLIENT [NAME TAGS PROMPT]");
                Ok(2)
            }
        }
    }
}

impl NewAgent {
    /// The popup half: ask name, tags and prompt in turn (Esc at any step cancels), then hand the
    /// spawn to the server and exit.
    fn ask(&self, pane: &str, client: &str) -> Result<i32, BoxError> {
        let tmux = self.service.tmux();
        let Some(name) = prompt_with_default("New agent name", "")? else {
            return Ok(0);
        };
        let name = name.trim();
        if name.is_empty() {
            tmux.display_status(client, "prefix+n: a name is required");
            return Ok(1);
        }
        let agent = format!("{ACCENT_ANSI}{BOLD}{name}{RESET}");
        let tags_label = format!("Tags for {agent}, comma-separated (default: {DEFAULT_TAG})");
        let Some(tags) = prompt_with_default(&tags_label, "")? else {
            return Ok(0);
        };
        let Some(prompt) = prompt_with_default(&format!("Prompt for {agent}"), "")? else {
            return Ok(0);
        };
        let prompt = prompt.trim();
        if prompt.is_empty() {
            tmux.display_status(client, "prefix+n: a prompt is required");
            return Ok(1);
        }
        // `run-shell` runs in the server's environment: bake the home into the command. Its
        // stdout would open in view mode and a nonzero exit would echo the whole command, so the
        // spawn verb's report and exit status are dropped — the outcome is on the status line.
        let home = format!("TX_IDE_HOME={}", self.home.root().display());
        let tx = bin_tx(&self.env).display().to_string();
        let command = shlex_join(&[
            "env",
            &home,
            &tx,
            "_new-agent",
            pane,
            client,
            name,
            tags.trim(),
            prompt,
        ]);
        tmux.run_shell_background(&format!("{command} >/dev/null 2>&1 || true"))?;
        Ok(0)
    }

    /// The background half: spawn in the pane's directory, tell the tx-assistant, report.
    fn spawn(
        &self,
        pane: &str,
        client: &str,
        name: &str,
        tags: &str,
        prompt: &str,
    ) -> Result<i32, BoxError> {
        let tmux = self.service.tmux();
        let tags = if tags.is_empty() { DEFAULT_TAG } else { tags };
        let cwd = tmux
            .display_message("#{pane_current_path}", Some(pane))
            .unwrap_or_default();
        let Some(spawn) = self.commands.upgrade().and_then(|table| table.get("spawn")) else {
            tmux.display_status(client, "prefix+n: the spawn verb is disabled");
            return Ok(1);
        };
        match spawn.run(&spawn_argv(name, tags, &cwd, prompt)) {
            Ok(0) => {}
            Ok(code) => {
                tmux.display_status(client, &format!("spawn of {name} failed (exit {code})"));
                return Ok(code);
            }
            Err(error) => {
                tmux.display_status(client, &format!("spawn of {name} failed: {error}"));
                return Ok(1);
            }
        }
        let status = match self
            .service
            .send_message(ASSISTANT, &notice(name, tags, &cwd, prompt))
        {
            Ok(()) => format!("spawned {name}; tx-assistant notified"),
            Err(_) => format!("spawned {name} (tx-assistant not running — not notified)"),
        };
        tmux.display_status(client, &status);
        Ok(0)
    }
}

/// `tx spawn` for an engine worker (default engine) in the pane's directory.
fn spawn_argv(name: &str, tags: &str, cwd: &str, prompt: &str) -> Vec<String> {
    [name, "--tag", &clean_tags(tags), "--cwd", cwd, "--prompt", prompt]
        .map(str::to_owned)
        .to_vec()
}

/// What the tx-assistant is told.
fn notice(name: &str, tags: &str, cwd: &str, prompt: &str) -> String {
    format!(
        "spawned agent {name} (tags {}, cwd {cwd}): {prompt}",
        clean_tags(tags)
    )
}

/// `tutor, api` → `tutor,api`.
fn clean_tags(tags: &str) -> String {
    split_tags(tags)
        .iter()
        .map(|tag| tag.trim())
        .filter(|tag| !tag.is_empty())
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawns_an_engine_worker_in_the_pane_directory() {
        assert_eq!(
            spawn_argv("health", "tutor, api", "/p/my proj", "add GET /health"),
            [
                "health",
                "--tag",
                "tutor,api",
                "--cwd",
                "/p/my proj",
                "--prompt",
                "add GET /health"
            ]
        );
    }

    #[test]
    fn the_assistant_hears_what_was_spawned_where() {
        assert_eq!(
            notice("health", "tutor, api", "/p", "add GET /health"),
            "spawned agent health (tags tutor,api, cwd /p): add GET /health"
        );
    }
}
