//! `hook`: the engine / tmux hook entry (cli.py `HookCommand`), routed to `hooks.rs`.

use std::cell::RefCell;
use std::rc::Rc;

use cordis::{BoxError, Component, Ctx};

use crate::app::{COMMANDS, Command, ENGINES, ENV, EVENTS, Env, HOME, SERVICE, STORE, Visibility};
use crate::engines::EngineRegistry;
use crate::events::EventLog;
use crate::hooks::Hooks;
use crate::service::SessionService;
use crate::storage::Home;
use crate::store::SessionStore;

pub struct HookVerbs;

impl Component for HookVerbs {
    fn name(&self) -> &str {
        "verbs.hooks"
    }
    fn inject(&self) -> &[&'static str] {
        &[
            "commands", "service", "store", "events", "engines", "home", "env",
        ]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let command = Hook {
            service: ctx.get(SERVICE)?,
            store: ctx.get(STORE)?,
            events: ctx.get(EVENTS)?,
            engines: ctx.get(ENGINES)?,
            home: ctx.get(HOME)?,
            env: ctx.get(ENV)?,
        };
        ctx.get(COMMANDS)?
            .register(ctx, Visibility::Hidden, command);
        Ok(())
    }
}

struct Hook {
    service: Rc<SessionService>,
    store: Rc<SessionStore>,
    events: Rc<EventLog>,
    engines: Rc<RefCell<EngineRegistry>>,
    home: Rc<Home>,
    env: Rc<Env>,
}

impl Command for Hook {
    fn name(&self) -> &'static str {
        "hook"
    }
    fn summary(&self) -> &'static str {
        "Internal: Claude/tmux hook entry — drive session state (S2)."
    }
    fn run(&self, argv: &[String]) -> Result<i32, BoxError> {
        let engines = self.engines.borrow();
        let hooks = Hooks {
            service: &self.service,
            store: &self.store,
            events: &self.events,
            engines: &engines,
            home: &self.home,
            env: &self.env,
        };
        Ok(hooks.dispatch(argv, &mut std::io::stdin().lock())?)
    }
}
