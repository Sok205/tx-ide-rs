//! tx-core plugin `spawn`: prefix+n. In the key's popup (bin/tmux-new-agent) an nvim form
//! (nvim/tx-new-agent.lua, embedded here and written to a temp dir at run time) asks for the
//! agent's name, tags, project, prompt, engine, model, effort and agent definition;
//! `_new-agent` then creates the agent session and reports on the firing client's status line.
//!
//! The project is the firing pane's directory (the poc has no projects config). The agents are
//! the agent definitions (`tx definitions agents`), the configured default (`agent.definition`)
//! preselected; each brings its own prompt, tools, skills and roles.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tx_client::tmux::Ui;
use tx_core::definitions::{DefinitionError, Definitions};
use tx_core::{
    CreateOptions, DefinitionChoice, EngineKind, LaunchOptions, Plugin, PluginError, Registry,
    TxCore, Verb, VerbError, VerbIo,
};

pub const PLUGIN_ID: &str = "spawn";
pub const DEFAULT_TAG: &str = "agent";
/// Preselected effort; every engine listed in [`EFFORTS`] accepts it.
pub const DEFAULT_EFFORT: &str = "high";
const TAGS_HELP: &str = "Comma-separated (default: agent)";
/// Each engine's models, the one it runs without `--model` first (preselected, passed as no
/// `--model`). An engine missing here offers `default` alone.
const MODELS: &[(&str, &[&str])] = &[
    ("claude", &["fable", "opus", "sonnet", "haiku"]),
    (
        "codex",
        &["gpt-6-astra", "gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna"],
    ),
];
const FALLBACK_MODELS: &[&str] = &["default"];
/// Each engine's effort tiers, as its own `Effort` names them (the engine validates the pick
/// at create). An engine missing here offers the Claude list.
const EFFORTS: &[(&str, &[&str])] = &[
    ("claude", &["low", "medium", "high", "xhigh", "max"]),
    (
        "codex",
        &["none", "minimal", "low", "medium", "high", "xhigh", "max"],
    ),
];
const FALLBACK_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];
const FORM_MODULE: &str = "tx-new-agent";
const FORM: &[u8] = include_bytes!("../nvim/tx-new-agent.lua");
const SPEC_ENV: &str = "TX_NEW_AGENT_SPEC";
const RTP_ENV: &str = "TX_NEW_AGENT_RTP";
/// Where the form writes its answers; also in the spec, exported for shims that stand in for
/// nvim.
const RESULT_ENV: &str = "TX_NEW_AGENT_RESULT";

#[derive(Clone, Copy, Debug, Default)]
pub struct SpawnPlugin;

impl Plugin for SpawnPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(NewAgent));
        r.resource("keys/spawn.tmux", include_bytes!("../tmux/keys.tmux"))?;
        r.resource(
            "bin/tmux-new-agent",
            include_bytes!("../bin/tmux-new-agent"),
        )?;
        Ok(())
    }
}

/// The form's answers, as tx-new-agent.lua writes them; `cwd` is the picked project's path.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Reply {
    pub name: String,
    pub prompt: String,
    pub engine: EngineKind,
    pub model: String,
    pub effort: String,
    pub agent: String,
    pub tags: String,
    pub cwd: String,
}

/// The agent definitions offered: every winning `agents/<name>.md` under the definition roots,
/// sorted.
pub fn agent_names(
    roots: impl IntoIterator<Item = PathBuf>,
) -> Result<Vec<String>, DefinitionError> {
    Ok(Definitions::new(roots).agent_files()?.into_keys().collect())
}

/// The pane's directory as the one project, named after its last component.
pub fn pane_project(path: &str) -> (String, String) {
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path);
    (name.to_owned(), path.to_owned())
}

/// The first model listed for `engine`: what it runs without `--model`.
pub fn default_model(engine: &str) -> &'static str {
    models_for(engine)[0]
}

fn models_for(engine: &str) -> &'static [&'static str] {
    MODELS
        .iter()
        .find(|(name, _)| *name == engine)
        .map_or(FALLBACK_MODELS, |(_, models)| models)
}

