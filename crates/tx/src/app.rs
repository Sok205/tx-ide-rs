//! Component wiring (docs/ARCHITECTURE.md): every invocation is a `cordis::Runtime` holding the
//! core services and the feature components that register verbs into the command table.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;

use cordis::{BoxError, Component, Ctx, Key, Runtime};

use crate::engines::EngineRegistry;
use crate::events::EventLog;
use crate::service::SessionService;
use crate::storage::Home;
use crate::store::{SessionStore, WarnOnce};
use crate::tmux::{Tmux, TmuxEnv};

/// The process environment, captured once at the edge; nothing below `main` reads `std::env`.
#[derive(Clone, Debug)]
pub struct Env {
    pub vars: BTreeMap<String, String>,
    pub cwd: PathBuf,
    /// This binary, for detached re-execs (`python -m tx …` in the reference).
    pub exe: PathBuf,
}

impl Env {
    pub fn capture() -> Self {
        Self {
            vars: std::env::vars().collect(),
            cwd: std::env::current_dir().unwrap_or_default(),
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("tx")),
        }
    }

    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars.get(name).map(String::as_str)
    }

    /// `$TX_SESSION_ID`, the actor of every log line (empty when unset).
    pub fn actor(&self) -> &str {
        self.var(crate::events::ACTOR_ENV).unwrap_or("")
    }
}

pub const ENV: Key<Env> = Key::new("env");
pub const HOME: Key<Home> = Key::new("home");
pub const EVENTS: Key<EventLog> = Key::new("events");
pub const STORE: Key<SessionStore> = Key::new("store");
pub const TMUX: Key<Tmux> = Key::new("tmux");
pub const COMMANDS: Key<CommandTable> = Key::new("commands");
/// The engine table (commutative: one removable row per adapter component).
pub const ENGINES: Key<RefCell<EngineRegistry>> = Key::new("engines");
pub const SERVICE: Key<SessionService> = Key::new("service");

/// A `tx <verb>`. `run` gets the verb's own argv; errors print as `tx <verb>: <error>`, exit 1.
pub trait Command {
    fn name(&self) -> &'static str;
    fn summary(&self) -> &'static str;
    fn run(&self, argv: &[String]) -> Result<i32, BoxError>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Hidden,
}

struct Row {
    id: u64,
    visibility: Visibility,
    command: Rc<dyn Command>,
}

/// The verb table: a commutative key (paper §3.4.2). Rows carry unique ids, so components
/// register in any order and removing one removes exactly its rows.
#[derive(Default)]
pub struct CommandTable {
    rows: RefCell<Vec<Row>>,
    next_id: Cell<u64>,
}

/// The order `tx --help` lists the reference's public verbs in; other verbs follow by name.
const HELP_ORDER: [&str; 25] = [
    "start",
    "attach",
    "ls",
    "spawn",
    "spawn-nvim",
    "spawn-view",
    "tag",
    "group",
    "rename",
    "whoami",
    "send-message",
    "send-user-message",
    "kill",
    "archive",
    "rm",
    "show",
    "history",
    "chat",
    "resume",
    "artifact",
    "sync",
    "fork",
    "handover",
    "rollover",
    "migrate",
];

impl CommandTable {
    /// Register `command` for the lifetime of the calling fiber (the undo removes the row).
    pub fn register(
        self: &Rc<Self>,
        ctx: &mut Ctx<'_>,
        visibility: Visibility,
        command: impl Command + 'static,
    ) {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.rows.borrow_mut().push(Row {
            id,
            visibility,
            command: Rc::new(command),
        });
        let table = Rc::clone(self);
        ctx.effect(move || table.rows.borrow_mut().retain(|row| row.id != id));
    }

    pub fn get(&self, name: &str) -> Option<Rc<dyn Command>> {
        self.rows
            .borrow()
            .iter()
            .find(|row| row.command.name() == name)
            .map(|row| Rc::clone(&row.command))
    }

    pub fn help(&self) -> String {
        let rows = self.rows.borrow();
        let mut public: Vec<_> = rows
            .iter()
            .filter(|row| row.visibility == Visibility::Public)
            .collect();
        public.sort_by_key(|row| {
            let name = row.command.name();
            (
                HELP_ORDER
                    .iter()
                    .position(|known| *known == name)
                    .unwrap_or(HELP_ORDER.len()),
                name,
            )
        });
        let mut out = String::from(
            "tx — tmux + Claude Code session controller\n\nusage: tx <command> [args]\n\ncommands:\n",
        );
        for row in public {
            out.push_str(&format!(
                "  {:<18} {}\n",
                row.command.name(),
                row.command.summary()
            ));
        }
        out
    }
}

