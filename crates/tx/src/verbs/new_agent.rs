//! `_new-agent`: the prefix+n seam. In the key's popup (bin/tmux-new-agent) an nvim form
//! (nvim/lua/tx-new-agent.lua) asks for the agent's name, prompt, project, engine, model, effort,
//! role and tags, then this hands the spawn to a background `run-shell`, so the popup closes at
//! once; the background half spawns the agent through the `spawn` verb and reports on the firing
//! client's status line.
//!
//! The projects are `config.json`'s `projects` (preselected: `default_project`, else the first);
//! without any, the firing pane's directory is the only choice.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::rc::{Rc, Weak};

use cordis::BoxError;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::app::{Command, CommandTable, Env};
use crate::engines::DEFAULT_EFFORT;
use crate::engines::antigravity::AGY_MODEL_FAMILY;
use crate::roles::role_names;
use crate::service::SessionService;
use crate::shlex::shlex_join;
use crate::storage::{Config, Home, Projects};

use super::common::{bin_tx, repo_root, split_tags};

const DEFAULT_TAG: &str = "agent";
/// The role option that passes no `--role`: COMMON alone.
const NO_ROLE: &str = "none";
/// Preselected when the home has it (it ships in agents/), else none.
const DEFAULT_ROLE: &str = "DEVELOPER";
const TAGS_HELP: &str = "Comma-separated (default: agent)";
const ENGINES: &[&str] = &["claude", "codex", "antigravity"];
/// Fixed lists: claude's aliases, and the codex and agy models offered at the time of writing.
/// Each starts with what the engine runs without `--model` (claude's and codex's configured model,
/// tx's agy flash family), preselected and spawned without `--model`.
const MODELS: &[(&str, &[&str])] = &[
    ("claude", &["fable", "opus", "sonnet", "haiku"]),
    (
        "codex",
        &["gpt-6-astra", "gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna"],
    ),
    (
        "antigravity",
        &[
            AGY_MODEL_FAMILY,
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

/// The form's answers, as nvim/lua/tx-new-agent.lua writes them; `cwd` is the picked project's
/// path.
#[derive(Deserialize)]
struct Reply {
    name: String,
    prompt: String,
    engine: String,
    model: String,
    effort: String,
    role: String,
    tags: String,
    cwd: String,
}

/// The form's answers.
struct Answers<'a> {
    name: &'a str,
    prompt: &'a str,
    engine: &'a str,
    model: &'a str,
    effort: &'a str,
    role: &'a str,
    tags: &'a str,
    cwd: &'a str,
}

impl Command for NewAgent {
    fn name(&self) -> &'static str {
        "_new-agent"
    }
    fn summary(&self) -> &'static str {
        "Internal: prefix+n — ask for a new agent's name, prompt, project, engine, model, effort, \
         role and tags in the popup, then spawn it in the background. Usage: _new-agent PANE \
         CLIENT | _new-agent CLIENT NAME PROMPT ENGINE MODEL EFFORT ROLE TAGS CWD"
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        match argv {
            [pane, client] => self.ask(pane, client),
            [client, name, prompt, engine, model, effort, role, tags, cwd] => {
                let answers = Answers {
                    name,
                    prompt,
                    engine,
                    model,
                    effort,
                    role,
                    tags,
                    cwd,
                };
                self.spawn(client, &answers)
            }
            _ => {
                eprintln!(
                    "usage: tx _new-agent PANE CLIENT | tx _new-agent CLIENT NAME PROMPT ENGINE \
                     MODEL EFFORT ROLE TAGS CWD"
                );
                Ok(2)
            }
        }
    }
}