fn efforts_for(engine: &str) -> &'static [&'static str] {
    EFFORTS
        .iter()
        .find(|(name, _)| *name == engine)
        .map_or(FALLBACK_EFFORTS, |(_, efforts)| efforts)
}

/// The form's spec: the one project, the engines, each one's models (its default first) and
/// efforts, the agents, and what is preselected (the agent: `default_agent` when offered, else
/// the first); `result` is where the answers go.
pub fn form_spec(
    result: &Path,
    agents: &[String],
    default_agent: Option<&str>,
    project: (&str, &str),
    engines: &[&str],
) -> Value {
    let models: Map<String, Value> = engines
        .iter()
        .map(|engine| ((*engine).to_owned(), json!(models_for(engine))))
        .collect();
    let efforts: Map<String, Value> = engines
        .iter()
        .map(|engine| ((*engine).to_owned(), json!(efforts_for(engine))))
        .collect();
    let agent = default_agent
        .filter(|agent| agents.iter().any(|offered| offered == agent))
        .or(agents.first().map(String::as_str))
        .unwrap_or_default();
    let (project_name, project_path) = project;
    json!({
        "result": result,
        "projects": [project_name],
        "project_paths": { project_name: project_path },
        "project": project_name,
        "engines": engines,
        "models": models,
        "efforts": efforts,
        "engine": engines[0],
        "effort": DEFAULT_EFFORT,
        "agents": agents,
        "agent": agent,
        "tags_hint": TAGS_HELP,
    })
}

/// `tutor, api` → `[tutor, api]`; empty → `[agent]`.
pub fn tags_of(tags: &str) -> Vec<String> {
    let tags: Vec<String> = tags
        .split(',')
        .map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map(str::to_owned)
        .collect();
    if tags.is_empty() {
        vec![DEFAULT_TAG.to_owned()]
    } else {
        tags
    }
}

/// The session the answers describe: the picked agent definition (which brings its roles), the
/// model only when it is not the engine's default. The engine validates the options when the
/// session is created.
pub fn create_options(reply: &Reply) -> CreateOptions {
    let mut launch = LaunchOptions::agent(reply.engine)
        .definition(DefinitionChoice::new(&reply.agent))
        .option("effort", reply.effort.as_str())
        .initial_prompt(&reply.prompt);
    if reply.model != default_model(reply.engine.as_str()) {
        launch = launch.option("model", reply.model.as_str());
    }
    CreateOptions::new(&reply.name, &reply.cwd, launch).tags(tags_of(&reply.tags))
}

fn io_failed(path: &Path) -> impl FnOnce(std::io::Error) -> VerbError {
    let path = path.to_path_buf();
    move |error| VerbError::Failed(format!("{}: {error}", path.display()))
}

/// Runs the form in `directory` (the spec, the lua module and the answers all live there);
/// `None` when it was cancelled.
fn run_form(directory: &Path, spec: &Value) -> Result<Option<Reply>, VerbError> {
    let lua_dir = directory.join("lua");
    fs::create_dir_all(&lua_dir).map_err(io_failed(&lua_dir))?;
    let module = lua_dir.join(format!("{FORM_MODULE}.lua"));
    fs::write(&module, FORM).map_err(io_failed(&module))?;
    let spec_path = directory.join("spec.json");
    fs::write(&spec_path, spec.to_string()).map_err(io_failed(&spec_path))?;
    let result = directory.join("answers.json");
    // --clean: the form brings its own look, and no user config runs before it.
    process::Command::new("nvim")
        .args([
            "--clean",
            "-n",
            "-i",
            "NONE",
            "--cmd",
            &format!("set shortmess+=I | lua vim.opt.rtp:prepend(vim.env.{RTP_ENV})"),
            "-c",
            &format!("lua require('{FORM_MODULE}').open()"),
        ])
        .env(SPEC_ENV, &spec_path)
        .env(RTP_ENV, directory)
        .env(RESULT_ENV, &result)
        .status()
        .map_err(|error| VerbError::Failed(format!("could not run nvim: {error}")))?;
    if !result.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&result).map_err(io_failed(&result))?;
    let reply: Reply = serde_json::from_str(&text)
        .map_err(|error| VerbError::Failed(format!("{}: {error}", result.display())))?;
    Ok(Some(reply))
}

