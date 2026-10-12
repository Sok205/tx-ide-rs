//! `_new-agent`: the prefix+n seam. In the key's popup (bin/tmux-new-agent) one form asks for the
//! agent's name, prompt, engine, model, effort and tags, then hands the spawn to a background
//! `run-shell`, so the popup closes at once; the background half spawns the agent through the
//! `spawn` verb and reports on the firing client's status line.

use std::rc::{Rc, Weak};

use cordis::BoxError;

use crate::app::{Command, CommandTable, Env};
use crate::engines::DEFAULT_EFFORT;
use crate::service::SessionService;
use crate::session_editor::{Field, Form, Kind, run_form};
use crate::shlex::shlex_join;
use crate::storage::Home;

use super::common::{bin_tx, split_tags};

const DEFAULT_TAG: &str = "agent";
const TAGS_HELP: &str = "Comma-separated · empty means agent";
/// The model option that passes no `--model`: the engine picks.
const DEFAULT_MODEL: &str = "default";
/// Field index of the engine, which the model and effort lists follow.
const ENGINE_FIELD: usize = 2;
const ENGINES: &[&str] = &["claude", "codex", "antigravity"];
/// Fixed lists: claude's aliases, and the codex and agy models offered at the time of writing.
const MODELS: &[(&str, &[&str])] = &[
    (
        "claude",
        &[DEFAULT_MODEL, "fable", "opus", "sonnet", "haiku"],
    ),
    (
        "codex",
        &[
            DEFAULT_MODEL,
            "gpt-6.1-sol",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
        ],
    ),
    (
        "antigravity",
        &[
            DEFAULT_MODEL,
            "gemini-3.8-flash-high",
            "gemini-3.1-pro-high",
            "claude-opus-4-6-thinking",
            "claude-sonnet-4-6",
        ],
    ),
];
/// `--effort` tiers. agy's effort picks its flash slug, which exists for 1-3 only.
const EFFORTS: &[(&str, &[&str])] = &[
    ("claude", &["1", "2", "3", "4", "5"]),
    ("codex", &["1", "2", "3", "4", "5"]),
    ("antigravity", &["1", "2", "3"]),
];

pub struct NewAgent {
    pub service: Rc<SessionService>,
    pub env: Rc<Env>,
    pub home: Rc<Home>,
    /// Weak: the table owns the verbs.
    pub commands: Weak<CommandTable>,
}

/// The form's answers.
struct Answers<'a> {
    name: &'a str,
    prompt: &'a str,
    engine: &'a str,
    model: &'a str,
    effort: &'a str,
    tags: &'a str,
}

impl Command for NewAgent {
    fn name(&self) -> &'static str {
        "_new-agent"
    }
    fn summary(&self) -> &'static str {
        "Internal: prefix+n — ask for a new agent's name, prompt, engine, model, effort and tags \
         in the popup, then spawn it in the background. Usage: _new-agent PANE CLIENT [NAME \
         PROMPT ENGINE MODEL EFFORT TAGS]"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        match argv {
            [pane, client] => self.ask(pane, client),
            [pane, client, name, prompt, engine, model, effort, tags] => {
                let answers = Answers {
                    name,
                    prompt,
                    engine,
                    model,
                    effort,
                    tags,
                };
                self.spawn(pane, client, &answers)
            }
            _ => {
                eprintln!(
                    "usage: tx _new-agent PANE CLIENT [NAME PROMPT ENGINE MODEL EFFORT TAGS]"
                );
                Ok(2)
            }
        }
    }
}