impl NewAgent {
    /// The popup half: fill the nvim form (Esc cancels), then hand the spawn to the server and
    /// exit.
    fn ask(&self, pane: &str, client: &str) -> Result<i32, BoxError> {
        let tmux = self.service.tmux();
        let projects = match self.projects(pane) {
            Ok(projects) => projects,
            Err(error) => {
                tmux.display_status(client, &format!("prefix+n: {error}"));
                return Ok(1);
            }
        };
        let directory = std::env::temp_dir().join(format!("tx-new-agent-{}", process::id()));
        fs::create_dir_all(&directory)?;
        let result = directory.join("answers.json");
        let spec = directory.join("spec.json");
        fs::write(
            &spec,
            form_spec(&result, &role_names(&self.home), &projects).to_string(),
        )?;
        // --clean: the form brings its own look, and no user config (dashboard, plugins) runs
        // before it. The module ships with this tx, so its `nvim/` leads the runtimepath.
        process::Command::new("nvim")
            .args([
                "--clean",
                "-n",
                "-i",
                "NONE",
                "--cmd",
                "set shortmess+=I | lua vim.opt.rtp:prepend(vim.env.TX_NEW_AGENT_RTP)",
                "-c",
                "lua require('tx-new-agent').open()",
            ])
            .env("TX_NEW_AGENT_SPEC", &spec)
            .env("TX_NEW_AGENT_RTP", repo_root(&self.env).join("nvim"))
            .status()?;
        let reply = read_reply(&result);
        fs::remove_dir_all(&directory)?;
        let Some(reply) = reply? else {
            return Ok(0);
        };
        let Reply {
            name,
            prompt,
            engine,
            model,
            effort,
            role,
            tags,
            cwd,
        } = reply;
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
            client,
            &name,
            &prompt,
            &engine,
            &model,
            &effort,
            &role,
            &tags,
            &cwd,
        ]);
        tmux.run_shell_background(&format!("{command} >/dev/null 2>&1 || true"))?;
        Ok(0)
    }

    /// The configured projects; without any, the firing pane's directory under its own name.
    fn projects(&self, pane: &str) -> Result<Projects, BoxError> {
        let home_dir = self.env.var("HOME").map(Path::new);
        let configured = Config::load(&self.home.config_path())?.projects(home_dir)?;
        if !configured.entries.is_empty() {
            return Ok(configured);
        }
        let path = self
            .service
            .tmux()
            .display_message("#{pane_current_path}", Some(pane))
            .unwrap_or_default();
        Ok(pane_project(&path))
    }

    /// The background half: spawn in the picked project and report.
    fn spawn(&self, client: &str, answers: &Answers) -> Result<i32, BoxError> {
        let tmux = self.service.tmux();
        let name = answers.name;
        let Some(spawn) = self.commands.upgrade().and_then(|table| table.get("spawn")) else {
            tmux.display_status(client, "prefix+n: the spawn verb is disabled");
            return Ok(1);
        };
        match spawn.run(&spawn_argv(answers)) {
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

/// The pane's directory as the one project, named after its last component.
fn pane_project(path: &str) -> Projects {
    let name = Path::new(path)
        .file_name()
        .map_or(path, |name| name.to_str().unwrap_or(path));
    Projects {
        entries: vec![(name.to_owned(), PathBuf::from(path))],
        default: None,
    }
}

/// The form's spec: the projects (names, their paths), the engines, each one's models (its
/// default first) and efforts, the roles (none first), and what is preselected; `result` is where
/// the answers go.
fn form_spec(result: &Path, roles: &[String], projects: &Projects) -> Value {
    let by_engine = |table: &[(&str, &[&str])]| {
        table
            .iter()
            .map(|(engine, options)| (engine.to_string(), json!(options)))
            .collect::<Map<_, _>>()
    };
    let role_options: Vec<&str> = std::iter::once(NO_ROLE)
        .chain(roles.iter().map(String::as_str))
        .collect();
    let role = if role_options.contains(&DEFAULT_ROLE) {
        DEFAULT_ROLE
    } else {
        NO_ROLE
    };
    let project_names: Vec<&str> = projects
        .entries
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    let project_paths: Map<String, Value> = projects
        .entries
        .iter()
        .map(|(name, path)| (name.clone(), json!(path)))
        .collect();
    json!({
        "result": result,
        "projects": project_names,
        "project_paths": project_paths,
        "project": projects.default_name(),
        "engines": ENGINES,
        "models": by_engine(MODELS),
        "efforts": by_engine(EFFORTS),
        "engine": ENGINES[0],
        "effort": DEFAULT_EFFORT.level().to_string(),
        "roles": role_options,
        "role": role,
        "tags_hint": TAGS_HELP,
    })
}

/// The form's answers, `None` when it was cancelled (it writes none).
fn read_reply(result: &Path) -> Result<Option<Reply>, BoxError> {
    if !result.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&fs::read_to_string(result)?)?))
}

