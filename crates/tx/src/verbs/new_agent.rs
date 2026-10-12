//! `_new-agent`: the prefix+n seam. In the key's popup (bin/tmux-new-agent) one form asks for the
//! agent's name, tags and prompt, then hands the spawn to a background `run-shell`, so the popup
//! closes at once; the background half spawns the agent through the `spawn` verb and reports on
//! the firing client's status line.

use std::rc::{Rc, Weak};

use cordis::BoxError;

use crate::app::{Command, CommandTable, Env};
use crate::service::SessionService;
use crate::session_editor::{Field, Form, run_form};
use crate::shlex::shlex_join;
use crate::storage::Home;

use super::common::{bin_tx, split_tags};

const DEFAULT_TAG: &str = "agent";
const TAGS_HELP: &str = "Comma-separated · empty means agent";

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
         it in the background. Usage: _new-agent PANE CLIENT [NAME TAGS PROMPT]"
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
    /// The popup half: fill the form (Esc cancels), then hand the spawn to the server and exit.
    fn ask(&self, pane: &str, client: &str) -> Result<i32, BoxError> {
        let Some([name, tags, prompt]) = run_form(new_agent_form())? else {
            return Ok(0);
        };
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
            name.trim(),
            tags.trim(),
            prompt.trim(),
        ]);
        self.service
            .tmux()
            .run_shell_background(&format!("{command} >/dev/null 2>&1 || true"))?;
        Ok(0)
    }

    /// The background half: spawn in the pane's directory and report.
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
            Ok(0) => {
                tmux.display_status(client, &format!("spawned {name}"));
                Ok(0)
            }
            Ok(code) => {
                tmux.display_status(client, &format!("spawn of {name} failed (exit {code})"));
                Ok(code)
            }
            Err(error) => {
                tmux.display_status(client, &format!("spawn of {name} failed: {error}"));
                Ok(1)
            }
        }
    }
}

/// Name and prompt are required; blank tags mean `agent`.
fn new_agent_form() -> Form<3> {
    let field = |label, help, required| Field {
        label,
        help,
        required,
        value: "",
    };
    Form::new(
        [
            field("Name", None, true),
            field("Tags", Some(TAGS_HELP), false),
            field("Prompt", None, true),
        ],
        "spawn",
    )
}

/// `tx spawn` for an engine worker (default engine) in the pane's directory.
fn spawn_argv(name: &str, tags: &str, cwd: &str, prompt: &str) -> Vec<String> {
    [
        name,
        "--tag",
        &clean_tags(tags),
        "--cwd",
        cwd,
        "--prompt",
        prompt,
    ]
    .map(str::to_owned)
    .to_vec()
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
    use crate::session_editor::{Key, Outcome};

    #[test]
    fn one_form_asks_name_tags_and_prompt() {
        let mut form = new_agent_form();
        let rows = form.render(9, 60).rows(9, 60);
        assert_eq!(rows[1], "  › Name:");
        assert_eq!(rows[3], "    Tags:");
        assert_eq!(rows[4], "              Comma-separated · empty means agent");
        assert_eq!(rows[5], "    Prompt:");
        assert_eq!(rows[8], "  ↑↓ switch   Enter spawn   Esc cancel");
        let type_text = |form: &mut Form<3>, text: &str| {
            text.chars()
                .for_each(|c| drop(form.handle(Key::Char(c), 9, 60)))
        };
        type_text(&mut form, "health");
        // Up from the first field wraps to the prompt; Enter refuses while it is blank.
        assert_eq!(form.handle(Key::Up, 9, 60), None);
        type_text(&mut form, " ");
        assert_eq!(form.handle(Key::Enter, 9, 60), None);
        assert_eq!(
            form.render(9, 60).rows(9, 60)[8],
            "  Prompt cannot be empty."
        );
        type_text(&mut form, "add GET /health");
        assert_eq!(
            form.handle(Key::Enter, 9, 60),
            Some(Outcome::Save([
                "health".into(),
                "".into(),
                " add GET /health".into()
            ]))
        );
    }

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
}
