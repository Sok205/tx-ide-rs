//! tx-core plugin `spawn`: prefix+n. In the key's popup (bin/tmux-new-agent) an nvim form
//! (nvim/tx-new-agent.lua, embedded here and written to a temp dir at run time) asks for the
//! agent's name, tags, project, prompt, engine, model, effort and role; `_new-agent` then
//! creates the agent session and reports on the firing client's status line.
//!
//! The project is the firing pane's directory (the poc has no projects config). The roles are
//! the `*.md` files in the core's profile dirs minus COMMON, which every agent gets. The plugin
//! ships the COMMON and DEVELOPER roles and their skills as `agents/` resources, which
//! `txc ui install` materialises under `<data>/roles/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tx_client::tmux::Ui;
use tx_core::{
    CreateOptions, LaunchOptions, Plugin, PluginError, Registry, TxCore, Verb, VerbError, VerbIo,
};

pub const PLUGIN_ID: &str = "spawn";
/// The role every agent gets, never offered in the form.
pub const COMMON_ROLE: &str = "COMMON";
/// The role option that adds nothing to COMMON.
pub const NO_ROLE: &str = "none";
/// Preselected when the profile dirs have it.
pub const DEFAULT_ROLE: &str = "DEVELOPER";
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
        r.resource(
            "agents/COMMON.md",
            include_bytes!("../../../../agents/COMMON.md"),
        )?;
        r.resource(
            "agents/DEVELOPER.md",
            include_bytes!("../../../../agents/DEVELOPER.md"),
        )?;
        r.resource(
            "agents/skills/tx-sessions/SKILL.md",
            include_bytes!("../../../../agents/skills/tx-sessions/SKILL.md"),
        )?;
        r.resource(
            "agents/skills/tx-artifacts/SKILL.md",
            include_bytes!("../../../../agents/skills/tx-artifacts/SKILL.md"),
        )?;
        r.resource(
            "agents/skills/tx-diff-review/SKILL.md",
            include_bytes!("../../../../agents/skills/tx-diff-review/SKILL.md"),
        )?;
        r.resource(
            "agents/skills/tx-code-tours/SKILL.md",
            include_bytes!("../../../../agents/skills/tx-code-tours/SKILL.md"),
        )?;
        r.resource(
            "agents/skills/tx-code-tours/scripts/apply_tour.py",
            include_bytes!("../../../../agents/skills/tx-code-tours/scripts/apply_tour.py"),
        )
    }
}

/// The form's answers, as tx-new-agent.lua writes them; `cwd` is the picked project's path.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Reply {
    pub name: String,
    pub prompt: String,
    pub engine: String,
    pub model: String,
    pub effort: String,
    pub role: String,
    pub tags: String,
    pub cwd: String,
}