/// A component that builds one service from its dependencies and provides it at `key`.
pub struct Service<T: 'static> {
    name: &'static str,
    inject: &'static [&'static str],
    provide: [&'static str; 1],
    key: Key<T>,
    build: fn(&Ctx<'_>) -> Result<T, BoxError>,
}

impl<T: 'static> Service<T> {
    pub const fn new(
        name: &'static str,
        inject: &'static [&'static str],
        key: Key<T>,
        build: fn(&Ctx<'_>) -> Result<T, BoxError>,
    ) -> Self {
        Self {
            name,
            inject,
            provide: [key.name],
            key,
            build,
        }
    }
}

impl<T: 'static> Component for Service<T> {
    fn name(&self) -> &str {
        self.name
    }
    fn inject(&self) -> &[&'static str] {
        self.inject
    }
    fn provide(&self) -> &[&'static str] {
        &self.provide
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let value = (self.build)(ctx)?;
        ctx.provide(self.key, value)?;
        Ok(())
    }
}

/// The environment snapshot as a component, so every other component reaches it by key.
struct EnvProvider(RefCell<Option<Env>>);

impl Component for EnvProvider {
    fn name(&self) -> &str {
        "env"
    }
    fn provide(&self) -> &[&'static str] {
        &[ENV.name]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let env = self.0.borrow_mut().take().ok_or("env already provided")?;
        ctx.provide(ENV, env)?;
        Ok(())
    }
}

/// The core services every feature builds on.
pub fn plug_core(runtime: &mut Runtime, env: Env) -> Result<(), cordis::Error> {
    runtime.plug(EnvProvider(RefCell::new(Some(env))))?;
    runtime.plug(Service::new("home", &["env"], HOME, |ctx| {
        let env = ctx.get(ENV)?;
        let home = Home::resolve(
            env.var("TX_IDE_HOME"),
            env.var("HOME").map(std::path::Path::new),
        );
        // Creating the skeleton is an emission past the system boundary (paper §6.1): not reverted.
        home.ensure()?;
        Ok(home)
    }))?;
    runtime.plug(Service::new("events", &["home"], EVENTS, |ctx| {
        Ok(EventLog::new(ctx.get(HOME)?.log_path()))
    }))?;
    runtime.plug(Service::new("store", &["home"], STORE, |ctx| {
        Ok(SessionStore::new(
            ctx.get(HOME)?.sessions_dir(),
            Rc::new(WarnOnce::stderr()),
        ))
    }))?;
    runtime.plug(Service::new("tmux", &["env"], TMUX, |ctx| {
        let env = ctx.get(ENV)?;
        Ok(Tmux::new(
            "tmux",
            TmuxEnv {
                tmux: env.var("TMUX").map(str::to_owned),
            },
        ))
    }))?;
    runtime.plug(Service::new("engines", &[], ENGINES, |_| {
        Ok(RefCell::new(EngineRegistry::new()))
    }))?;
    runtime.plug(Service::new(
        "service",
        &["store", "tmux", "events", "engines", "home", "env"],
        SERVICE,
        |ctx| {
            Ok(SessionService::new(
                ctx.get(STORE)?,
                ctx.get(TMUX)?,
                ctx.get(EVENTS)?,
                ctx.get(ENGINES)?,
                ctx.get(HOME)?,
                ctx.get(ENV)?,
            ))
        },
    ))?;
    runtime.plug(Service::new("commands", &[], COMMANDS, |_| {
        Ok(CommandTable::default())
    }))?;
    Ok(())
}

/// `main()` of the reference: help, unknown verb, dispatch, `tx <verb>: <error>` on failure.
pub fn run(args: Vec<OsString>) -> i32 {
    let argv: Vec<String> = args
        .into_iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut runtime = Runtime::new();
    if let Err(error) = plug_core(&mut runtime, Env::capture())
        .and_then(|()| crate::plugins::plug_features(&mut runtime))
    {
        eprintln!("tx: {error}");
        return 1;
    }
    for (component, error) in runtime.take_failures() {
        eprintln!("tx: {component}: {error}");
    }
    let code = dispatch(&runtime, &argv);
    runtime.shutdown();
    let _ = std::io::stdout().flush();
    code
}

fn dispatch(runtime: &Runtime, argv: &[String]) -> i32 {
    let Some(table) = runtime.get(COMMANDS) else {
        eprintln!("tx: the command table is not available");
        return 1;
    };
    let name = argv.first().map(String::as_str).unwrap_or("");
    if matches!(name, "" | "-h" | "--help" | "help") {
        print!("{}", table.help());
        return 0;
    }
    let Some(command) = table.get(name) else {
        eprintln!("tx: unknown command: {name}\n");
        print!("{}", table.help());
        return 2;
    };
    match command.run(&argv[1..]) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("tx {name}: {error}");
            1
        }
    }
}