/// `tx spawn` for an engine worker in the picked project.
fn spawn_argv(answers: &Answers) -> Vec<String> {
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
        answers.cwd,
        "--engine",
        answers.engine,
        "--effort",
        answers.effort,
        "--prompt",
        answers.prompt,
    ]
    .map(str::to_owned)
    .to_vec();
    if answers.model != default_model(answers.engine) {
        argv.extend(["--model".to_owned(), answers.model.to_owned()]);
    }
    if answers.role != NO_ROLE {
        argv.extend(["--role".to_owned(), answers.role.to_owned()]);
    }
    argv
}

/// The first model listed for `engine`: what it runs without `--model`.
fn default_model(engine: &str) -> &'static str {
    MODELS
        .iter()
        .find(|(name, _)| *name == engine)
        .map(|(_, models)| models[0])
        .expect("a listed engine")
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

    fn two_projects() -> Projects {
        Projects {
            entries: vec![
                ("tx".to_owned(), PathBuf::from("/u/code/tx")),
                ("web".to_owned(), PathBuf::from("/srv/web")),
            ],
            default: Some("web".to_owned()),
        }
    }

    #[test]
    fn the_spec_preselects_claude_its_default_model_effort_3_and_developer() {
        let projects = two_projects();
        let spec = form_spec(Path::new("/t/answers.json"), &["DEVELOPER".to_owned()], &projects);
        assert_eq!(spec["result"], "/t/answers.json");
        assert_eq!(spec["projects"], json!(["tx", "web"]));
        assert_eq!(
            spec["project_paths"],
            json!({"tx": "/u/code/tx", "web": "/srv/web"})
        );
        assert_eq!(spec["project"], "web");
        assert_eq!(spec["engines"], json!(["claude", "codex", "antigravity"]));
        assert_eq!(spec["engine"], "claude");
        assert_eq!(spec["models"]["claude"][0], "fable");
        assert_eq!(spec["models"]["antigravity"][0], AGY_MODEL_FAMILY);
        assert_eq!(spec["efforts"]["antigravity"], json!(["1", "2", "3"]));
        assert_eq!(spec["effort"], "3");
        assert_eq!(spec["roles"], json!(["none", "DEVELOPER"]));
        assert_eq!(spec["role"], "DEVELOPER");
        let without_developer = form_spec(
            Path::new("/t/answers.json"),
            &["REVIEWER".to_owned()],
            &projects,
        );
        assert_eq!(without_developer["role"], "none");
    }

    #[test]
    fn the_pane_directory_stands_in_for_missing_projects() {
        let projects = pane_project("/p/my proj");
        assert_eq!(
            projects.entries,
            vec![("my proj".to_owned(), PathBuf::from("/p/my proj"))]
        );
        assert_eq!(projects.default_name(), Some("my proj"));
        let spec = form_spec(Path::new("/t/answers.json"), &[], &projects);
        assert_eq!(spec["projects"], json!(["my proj"]));
        assert_eq!(spec["project"], "my proj");
        assert_eq!(pane_project("/").entries[0].0, "/");
    }

    #[test]
    fn spawns_an_engine_worker_in_the_picked_project() {
        let answers = Answers {
            name: "health",
            prompt: "add GET /health",
            engine: "codex",
            model: "gpt-6-sol",
            effort: "4",
            role: "DEVELOPER",
            tags: "tutor, api",
            cwd: "/p/my proj",
        };
        assert_eq!(
            spawn_argv(&answers),
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
                "gpt-6-sol",
                "--role",
                "DEVELOPER"
            ]
        );
        let defaults = Answers {
            model: "gpt-6-astra",
            role: NO_ROLE,
            tags: "",
            ..answers
        };
        let argv = spawn_argv(&defaults);
        assert!(!argv.contains(&"--model".to_owned()));
        assert!(!argv.contains(&"--role".to_owned()));
        assert_eq!(argv[2], DEFAULT_TAG);
    }
}