/// The role names offered: every `<NAME>.md` (not `.local.md`) in `dirs`, minus COMMON,
/// sorted and deduplicated.
pub fn role_names(dirs: &[PathBuf]) -> Vec<String> {
    let mut names: Vec<String> = dirs
        .iter()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flatten()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|file_name| !file_name.ends_with(tx_core::profile::LOCAL_SUFFIX))
        .filter_map(|file_name| {
            file_name
                .strip_suffix(tx_core::profile::ROLE_SUFFIX)
                .map(str::to_owned)
        })
        .filter(|name| name != COMMON_ROLE)
        .collect();
    names.sort();
    names.dedup();
    names
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
/// efforts, the roles (none first), and what is preselected; `result` is where the answers go.
pub fn form_spec(
    result: &Path,
    roles: &[String],
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
    let role_options: Vec<&str> = std::iter::once(NO_ROLE)
        .chain(roles.iter().map(String::as_str))
        .collect();
    let role = if role_options.contains(&DEFAULT_ROLE) {
        DEFAULT_ROLE
    } else {
        NO_ROLE
    };
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
        "roles": role_options,
        "role": role,
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

/// The session the answers describe: COMMON plus the picked role, the model only when it is
/// not the engine's default. The engine validates the options when the session is created.
pub fn create_options(reply: &Reply) -> CreateOptions {
    let mut roles = vec![COMMON_ROLE.to_owned()];
    if reply.role != NO_ROLE {
        roles.push(reply.role.clone());
    }
    let mut launch = LaunchOptions::agent(&reply.engine)
        .profile(roles)
        .option("effort", reply.effort.as_str())
        .initial_prompt(&reply.prompt);
    if reply.model != default_model(&reply.engine) {
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
        "_new-agent <pane> <client>   the new-agent form, then create the agent (prefix+n, internal)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [pane, client] = argv else {
            let _ = writeln!(io.stderr, "usage: txc _new-agent <pane> <client>");
            return Ok(2);
        };
        let ui = Ui::from_core(core);
        let path = ui
            .display_message("#{pane_current_path}", Some(pane))
            .or_else(|| {
                std::env::current_dir()
                    .ok()
                    .map(|cwd| cwd.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "/".to_owned());
        let (project_name, project_path) = pane_project(&path);
        let engines = core.engine_ids();
        if engines.is_empty() {
            ui.display_status(client, "prefix+n: no engine plugin is active");
            return Ok(1);
        }
        let directory = std::env::temp_dir().join(format!("tx-new-agent-{}", process::id()));
        fs::create_dir_all(&directory).map_err(io_failed(&directory))?;
        let spec = form_spec(
            &directory.join("answers.json"),
            &role_names(&core.config().profile_dirs),
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
            .create_session(create_options(&reply))
            .and_then(|id| core.get_session(&id));
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
            engine: "codex".into(),
            model: "gpt-6-sol".into(),
            effort: "xhigh".into(),
            role: "DEVELOPER".into(),
            tags: "tutor, api".into(),
            cwd: "/p/my proj".into(),
        }
    }

    #[test]
    fn the_spec_preselects_the_first_engine_its_default_model_high_and_developer() {
        let roles = ["DEVELOPER".to_owned(), "REVIEWER".to_owned()];
        let spec = form_spec(
            Path::new("/t/answers.json"),
            &roles,
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
        assert_eq!(spec["roles"], json!(["none", "DEVELOPER", "REVIEWER"]));
        assert_eq!(spec["role"], "DEVELOPER");
        let without_developer = form_spec(
            Path::new("/t/answers.json"),
            &["REVIEWER".to_owned()],
            ("web", "/srv/web"),
            &["claude"],
        );
        assert_eq!(without_developer["role"], "none");
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
    fn roles_are_the_md_files_minus_common_and_locals() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        for name in [
            "COMMON.md",
            "DEVELOPER.md",
            "DEVELOPER.local.md",
            "notes.txt",
        ] {
            fs::write(dir.path().join(name), "x").unwrap();
        }
        for name in ["REVIEWER.md", "DEVELOPER.md"] {
            fs::write(other.path().join(name), "x").unwrap();
        }
        let dirs = [
            dir.path().to_path_buf(),
            other.path().to_path_buf(),
            PathBuf::from("/nonexistent"),
        ];
        assert_eq!(role_names(&dirs), ["DEVELOPER", "REVIEWER"]);
        assert!(role_names(&[]).is_empty());
    }

    #[test]
    fn tags_are_split_and_default_to_agent() {
        assert_eq!(tags_of("tutor, api,,"), ["tutor", "api"]);
        assert_eq!(tags_of(""), [DEFAULT_TAG]);
    }

    #[test]
    fn the_answers_become_an_agent_session_under_common_and_the_role() {
        let options = create_options(&reply());
        assert_eq!(options.name, "health");
        assert_eq!(options.cwd, PathBuf::from("/p/my proj"));
        assert_eq!(options.tags, ["tutor", "api"]);
        let LaunchOptions::Agent {
            engine,
            profile,
            options,
            initial_prompt,
            ..
        } = options.launch
        else {
            panic!("an agent launch");
        };
        assert_eq!(engine, "codex");
        assert_eq!(profile, ["COMMON", "DEVELOPER"]);
        assert_eq!(options, json!({"effort": "xhigh", "model": "gpt-6-sol"}));
        assert_eq!(initial_prompt.as_deref(), Some("add GET /health"));
    }

    #[test]
    fn the_default_model_and_no_role_pass_nothing_extra() {
        let defaults = Reply {
            model: "gpt-6-astra".into(),
            role: NO_ROLE.into(),
            tags: String::new(),
            ..reply()
        };
        let options = create_options(&defaults);
        assert_eq!(options.tags, [DEFAULT_TAG]);
        let LaunchOptions::Agent {
            profile, options, ..
        } = options.launch
        else {
            panic!("an agent launch");
        };
        assert_eq!(profile, ["COMMON"]);
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
