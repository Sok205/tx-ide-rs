//! `_new-agent`: the prefix+n seam. The key's tmux prompt leaves name / tags / prompt in three
//! buffers; this spawns the agent through the `spawn` verb, tells the tx-assistant, and reports
//! on the firing client's status line.

use std::rc::{Rc, Weak};

use cordis::BoxError;

use crate::app::{Command, CommandTable};
use crate::service::SessionService;

use super::common::split_tags;

/// The buffers prefix+n's prompt writes (tmux/tx-ide.tmux).
const BUFFERS: [&str; 3] = [
    "tx-new-agent-name",
    "tx-new-agent-tags",
    "tx-new-agent-prompt",
];
const ASSISTANT: &str = "tx-assistant";

pub struct NewAgent {
    pub service: Rc<SessionService>,
    /// Weak: the table owns the verbs.
    pub commands: Weak<CommandTable>,
}

impl Command for NewAgent {
    fn name(&self) -> &'static str {
        "_new-agent"
    }
    fn summary(&self) -> &'static str {
        "Internal: prefix+n — spawn the agent named in the tx-new-agent-* buffers, tell the \
         tx-assistant. Usage: _new-agent PANE CLIENT"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let [pane, client] = argv else {
            eprintln!("usage: tx _new-agent PANE CLIENT");
            return Ok(2);
        };
        let tmux = self.service.tmux();
        let [name, tags, prompt] = BUFFERS.map(|buffer| {
            let value = tmux.show_buffer(buffer).unwrap_or_default();
            tmux.delete_buffer(buffer);
            value.trim().to_owned()
        });
        if name.is_empty() || prompt.is_empty() {
            tmux.display_status(client, "prefix+n: a name and a prompt are required");
            return Ok(1);
        }
        let tags = if tags.is_empty() { "agent".to_owned() } else { tags };
        let cwd = tmux
            .display_message("#{pane_current_path}", Some(pane))
            .unwrap_or_default();
        let Some(spawn) = self.commands.upgrade().and_then(|table| table.get("spawn")) else {
            tmux.display_status(client, "prefix+n: the spawn verb is disabled");
            return Ok(1);
        };
        match spawn.run(&spawn_argv(&name, &tags, &cwd, &prompt)) {
            Ok(0) => {}
            Ok(code) => {
                tmux.display_status(client, &format!("prefix+n: spawn failed (exit {code})"));
                return Ok(code);
            }
            Err(error) => {
                tmux.display_status(client, &format!("prefix+n: {error}"));
                return Ok(1);
            }
        }
        let status = match self
            .service
            .send_message(ASSISTANT, &notice(&name, &tags, &cwd, &prompt))
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