impl NewAgent {
    /// The popup half: fill the form (Esc cancels), then hand the spawn to the server and exit.
    fn ask(&self, pane: &str, client: &str) -> Result<i32, BoxError> {
        let Some([name, prompt, engine, model, effort, tags]) = run_form(new_agent_form())? else {
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
            prompt.trim(),
            &engine,
            &model,
            &effort,
            tags.trim(),
        ]);
        self.service
            .tmux()
            .run_shell_background(&format!("{command} >/dev/null 2>&1 || true"))?;
        Ok(0)
    }

    /// The background half: spawn in the pane's directory and report.
    fn spawn(&self, pane: &str, client: &str, answers: &Answers) -> Result<i32, BoxError> {
        let tmux = self.service.tmux();
        let name = answers.name;
        let cwd = tmux
            .display_message("#{pane_current_path}", Some(pane))
            .unwrap_or_default();
        let Some(spawn) = self.commands.upgrade().and_then(|table| table.get("spawn")) else {
            tmux.display_status(client, "prefix+n: the spawn verb is disabled");
            return Ok(1);
        };
        match spawn.run(&spawn_argv(answers, &cwd)) {
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

/// Name and prompt are required; engine, model and effort are lists (claude, the engine's default
/// model, 3); blank tags mean `agent`.
fn new_agent_form() -> Form<6> {
    let effort = DEFAULT_EFFORT.level().to_string();
    let text = |label, help, required| Field {
        label,
        help,
        required,
        kind: Kind::Text,
        value: "",
    };
    let choice = |label, kind, value| Field {
        label,
        help: None,
        required: false,
        kind,
        value,
    };
    let by_engine = |table| Kind::ChoiceOf {
        field: ENGINE_FIELD,
        table,
    };
    Form::new(
        [
            text("Name", None, true),
            text("Prompt", None, true),
            choice("Engine", Kind::Choice(ENGINES), ENGINES[0]),
            choice("Model", by_engine(MODELS), DEFAULT_MODEL),
            choice("Effort", by_engine(EFFORTS), &effort),
            text("Tags", Some(TAGS_HELP), false),
        ],
        "spawn",
    )
}

/// `tx spawn` for an engine worker in the pane's directory.
fn spawn_argv(answers: &Answers, cwd: &str) -> Vec<String> {
    let tags = if answers.tags.is_empty() {
        DEFAULT_TAG
    } else {
        answers.tags
    };
    let mut argv = [
        answers.name,
        "--tag",
        &clean_tags(tags),
        "--cwd",
        cwd,
        "--engine",
        answers.engine,
        "--effort",
        answers.effort,
        "--prompt",
        answers.prompt,
    ]
    .map(str::to_owned)
    .to_vec();
    if answers.model != DEFAULT_MODEL {
        argv.extend(["--model".to_owned(), answers.model.to_owned()]);
    }
    argv
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

    const HEIGHT: usize = 17;
    const WIDTH: usize = 62;

    fn press(form: &mut Form<6>, keys: &[Key]) -> Option<Outcome<6>> {
        keys.iter().find_map(|&key| form.handle(key, HEIGHT, WIDTH))
    }

    fn typed(text: &str) -> Vec<Key> {
        text.chars().map(Key::Char).collect()
    }

    fn rows(form: &mut Form<6>) -> Vec<String> {
        form.render(HEIGHT, WIDTH).rows(HEIGHT, WIDTH)
    }

    #[test]
    fn engine_model_and_effort_are_lists_side_by_side() {
        let mut form = new_agent_form();
        let rows = rows(&mut form);
        assert_eq!(rows[1], "  › Name:");
        assert_eq!(rows[3], "    Prompt:");
        assert_eq!(
            rows[5],
            "    Engine          Model                        Effort"
        );
        assert_eq!(
            rows[6],
            "  ● claude        ● default                      1"
        );
        assert_eq!(
            rows[7],
            "    codex           fable                        2"
        );
        assert_eq!(
            rows[8],
            "    antigravity     opus                       ● 3"
        );
        assert_eq!(
            rows[10],
            "                    haiku                        5"
        );
        assert_eq!(rows[12], "    Tags:");
        assert_eq!(
            rows[13],
            "              Comma-separated · empty means agent"
        );
        assert_eq!(rows[15], "  Tab next   ↑↓ move   Enter spawn   Esc cancel");
    }

    #[test]
    fn arrows_move_one_row_tab_jumps_and_enter_picks() {
        let mut form = new_agent_form();
        // ↓ from Name to Prompt, ↓ onto the engine list (claude), ↓ to codex, Enter picks it.
        let mut keys = typed("health");
        keys.extend([Key::Down, Key::Down, Key::Down, Key::Enter]);
        press(&mut form, &keys);
        assert_eq!(
            rows(&mut form)[7],
            "  ● codex           gpt-6.1-sol                  2"
        );
        // → to the model column on the same row, Space picks; → to effort, ↓↓ to 5, Enter.
        press(
            &mut form,
            &[
                Key::Right,
                Key::Char(' '),
                Key::Right,
                Key::Down,
                Key::Down,
                Key::Down,
            ],
        );
        press(&mut form, &[Key::Enter]);
        // ↓ past the bottom of a list leaves it for Tags; Tab wraps to Name, Shift-Tab back.
        press(&mut form, &[Key::Down]);
        assert_eq!(rows(&mut form)[12], "  › Tags:");
        press(&mut form, &typed("api"));
        // Enter on a text field spawns — but the prompt is still blank.
        assert_eq!(press(&mut form, &[Key::Enter]), None);
        assert_eq!(rows(&mut form)[15], "  Prompt cannot be empty.");
        let mut keys = typed("add GET /health");
        keys.push(Key::Enter);
        assert_eq!(
            press(&mut form, &keys),
            Some(Outcome::Save([
                "health".into(),
                "add GET /health".into(),
                "codex".into(),
                "gpt-6.1-sol".into(),
                "5".into(),
                "api".into(),
            ]))
        );
    }

    #[test]
    fn a_new_engine_keeps_the_model_and_effort_it_offers() {
        let mut form = new_agent_form();
        // Effort 5, then engine antigravity: it has no 5 (falls back to 1) and a different list.
        press(
            &mut form,
            &[
                Key::Char('\t'),
                Key::Char('\t'),
                Key::Char('\t'),
                Key::Char('\t'),
            ],
        );
        press(&mut form, &[Key::Down, Key::Down, Key::Enter]);
        press(
            &mut form,
            &[Key::BackTab, Key::BackTab, Key::Down, Key::Down, Key::Enter],
        );
        let rows = rows(&mut form);
        assert_eq!(
            rows[6],
            "    claude        ● default                    ● 1"
        );
        assert_eq!(
            rows[7],
            "    codex           gemini-3.8-flash-high        2"
        );
        assert_eq!(
            rows[8],
            "  ● antigravity     gemini-3.1-pro-high          3"
        );
        assert_eq!(rows[9], "                    claude-opus-4-6-thinking");
    }

    #[test]
    fn spawns_an_engine_worker_in_the_pane_directory() {
        let answers = Answers {
            name: "health",
            prompt: "add GET /health",
            engine: "codex",
            model: "gpt-6-sol",
            effort: "4",
            tags: "tutor, api",
        };
        assert_eq!(
            spawn_argv(&answers, "/p/my proj"),
            [
                "health",
                "--tag",
                "tutor,api",
                "--cwd",
                "/p/my proj",
                "--engine",
                "codex",
                "--effort",
                "4",
                "--prompt",
                "add GET /health",
                "--model",
                "gpt-6-sol"
            ]
        );
        let defaults = Answers {
            model: DEFAULT_MODEL,
            tags: "",
            ..answers
        };
        let argv = spawn_argv(&defaults, "/p");
        assert!(!argv.contains(&"--model".to_owned()));
        assert_eq!(argv[2], DEFAULT_TAG);
    }
}
