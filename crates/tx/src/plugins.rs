//! The feature manifest: every component the reference ships, plugged after the core.

use std::path::Path;
use std::rc::Rc;

use cordis::{BoxError, Component, Ctx, Runtime};

use crate::app::{ENGINES, ENV, Env, HOME};
use crate::engines::{AntigravityEngine, ClaudeEngine, CodexEngine, CodexHostEnv, EngineAdapter};
use crate::session::Engine;
use crate::storage::Home;
use crate::verbs;

pub fn plug_features(runtime: &mut Runtime) -> Result<(), cordis::Error> {
    runtime.plug(EngineComponent {
        name: "engine.claude",
        engine: Engine::Claude,
        build: claude,
    })?;
    runtime.plug(EngineComponent {
        name: "engine.codex",
        engine: Engine::Codex,
        build: codex,
    })?;
    runtime.plug(EngineComponent {
        name: "engine.antigravity",
        engine: Engine::Antigravity,
        build: antigravity,
    })?;
    runtime.plug(verbs::home::HomeVerbs)?;
    runtime.plug(verbs::sessions::SessionVerbs)?;
    runtime.plug(verbs::listing::ListingVerbs)?;
    runtime.plug(verbs::hooks::HookVerbs)?;
    runtime.plug(verbs::chat::ChatVerbs)?;
    runtime.plug(verbs::artifacts::ArtifactVerbs)?;
    Ok(())
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