pub struct NewAgent;

impl Verb for NewAgent {
    fn name(&self) -> &'static str {
        "_new-agent"
    }

    fn usage(&self) -> &'static str {
        "_new-agent <pane> <client> [--cwd DIR]   the new-agent form, then create the agent (prefix+n, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let (pane, client, directory) = match argv {
            [pane, client] => (pane, client, None),
            [pane, client, flag, directory] if flag == "--cwd" => {
                (pane, client, Some(directory.clone()))
            }
            _ => {
                let _ = writeln!(
                    io.stderr,
                    "usage: tx _new-agent <pane> <client> [--cwd DIR]"
                );
                return Ok(2);
            }
        };
        let ui = Ui::from_core(core);
        let path = directory
            .or_else(|| ui.display_message("#{pane_current_path}", Some(pane)))
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|cwd| cwd.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "/".to_owned());
        let (project_name, project_path) = pane_project(&path);
        let kinds = core.engine_kinds();
        let engines: Vec<&str> = kinds.iter().map(|kind| kind.as_str()).collect();
        if engines.is_empty() {
            ui.display_status(client, "prefix+n: no engine plugin is active");
            return Ok(1);
        }
        let agents = match agent_names(core.config().directories.definition_roots()) {
            Ok(agents) if agents.is_empty() => {
                ui.display_status(
                    client,
                    "prefix+n: no agent definitions (run `tx ui install`)",
                );
                return Ok(1);
            }
            Ok(agents) => agents,
            Err(error) => {
                ui.display_status(client, &format!("prefix+n: {error}"));
                return Ok(1);
            }
        };
        let directory = std::env::temp_dir().join(format!("tx-new-agent-{}", process::id()));
        fs::create_dir_all(&directory).map_err(io_failed(&directory))?;
        let spec = form_spec(
            &directory.join("answers.json"),
            &agents,
            core.config().default_definition.as_deref(),
            (&project_name, &project_path),
            &engines,
        );
        let reply = run_form(&directory, &spec);
        let _ = fs::remove_dir_all(&directory);
        let Some(reply) = reply? else {
            return Ok(0);
        };
        // The agent may get `<name>-2` when the name is taken: say what it is really called.
        let created = core
            .create_agent(create_options(&reply))
            .and_then(|id| core.get_agent(&id));
        match created {
            Ok(session) => {
                ui.display_status(client, &format!("spawned {}", session.name));
                Ok(0)
            }
            Err(error) => {
                ui.display_status(client, &format!("spawn of {} failed: {error}", reply.name));
                Ok(1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply() -> Reply {
        Reply {
            name: "health".into(),
            prompt: "add GET /health".into(),
            engine: EngineKind::Codex,
            model: "gpt-6-sol".into(),
            effort: "xhigh".into(),
            agent: "qa".into(),
            tags: "tutor, api".into(),
            cwd: "/p/my proj".into(),
        }
    }

    #[test]
    fn the_spec_preselects_the_first_engine_its_default_model_high_and_the_default_agent() {
        let agents = ["claude".to_owned(), "developer".to_owned(), "qa".to_owned()];
        let spec = form_spec(
            Path::new("/t/answers.json"),
            &agents,
            Some("developer"),
            ("web", "/srv/web"),
            &["claude", "codex", "aider"],
        );
        assert_eq!(spec["result"], "/t/answers.json");
        assert_eq!(spec["projects"], json!(["web"]));
        assert_eq!(spec["project_paths"], json!({"web": "/srv/web"}));
        assert_eq!(spec["project"], "web");
        assert_eq!(spec["engines"], json!(["claude", "codex", "aider"]));
        assert_eq!(spec["engine"], "claude");
        assert_eq!(spec["models"]["claude"][0], "fable");
        assert_eq!(spec["models"]["aider"], json!(["default"]));
        assert_eq!(
            spec["efforts"]["claude"],
            json!(["low", "medium", "high", "xhigh", "max"])
        );
        assert_eq!(
            spec["efforts"]["codex"],
            json!(["none", "minimal", "low", "medium", "high", "xhigh", "max"])
        );
        assert_eq!(spec["efforts"]["aider"], spec["efforts"]["claude"]);
        assert_eq!(spec["effort"], "high");
        assert_eq!(spec["agents"], json!(["claude", "developer", "qa"]));
        assert_eq!(spec["agent"], "developer");
        // A default that is not offered (or none at all) preselects the first agent.
        for default in [Some("ghost"), None] {
            let spec = form_spec(
                Path::new("/t/answers.json"),
                &agents,
                default,
                ("web", "/srv/web"),
                &["claude"],
            );
            assert_eq!(spec["agent"], "claude", "{default:?}");
        }
    }

    #[test]
    fn the_pane_directory_is_the_project() {
        assert_eq!(
            pane_project("/p/my proj"),
            ("my proj".to_owned(), "/p/my proj".to_owned())
        );
        assert_eq!(pane_project("/").0, "/");
    }

    #[test]
    fn the_agents_are_the_definitions_of_every_root() {
        let shipped = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        for (root, name) in [
            (&shipped, "developer"),
            (&shipped, "qa"),
            (&user, "developer"),
            (&user, "mine"),
        ] {
            let agents = root.path().join("agents");
            fs::create_dir_all(&agents).unwrap();
            fs::write(agents.join(format!("{name}.md")), "x").unwrap();
        }
        fs::write(shipped.path().join("agents/notes.txt"), "x").unwrap();
        let roots = [
            shipped.path().to_path_buf(),
            user.path().to_path_buf(),
            PathBuf::from("/nonexistent"),
        ];
        assert_eq!(agent_names(roots).unwrap(), ["developer", "mine", "qa"]);
        assert!(agent_names([]).unwrap().is_empty());
    }

    #[test]
    fn tags_are_split_and_default_to_agent() {
        assert_eq!(tags_of("tutor, api,,"), ["tutor", "api"]);
        assert_eq!(tags_of(""), [DEFAULT_TAG]);
    }

    #[test]
    fn the_answers_become_an_agent_session_of_the_picked_definition() {
        let options = create_options(&reply());
        assert_eq!(options.name, "health");
        assert_eq!(options.cwd, PathBuf::from("/p/my proj"));
        assert_eq!(options.tags, ["tutor", "api"]);
        let LaunchOptions::Agent {
            engine,
            profile,
            options,
            initial_prompt,
            definition,
        } = options.launch
        else {
            panic!("an agent launch");
        };
        assert_eq!(engine, EngineKind::Codex);
        // The definition brings its roles: the launch names none.
        assert!(profile.is_empty());
        assert_eq!(definition, Some(DefinitionChoice::new("qa")));
        assert_eq!(options, json!({"effort": "xhigh", "model": "gpt-6-sol"}));
        assert_eq!(initial_prompt, "add GET /health");
    }

    #[test]
    fn the_default_model_passes_nothing_extra() {
        let defaults = Reply {
            model: "gpt-6-astra".into(),
            tags: String::new(),
            ..reply()
        };
        let options = create_options(&defaults);
        assert_eq!(options.tags, [DEFAULT_TAG]);
        let LaunchOptions::Agent { options, .. } = options.launch else {
            panic!("an agent launch");
        };
        assert_eq!(options, json!({"effort": "xhigh"}));
    }

    /// The engine, not the form, decides what an effort may be: a bad one is passed through
    /// and refused at create.
    #[test]
    fn a_bad_effort_is_passed_to_the_engine() {
        let bad = Reply {
            effort: "7".into(),
            ..reply()
        };
        let LaunchOptions::Agent { options, .. } = create_options(&bad).launch else {
            panic!("an agent launch");
        };
        assert_eq!(options["effort"], "7");
    }
}
