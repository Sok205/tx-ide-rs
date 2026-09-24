//! The feature manifest and its loader (paper §5.2): every component the reference ships, as a
//! named entry the user can switch off in `config.json`:
//!
//! ```json
//! { "plugins": { "engine.codex": { "disabled": true }, "verbs.chat": { "disabled": true } } }
//! ```
//!
//! The core services (`app::plug_core`) are not entries: every verb needs them. An entry's other
//! keys are its configuration; a change to them rebuilds just that component. A malformed
//! `config.json` loads the defaults: only the readers the reference has (reconcile, sync) may fail
//! on it, so a broken config never breaks a verb the reference would run.

use std::cell::RefCell;
use std::fmt::Write as _;
use std::path::Path;
use std::rc::Rc;

use cordis::loader::{Entry, Loader, Status};
use cordis::{BoxError, Component, Ctx, Runtime};
use serde_json::{Map, Value};

use crate::app::{COMMANDS, Command, ENGINES, ENV, Env, HOME, Visibility};
use crate::engines::{AntigravityEngine, ClaudeEngine, CodexEngine, CodexHostEnv, EngineAdapter};
use crate::session::Engine;
use crate::storage::{Config, Home};
use crate::verbs;

type Factory = fn() -> Rc<dyn Component>;

/// Every feature component, in load order.
const MANIFEST: [(&str, Factory); 10] = [
    ("engine.claude", || {
        engine("engine.claude", Engine::Claude, claude)
    }),
    ("engine.codex", || {
        engine("engine.codex", Engine::Codex, codex)
    }),
    ("engine.antigravity", || {
        engine("engine.antigravity", Engine::Antigravity, antigravity)
    }),
    ("verbs.home", || Rc::new(verbs::home::HomeVerbs)),
    ("verbs.sessions", || Rc::new(verbs::sessions::SessionVerbs)),
    ("verbs.listing", || Rc::new(verbs::listing::ListingVerbs)),
    ("verbs.hooks", || Rc::new(verbs::hooks::HookVerbs)),
    ("verbs.chat", || Rc::new(verbs::chat::ChatVerbs)),
    ("verbs.artifacts", || {
        Rc::new(verbs::artifacts::ArtifactVerbs)
    }),
    ("verbs.install", || Rc::new(verbs::install::InstallVerbs)),
];

/// What `tx _plugins` prints: each entry and its status after loading.
type Report = Rc<RefCell<Vec<(String, Option<Status>)>>>;

/// Plug the feature components the config enables. Warnings (a bad `plugins` section, an unknown
/// entry) go to stderr; they never stop the verb.
pub fn plug_features(runtime: &mut Runtime) -> Result<(), cordis::Error> {
    let report = Report::default();
    runtime.plug(PluginsVerb(Rc::clone(&report)))?;
    let (entries, warnings) = match runtime.get(HOME) {
        Some(home) => entries(&home),
        None => (defaults(), Vec::new()),
    };
    for warning in warnings {
        eprintln!("tx: config.json: {warning}");
    }
    let mut loader = Loader::new();
    let errors = loader.reconcile(runtime, &entries, |id, _config| {
        MANIFEST
            .iter()
            .find(|(known, _)| *known == id)
            .map(|(_, factory)| factory())
    });
    for error in errors {
        eprintln!("tx: config.json plugins: {error}");
    }
    *report.borrow_mut() = entries
        .iter()
        .map(|entry| (entry.id.clone(), loader.status(runtime, &entry.id)))
        .collect();
    Ok(())
}

fn defaults() -> Vec<Entry<Value>> {
    MANIFEST
        .iter()
        .map(|(id, _)| Entry::new(*id, Value::Null))
        .collect()
}

/// The manifest with the user's `plugins` section applied, plus warnings about that section.
/// Unknown ids are kept as entries so the loader reports them.
fn entries(home: &Home) -> (Vec<Entry<Value>>, Vec<String>) {
    let mut warnings = Vec::new();
    let settings = match Config::load(&home.config_path()) {
        Ok(config) => config.raw().get("plugins").cloned(),
        Err(_) => None,
    };
    let settings = match settings {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        Some(_) => {
            warnings.push("'plugins' must be an object; using the defaults".to_owned());
            Map::new()
        }
    };
    let mut entries = defaults();
    for (id, value) in settings {
        let Value::Object(mut options) = value else {
            warnings.push(format!("plugins.{id} must be an object; ignoring it"));
            continue;
        };
        let disabled = match options.remove("disabled") {
            None => false,
            Some(Value::Bool(disabled)) => disabled,
            Some(_) => {
                warnings.push(format!(
                    "plugins.{id}.disabled must be true or false; ignoring it"
                ));
                false
            }
        };
        let entry = Entry::new(id.as_str(), Value::Object(options)).disabled(disabled);
        match entries.iter_mut().find(|known| known.id == id) {
            Some(known) => *known = entry,
            None => entries.push(entry),
        }
    }
    (entries, warnings)
}

/// `tx _plugins`: the feature components and their status.
struct PluginsVerb(Report);

impl Component for PluginsVerb {
    fn name(&self) -> &str {
        "verbs.plugins"
    }
    fn inject(&self) -> &[&'static str] {
        &["commands"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        ctx.get(COMMANDS)?
            .register(ctx, Visibility::Hidden, ListPlugins(Rc::clone(&self.0)));
        Ok(())
    }
}

struct ListPlugins(Report);

impl Command for ListPlugins {
    fn name(&self) -> &'static str {
        "_plugins"
    }
    fn summary(&self) -> &'static str {
        "List the feature components and whether each is active (config.json `plugins`)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        if !argv.is_empty() {
            return Err("takes no arguments".into());
        }
        let mut out = String::new();
        for (id, status) in self.0.borrow().iter() {
            let status = match status {
                Some(Status::Active) => "active",
                Some(Status::Waiting) => "waiting (a dependency is disabled or missing)",
                Some(Status::Disabled) => "disabled",
                Some(Status::Failed) => "failed",
                None => "unknown",
            };
            writeln!(out, "{id:<20} {status}")?;
        }
        print!("{out}");
        Ok(0)
    }
}

fn engine(
    name: &'static str,
    engine: Engine,
    build: fn(&Home, &Env) -> Rc<dyn EngineAdapter>,
) -> Rc<dyn Component> {
    Rc::new(EngineComponent {
        name,
        engine,
        build,
    })
}

fn home_dir(env: &Env) -> Option<&Path> {
    env.var("HOME").map(Path::new)
}

fn claude(home: &Home, env: &Env) -> Rc<dyn EngineAdapter> {
    Rc::new(ClaudeEngine::new(
        home.clone(),
        env.var("CLAUDE_CONFIG_DIR"),
        home_dir(env),
    ))
}

fn codex(home: &Home, env: &Env) -> Rc<dyn EngineAdapter> {
    let host = CodexHostEnv {
        codex_home: env.var("CODEX_HOME").map(str::to_owned),
        home_dir: home_dir(env).map(Path::to_path_buf),
        path: env.var("PATH").map(str::to_owned),
    };
    Rc::new(CodexEngine::new(home.clone(), host, env.exe.clone()))
}

fn antigravity(home: &Home, env: &Env) -> Rc<dyn EngineAdapter> {
    Rc::new(AntigravityEngine::new(home.clone(), home_dir(env)))
}

/// One engine adapter as a component: a row in the engine table for as long as it is loaded.
struct EngineComponent {
    name: &'static str,
    engine: Engine,
    build: fn(&Home, &Env) -> Rc<dyn EngineAdapter>,
}

impl Component for EngineComponent {
    fn name(&self) -> &str {
        self.name
    }
    fn inject(&self) -> &[&'static str] {
        &["engines", "home", "env"]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let (home, env) = (ctx.get(HOME)?, ctx.get(ENV)?);
        let adapter = (self.build)(&home, &env);
        let engines = ctx.get(ENGINES)?;
        let id = engines.borrow_mut().register(self.engine, adapter);
        ctx.effect(move || {
            engines.borrow_mut().unregister(id);
        });
        Ok(())
    }
}
