//! `TxCore`: the public operations. One struct, built once by `open`, owning its collaborators.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::config::CoreConfig;
use crate::engine::{EngineRegistry, LaunchMode, LaunchRequest, LlmEngine, apply_workspace};
use crate::error::CoreError;
use crate::events::{Event, EventLog, EventSink, EventType, now};
use crate::locks::Locks;
use crate::messaging::{DELIVERY_PAUSE, build_envelope, deliver};
use crate::profile::{Profile, Profiles, ResolvedProfile};
use crate::session::{Launch, Lifecycle, Session, SessionId, validate_name};
use crate::state::{LlmEngineHook, LlmEngineState};
use crate::store::{SessionStore, StoreError};
use crate::tmux::{Live, Tmux, TmuxError, exact};
use crate::worktree::WorktreeManager;

pub const SESSIONS_DIR: &str = "sessions";
pub const LOCKS_DIR: &str = "locks";
pub const PROFILES_DIR: &str = "profiles";
pub const LOG_FILE: &str = "log.jsonl";
pub const TMUX_BINARY: &str = "tmux";
pub const SESSION_ID_ENV: &str = "TX_SESSION_ID";
pub const TMUX_SOCKET_ENV: &str = "TX_TMUX_SOCKET";

/// Written on `open` when the data dir has no `tmux.conf`. The IDE may replace it.
pub const DEFAULT_TMUX_CONF: &str = "\
# tx-core server config. Edit freely; the IDE may replace this file.
# The server exits with its last session; records of its sessions become `exited`.
set -s exit-empty on
set -g history-limit 50000
";

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CreateOptions {
    /// The name asked for. An agent may get `<name>-2` … `<name>-99` instead when the name is
    /// taken by a running record or a worktree on disk; read it back from the record.
    pub name: String,
    /// The starting directory. For an agent: a git checkout to branch from; the agent runs in
    /// its own detached worktree at that checkout's `HEAD`, and the record's `cwd` names the
    /// worktree. For a process: where it runs.
    pub cwd: PathBuf,
    pub parent: Option<SessionId>,
    pub tags: Vec<String>,
    pub env: Vec<(String, String)>,
    pub launch: LaunchOptions,
}

impl CreateOptions {
    pub fn new(name: impl Into<String>, cwd: impl Into<PathBuf>, launch: LaunchOptions) -> Self {
        Self {
            name: name.into(),
            cwd: cwd.into(),
            parent: None,
            tags: Vec::new(),
            env: Vec::new(),
            launch,
        }
    }

    pub fn parent(mut self, parent: SessionId) -> Self {
        self.parent = Some(parent);
        self
    }

    pub fn tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags.extend(tags.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// How `fork_session` names and starts the new agent. Everything else (engine, roles, model,
/// effort, starting checkout) comes from the source record.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ForkOptions {
    /// The name asked for; `None` is `<source name>-fork`. Made unique the way `create` does.
    pub name: Option<String>,
    /// The fork's first message. Required: an engine never starts a fork silently.
    pub prompt: String,
    /// `None`: the source's tags.
    pub tags: Option<Vec<String>>,
    /// Engine options the fork changes, a JSON object as in `LaunchOptions::Agent`; the rest of
    /// the engine's data starts as a copy of the source's (its model and effort included), so
    /// `{}` is "like the source".
    pub options: serde_json::Value,
}

impl ForkOptions {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            name: None,
            prompt: prompt.into(),
            tags: None,
            options: serde_json::Value::Object(serde_json::Map::new()),
        }
    }

    /// Replaces the engine options with `options`, a JSON object.
    pub fn options(mut self, options: serde_json::Value) -> Self {
        self.options = options;
        self
    }

    /// Sets one engine option (`option("model", "fable")`); a repeated key replaces.
    pub fn option(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        if let serde_json::Value::Object(options) = &mut self.options {
            options.insert(key.into(), value.into());
        }
        self
    }

    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = Some(tags.into_iter().map(Into::into).collect());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LaunchOptions {
    Agent {
        engine: String,
        profile: Vec<String>,
        /// Engine options as a JSON object (`{"model": "fable", "effort": "high"}`): the engine
        /// validates them (`LlmEngine::configure`) and they land in `ext[ext_key]` before the
        /// launch. Core never interprets a key.
        options: serde_json::Value,
        initial_prompt: Option<String>,
        /// A conversation id the engine continues: stored as `{"conversation_id": id}` under its
        /// `ext` key, then launched with `LaunchMode::Resume`.
        resume: Option<String>,
    },
    Process {
        program: String,
        args: Vec<String>,
    },
}

impl LaunchOptions {
    pub fn agent(engine: impl Into<String>) -> Self {
        Self::Agent {
            engine: engine.into(),
            profile: Vec::new(),
            options: serde_json::Value::Object(serde_json::Map::new()),
            initial_prompt: None,
            resume: None,
        }
    }

    pub fn process<I, S>(program: impl Into<String>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Process {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }

    /// Role names, in order. No effect on a `Process` launch.
    pub fn profile<I, S>(mut self, roles: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        if let Self::Agent { profile, .. } = &mut self {
            profile.extend(roles.into_iter().map(Into::into));
        }
        self
    }

    /// Replaces the engine options with `options`, a JSON object. No effect on a `Process`
    /// launch.
    pub fn options(mut self, options: serde_json::Value) -> Self {
        if let Self::Agent { options: slot, .. } = &mut self {
            *slot = options;
        }
        self
    }

    /// Sets one engine option (`option("model", "fable")`); a repeated key replaces. No effect
    /// on a `Process` launch.
    pub fn option(mut self, key: impl Into<String>, value: impl Into<serde_json::Value>) -> Self {
        if let Self::Agent {
            options: serde_json::Value::Object(options),
            ..
        } = &mut self
        {
            options.insert(key.into(), value.into());
        }
        self
    }

    /// No effect on a `Process` launch.
    pub fn initial_prompt(mut self, prompt: impl Into<String>) -> Self {
        if let Self::Agent { initial_prompt, .. } = &mut self {
            *initial_prompt = Some(prompt.into());
        }
        self
    }

    /// No effect on a `Process` launch.
    pub fn resume(mut self, resume: impl Into<String>) -> Self {
        if let Self::Agent { resume: slot, .. } = &mut self {
            *slot = Some(resume.into());
        }
        self
    }
}

/// Every field is a conjunct; `None` matches everything.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListFilter {
    pub parent: Option<SessionId>,
    pub tag: Option<String>,
    pub lifecycle: Option<Lifecycle>,
}

impl ListFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parent(mut self, parent: SessionId) -> Self {
        self.parent = Some(parent);
        self
    }

    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }

    pub fn lifecycle(mut self, lifecycle: Lifecycle) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }

    pub fn matches(&self, session: &Session) -> bool {
        self.parent
            .as_ref()
            .is_none_or(|parent| session.parent.as_ref() == Some(parent))
            && self
                .tag
                .as_ref()
                .is_none_or(|tag| session.tags.contains(tag))
            && self
                .lifecycle
                .is_none_or(|lifecycle| session.lifecycle == lifecycle)
    }
}

/// `sender: None` is the user. Core never infers the sender.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SendOptions {
    pub sender: Option<SessionId>,
}

impl SendOptions {
    pub fn from_agent(sender: SessionId) -> Self {
        Self {
            sender: Some(sender),
        }
    }
}

/// Input was submitted to the pane. Not read, not completed, not retried.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliveryReceipt {
    pub recipient: SessionId,
    pub pane: String,
    pub submitted_at: f64,
}

#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum RefreshResult {
    Unchanged(Session),
    Exited(Session),
}

impl RefreshResult {
    pub fn session(&self) -> &Session {
        match self {
            RefreshResult::Unchanged(session) | RefreshResult::Exited(session) => session,
        }
    }
}

/// Where a live session executes. Attach with `tmux -S <socket> attach -t <session_target>`
/// (prefix `TMUX=` from inside another tmux client).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionLocation {
    pub socket: PathBuf,
    pub session_target: String,
    pub pane_target: String,
}

/// Not `Clone`: one core per process is the intended shape; locks and the registry are owned.
pub struct TxCore {
    config: CoreConfig,
    store: SessionStore,
    tmux: Tmux,
    events: EventLog,
    engines: EngineRegistry,
    profiles: Profiles,
    worktrees: WorktreeManager,
    locks: Locks,
    sinks: Vec<Box<dyn EventSink>>,
    pub(crate) host: crate::plugin::Host,
}

/// `env` keys are shell-style names (`-e KEY=VALUE` cannot carry `=` or NUL; an empty key is
/// accepted by tmux but reaches no process).
fn validate_env(env: &[(String, String)]) -> Result<(), CoreError> {
    for (key, value) in env {
        let key_ok = !key.is_empty()
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.');
        if !key_ok || value.contains('\0') {
            return Err(CoreError::InvalidEnvKey(key.clone()));
        }
    }
    Ok(())
}

/// tmux's ways of saying the session we just made is already gone: no server behind the socket,
/// the session is not there (`can't find session` from session-typed targets, `no such session`
/// from the pane-typed `set-option` target), or the server quit while answering (`exit-empty`
/// after the last session ended, which is ours). Anything else is an unknown answer, never read
/// as "exited".
fn session_vanished(stderr: &str) -> bool {
    crate::tmux::is_no_server(stderr)
        || stderr.starts_with("can't find session")
        || stderr.starts_with("no such session")
        || crate::tmux::server_quit(stderr)
}

/// `env[key] = value` for each pair: replaces in place, else appends.
fn merge_env(env: &mut Vec<(String, String)>, layer: &[(String, String)]) {
    for (key, value) in layer {
        match env.iter_mut().find(|(existing, _)| existing == key) {
            Some(slot) => slot.1.clone_from(value),
            None => env.push((key.clone(), value.clone())),
        }
    }
}

fn event(
    type_: EventType,
    session: &Session,
    actor: Option<SessionId>,
    detail: impl Into<String>,
) -> Event {
    Event::new(
        type_,
        session.id.clone(),
        session.name.clone(),
        actor,
        detail,
    )
}

/// The process is gone, whatever the adapter last said: `ended_at` is set and an agent's state
/// becomes `Ended`. No `StateChanged`: `Exited` / `Stopped` already say so.
fn mark_ended(session: &mut Session, lifecycle: Lifecycle) {
    let ended_at = now();
    session.lifecycle = lifecycle;
    session.ended_at = Some(ended_at);
    if session.state.is_some() {
        session.state = Some(LlmEngineState::Ended);
    }
}

fn store_io(path: &Path) -> impl FnOnce(std::io::Error) -> CoreError + '_ {
    move |source| {
        CoreError::Store(StoreError::Io {
            path: path.to_owned(),
            source,
        })
    }
}

impl std::fmt::Debug for TxCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxCore")
            .field("directories", &self.config.directories)
            .field("socket", &self.tmux.socket())
            .field("engines", &self.engines.ids())
            .finish_non_exhaustive()
    }
}

impl TxCore {
    /// Checks the socket path, creates the layout (`<data>/sessions`, `<state>/locks`,
    /// `<state>/profiles`, `<run>` private to the user), writes a default `tmux.conf` if none.
    /// Does not start tmux.
    pub fn open(config: CoreConfig) -> Result<Self, CoreError> {
        config.check_socket_path()?;
        let directories = &config.directories;
        for dir in [
            directories.data.join(SESSIONS_DIR),
            directories.state.join(LOCKS_DIR),
            directories.state.join(PROFILES_DIR),
            directories.run.clone(),
        ] {
            std::fs::create_dir_all(&dir).map_err(store_io(&dir))?;
        }
        std::fs::set_permissions(&directories.run, std::fs::Permissions::from_mode(0o700))
            .map_err(store_io(&directories.run))?;
        let conf = config.tmux_conf_path();
        if !conf.exists() {
            std::fs::write(&conf, DEFAULT_TMUX_CONF).map_err(store_io(&conf))?;
        }
        Ok(Self {
            store: SessionStore::new(directories.data.join(SESSIONS_DIR)),
            tmux: Tmux::new(TMUX_BINARY, config.socket_path(), conf),
            events: EventLog::new(directories.data.join(LOG_FILE)),
            engines: EngineRegistry::new(),
            profiles: Profiles::new(config.profile_dirs.clone(), config.skill_dirs.clone()),
            worktrees: WorktreeManager::new(config.worktrees_dir()),
            locks: Locks::new(directories.state.join(LOCKS_DIR)),
            sinks: Vec::new(),
            host: Default::default(),
            config,
        })
    }

    pub fn config(&self) -> &CoreConfig {
        &self.config
    }

    /// `<state>/profiles/<id>.md`: the composed role prompt a session was launched with.
    fn profile_path(&self, id: &SessionId) -> PathBuf {
        self.config
            .directories
            .state
            .join(PROFILES_DIR)
            .join(format!("{id}.md"))
    }

    pub fn socket_path(&self) -> &Path {
        self.tmux.socket()
    }

    pub fn register_engine(&mut self, adapter: Box<dyn LlmEngine>) -> Result<(), CoreError> {
        self.engines.register(adapter)
    }

    pub fn engine_ids(&self) -> Vec<&str> {
        self.engines.ids()
    }

    /// Registers an in-process listener for every event this core emits. See [`EventSink`].
    pub fn subscribe(&mut self, sink: Box<dyn EventSink>) {
        self.sinks.push(sink);
    }

    /// The log line first (the file never lags the sinks), then every sink in order. Runs only
    /// once the operation has released its locks, so a sink may use another core on the same
    /// state dir. A sink that panics is contained; the operation already happened.
    fn emit(&self, events: Vec<Event>) {
        for event in &events {
            // The log is an operation trail, not the record; a failed append must not fail the
            // operation that already happened.
            let _ = self.events.append(event);
            for sink in &self.sinks {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    sink.on_event(event);
                }));
            }
        }
    }

    // ----- reads: stored snapshots, never tmux ------------------------------------------------

    pub fn list_sessions(&self, filter: &ListFilter) -> Result<Vec<SessionId>, CoreError> {
        Ok(self
            .store
            .all()?
            .into_iter()
            .filter(|session| filter.matches(session))
            .map(|session| session.id)
            .collect())
    }

    pub fn get_session(&self, id: &SessionId) -> Result<Session, CoreError> {
        self.store
            .load(id)?
            .ok_or_else(|| CoreError::NotFound(id.to_string()))
    }

    /// An id first, then a name: the `Running` record with it, else the most recently created.
    pub fn resolve(&self, name_or_id: &str) -> Result<SessionId, CoreError> {
        if let Ok(id) = name_or_id.parse::<SessionId>()
            && self.store.load(&id)?.is_some()
        {
            return Ok(id);
        }
        let mut named: Vec<Session> = self
            .store
            .all()?
            .into_iter()
            .filter(|session| session.name == name_or_id)
            .collect();
        if let Some(running) = named
            .iter()
            .find(|session| session.lifecycle == Lifecycle::Running)
        {
            return Ok(running.id.clone());
        }
        named
            .pop()
            .map(|session| session.id)
            .ok_or_else(|| CoreError::NotFound(name_or_id.to_owned()))
    }

    // ----- create ------------------------------------------------------------------------------

    /// Starts a session and returns its id. An agent runs in its own detached worktree of the
    /// checkout `opts.cwd`, under the first free name (`opts.name`, else `opts.name-2` …); the
    /// record carries the name and the worktree. A process runs in `opts.cwd` as asked.
    pub fn create_session(&self, opts: CreateOptions) -> Result<SessionId, CoreError> {
        let (id, events) = self.create_locked(opts, None)?;
        self.emit(events);
        Ok(id)
    }

    /// A new agent continuing `source`'s conversation in a branch, in a new worktree made from
    /// the source's working directory at its committed `HEAD` (uncommitted changes stay
    /// behind). It runs the source's engine with its roles and a copy of its engine data (model
    /// and effort included) under `options.options`, carries `parent = source`, and starts with
    /// `options.prompt`. The source keeps running. `NotAnAgent` for a process session; whether
    /// the source has a conversation to branch is the engine's answer, surfaced as its
    /// `EngineError`.
    pub fn fork_session(
        &self,
        source: &SessionId,
        options: ForkOptions,
    ) -> Result<SessionId, CoreError> {
        let (id, events) = self.fork_locked(source, options)?;
        self.emit(events);
        Ok(id)
    }

    fn fork_locked(
        &self,
        source: &SessionId,
        options: ForkOptions,
    ) -> Result<(SessionId, Vec<Event>), CoreError> {
        if options.prompt.trim().is_empty() {
            return Err(CoreError::PromptRequired);
        }
        // A snapshot under the source's lock; the fork never holds the source afterwards.
        let source = {
            let _session = self.locks.session(source).map_err(CoreError::Lock)?;
            self.get_session(source)?
        };
        let Launch::Agent {
            engine, profile, ..
        } = &source.launch
        else {
            return Err(CoreError::NotAnAgent(source.id.clone()));
        };
        let name = options
            .name
            .unwrap_or_else(|| format!("{}-fork", source.name));
        let launch = LaunchOptions::Agent {
            engine: engine.clone(),
            profile: profile.clone(),
            options: options.options,
            initial_prompt: Some(options.prompt),
            resume: None,
        };
        let opts = CreateOptions {
            name,
            cwd: source.cwd.clone(),
            parent: Some(source.id.clone()),
            tags: options.tags.unwrap_or_else(|| source.tags.clone()),
            env: Vec::new(),
            launch,
        };
        self.create_locked(opts, Some(&source))
    }

    /// `fork_of`: the record whose conversation the new agent branches; `None` for a plain
    /// create.
    fn create_locked(
        &self,
        opts: CreateOptions,
        fork_of: Option<&Session>,
    ) -> Result<(SessionId, Vec<Event>), CoreError> {
        validate_name(&opts.name)?;
        validate_env(&opts.env)?;
        if !opts.cwd.is_dir() {
            return Err(CoreError::NotADirectory(opts.cwd));
        }
        // The real directory: git is asked about the real checkout, a process runs in the real
        // directory, and the record and every workspace path say so.
        let cwd = std::fs::canonicalize(&opts.cwd).map_err(store_io(&opts.cwd))?;
        let opts = CreateOptions { cwd, ..opts };
        // Pure checks before the lock: an unknown engine, a missing prompt or an unknown role never
        // touches state.
        let (engine, roles) = match &opts.launch {
            LaunchOptions::Agent {
                engine,
                profile,
                initial_prompt,
                resume,
                ..
            } => {
                let adapter = self
                    .engines
                    .get(engine)
                    .ok_or_else(|| CoreError::UnknownEngine(engine.clone()))?;
                // A resume carries its history instead; PR 6 removes resume-by-create.
                let has_prompt = initial_prompt
                    .as_deref()
                    .is_some_and(|prompt| !prompt.trim().is_empty());
                if !has_prompt && resume.is_none() {
                    return Err(CoreError::PromptRequired);
                }
                (
                    Some((adapter, self.profiles.resolve(profile)?)),
                    profile.clone(),
                )
            }
            LaunchOptions::Process { program, .. } => {
                if program.is_empty() {
                    return Err(CoreError::InvalidName(program.clone()));
                }
                (None, Vec::new())
            }
            #[allow(unreachable_patterns)]
            _ => unreachable!("LaunchOptions variants are all handled"),
        };

        let _create = self.locks.create().map_err(CoreError::Lock)?;
        if engine.is_none() {
            // A process runs where it is asked to, under the name it is asked for.
            if self.name_in_use(&opts.name, None)? {
                return Err(CoreError::NameTaken(opts.name));
            }
            return self.launch_locked(opts, engine, roles, fork_of);
        }
        // An agent gets its own detached worktree at the starting checkout's `HEAD`, under the
        // first name free among the running records and on disk (`name`, `name-2`, … `name-99`).
        // A directory that is not a checkout fails here, with git's own words, before anything
        // else exists. Whatever fails after this point takes the worktree with it.
        let unavailable_names = self.running_names()?;
        let (name, worktree) =
            self.worktrees
                .create_unique(&opts.cwd, &opts.name, &unavailable_names)?;
        validate_name(&name)
            .and_then(|()| std::fs::canonicalize(&worktree).map_err(store_io(&worktree)))
            .and_then(|cwd| {
                self.launch_locked(CreateOptions { name, cwd, ..opts }, engine, roles, fork_of)
            })
            .inspect_err(|_| {
                let _ = self.worktrees.remove(&worktree);
            })
    }

    /// Under the create lock, with `opts.name` free and `opts.cwd` the directory the session
    /// runs in: the profile, the engine's plan, the tmux session, then the record. A fork
    /// (`fork_of`) is launched in `LaunchMode::Fork` and announced as `Forked`, not `Created`.
    fn launch_locked(
        &self,
        opts: CreateOptions,
        engine: Option<(&dyn LlmEngine, ResolvedProfile)>,
        roles: Vec<String>,
        fork_of: Option<&Session>,
    ) -> Result<(SessionId, Vec<Event>), CoreError> {
        let id = SessionId::mint();
        let profile_file = self.profile_path(&id);
        let cleanup_profile = || {
            let _ = std::fs::remove_file(&profile_file);
        };

        let mut env = vec![
            (SESSION_ID_ENV.to_owned(), id.to_string()),
            (
                TMUX_SOCKET_ENV.to_owned(),
                self.tmux.socket().to_string_lossy().into_owned(),
            ),
        ];
        // So a `txc` / `txi` run inside the session opens this same core: tmux does not pass the
        // caller's `XDG_*` into sessions.
        for (key, directory) in self.config.directories.environment() {
            env.push((key.to_owned(), directory.to_string_lossy().into_owned()));
        }
        merge_env(&mut env, &self.config.env);
        merge_env(&mut env, &opts.env);

        // The record exists before the engine is asked: the engine reads its id, cwd and data
        // from it. A resume names its conversation there, under the engine's `ext` key; a fork
        // starts from a copy of the source's data (its options among it), which the engine's
        // `configure` then adjusts and its fork plan replaces with the fork's own. Core copies
        // the value whole; it never reads into it.
        let mut ext = std::collections::BTreeMap::new();
        let launch = match (&opts.launch, &engine) {
            (LaunchOptions::Agent { engine, resume, .. }, Some((adapter, _))) => {
                if let Some(resume) = resume.as_deref().filter(|resume| !resume.is_empty()) {
                    ext.insert(
                        adapter.ext_key().to_owned(),
                        serde_json::json!({ "conversation_id": resume }),
                    );
                }
                if let Some(data) = fork_of.and_then(|source| source.ext.get(adapter.ext_key())) {
                    ext.insert(adapter.ext_key().to_owned(), data.clone());
                }
                Launch::Agent {
                    engine: engine.clone(),
                    profile: roles,
                    model: None,
                    effort: None,
                    resume: resume.clone(),
                }
            }
            (LaunchOptions::Process { program, args, .. }, _) => Launch::Process {
                program: program.clone(),
                args: args.clone(),
            },
            (LaunchOptions::Agent { .. }, None) => unreachable!("engine resolved above"),
        };
        let created_at = now();
        let mut session = Session {
            id: id.clone(),
            name: opts.name.clone(),
            parent: opts.parent,
            tags: opts.tags,
            cwd: opts.cwd.clone(),
            created_at,
            ended_at: None,
            last_modified_at: created_at,
            lifecycle: Lifecycle::Running,
            // An agent starts in `Starting` until its adapter's first change; a process has
            // no engine and so no state.
            state: match &launch {
                Launch::Agent { .. } => Some(LlmEngineState::Starting),
                Launch::Process { .. } => None,
            },
            launch,
            ext,
        };

        let (program, args) = match (&opts.launch, engine) {
            (
                LaunchOptions::Agent {
                    options,
                    initial_prompt,
                    ..
                },
                Some((adapter, resolved)),
            ) => {
                // A resume already named its conversation in the record.
                let mode = match fork_of {
                    Some(source) => LaunchMode::Fork { source },
                    None if session.ext.contains_key(adapter.ext_key()) => LaunchMode::Resume,
                    None => LaunchMode::Start,
                };
                // The engine validates the options and merges them into its data, which the
                // launch then reads. A refusal here has written nothing yet.
                let engine_data = adapter.configure(&session, options)?;
                session
                    .ext
                    .insert(adapter.ext_key().to_owned(), engine_data);
                let profile =
                    Profile::write(resolved, &profile_file).map_err(store_io(&profile_file))?;
                let request = LaunchRequest {
                    session: &session,
                    mode,
                    profile: &profile,
                    initial_prompt: initial_prompt.as_deref(),
                    hook_command: &self.config.hook_command,
                };
                let plan = adapter
                    .launch(&request)
                    .inspect_err(|_| cleanup_profile())?;
                apply_workspace(&opts.cwd, &plan.workspace).inspect_err(|_| cleanup_profile())?;
                validate_env(&plan.env).inspect_err(|_| cleanup_profile())?;
                merge_env(&mut env, &plan.env);
                if plan.program.is_empty() {
                    cleanup_profile();
                    return Err(CoreError::InvalidName(plan.program));
                }
                if let Some(engine_data) = plan.engine_data {
                    session
                        .ext
                        .insert(adapter.ext_key().to_owned(), engine_data);
                }
                (plan.program, plan.args)
            }
            (LaunchOptions::Process { program, args, .. }, _) => (program.clone(), args.clone()),
            (LaunchOptions::Agent { .. }, None) => unreachable!("engine resolved above"),
        };

        if let Err(error) = self
            .tmux
            .new_session(&opts.name, &opts.cwd, &program, &args, &env)
        {
            cleanup_profile();
            return Err(match error {
                TmuxError::Failed { stderr, .. } if stderr.starts_with("duplicate session") => {
                    CoreError::NameTaken(opts.name)
                }
                other => CoreError::Tmux(other),
            });
        }
        let undo = |error: CoreError| {
            let _ = self.tmux.kill_session(&exact(&opts.name));
            cleanup_profile();
            error
        };
        // A program that exits at once takes its session (and, if it was the last one, the
        // server) with it before `@tx-id` can be set. That is a session that ran and exited,
        // not a failure to create one: the record says so.
        let exited_at_once = match self.tmux.set_tx_id(&opts.name, &id) {
            Ok(()) => false,
            Err(TmuxError::Failed { stderr, .. }) if session_vanished(&stderr) => true,
            Err(error) => return Err(undo(CoreError::Tmux(error))),
        };
        if exited_at_once {
            session.ended_at = Some(session.created_at);
            session.lifecycle = Lifecycle::Exited;
            session.state = session.state.map(|_| LlmEngineState::Ended);
        }
        if let Err(error) = self.save(&mut session) {
            return Err(undo(CoreError::Store(error)));
        }
        let mut events = vec![match fork_of {
            Some(source) => event(EventType::Forked, &session, None, source.id.as_str()),
            None => event(EventType::Created, &session, None, ""),
        }];
        if exited_at_once {
            events.push(event(
                EventType::Exited,
                &session,
                None,
                "exited before registration",
            ));
        }
        Ok((id, events))
    }

    /// Every record write goes through here: stamps `last_modified_at`, then saves.
    fn save(&self, session: &mut Session) -> Result<(), StoreError> {
        session.last_modified_at = now();
        self.store.save(session)
    }

    /// The names of the `Running` records: what a new agent's name must not collide with.
    fn running_names(&self) -> Result<std::collections::HashSet<String>, CoreError> {
        Ok(self
            .store
            .all()?
            .into_iter()
            .filter(|session| session.lifecycle == Lifecycle::Running)
            .map(|session| session.name)
            .collect())
    }

    /// Whether a `Running` record other than `except` carries `name`.
    fn name_in_use(&self, name: &str, except: Option<&SessionId>) -> Result<bool, CoreError> {
        Ok(self.store.all()?.iter().any(|session| {
            session.lifecycle == Lifecycle::Running
                && session.name == name
                && Some(&session.id) != except
        }))
    }

    // ----- mutations -------------------------------------------------------------------------

    pub fn rename_session(&self, id: &SessionId, name: &str) -> Result<Session, CoreError> {
        let (session, events) = self.rename_locked(id, name)?;
        self.emit(events);
        Ok(session)
    }

    fn rename_locked(
        &self,
        id: &SessionId,
        name: &str,
    ) -> Result<(Session, Vec<Event>), CoreError> {
        validate_name(name)?;
        let _create = self.locks.create().map_err(CoreError::Lock)?;
        let _session = self.locks.session(id).map_err(CoreError::Lock)?;
        let mut session = self.get_session(id)?;
        if session.name == name {
            return Ok((session, Vec::new()));
        }
        if self.name_in_use(name, Some(id))? {
            return Err(CoreError::NameTaken(name.to_owned()));
        }
        if session.lifecycle == Lifecycle::Running
            && let Some(live) = self.tmux.is_live(id)?
        {
            match self.tmux.rename_session(&live.session_target, name) {
                Ok(()) => {}
                Err(TmuxError::Failed { stderr, .. })
                    if stderr.starts_with("duplicate session") =>
                {
                    return Err(CoreError::NameTaken(name.to_owned()));
                }
                Err(error) => return Err(CoreError::Tmux(error)),
            }
        }
        let old = std::mem::replace(&mut session.name, name.to_owned());
        self.save(&mut session)?;
        let renamed = event(
            EventType::Renamed,
            &session,
            None,
            format!("{old} -> {name}"),
        );
        Ok((session, vec![renamed]))
    }

    /// Runs `f` on `ext[plugin]` (`Null` when absent) under the session lock and saves the
    /// record if the value changed; leaving it `Null` removes the key. Returns whether a write
    /// happened. `f` must not call back into the core (the lock is held).
    pub fn update_ext(
        &self,
        id: &SessionId,
        plugin: &str,
        f: impl FnOnce(&mut serde_json::Value),
    ) -> Result<bool, CoreError> {
        let _session = self.locks.session(id).map_err(CoreError::Lock)?;
        let mut session = self.get_session(id)?;
        let before = session
            .ext
            .get(plugin)
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let mut value = before.clone();
        f(&mut value);
        if value == before {
            return Ok(false);
        }
        if value.is_null() {
            session.ext.remove(plugin);
        } else {
            session.ext.insert(plugin.to_owned(), value);
        }
        self.save(&mut session)?;
        Ok(true)
    }

    /// A hook the session's engine wired fired inside it: the engine decides the new state and
    /// its own data, core writes both under the session lock (one save) and emits
    /// `StateChanged` (`from → to`) when the state moved and `EngineDataChanged` (the ext key)
    /// when `ext[ext_key]` was rewritten. Core reads `ext` only to hand the engine its own key,
    /// never to interpret it. Returns the state after the hook; `None` when the hook is not for
    /// a live agent this core can read: the session is no longer `Running` (a stale hook), is a
    /// process session, or its engine is not registered here.
    pub fn hook(
        &self,
        id: &SessionId,
        hook: LlmEngineHook,
    ) -> Result<Option<LlmEngineState>, CoreError> {
        let (state, changed) = {
            let _session = self.locks.session(id).map_err(CoreError::Lock)?;
            let mut session = self.get_session(id)?;
            if session.lifecycle != Lifecycle::Running {
                return Ok(None);
            }
            let Launch::Agent { engine, .. } = &session.launch else {
                return Ok(None);
            };
            let Some(adapter) = self.engines.get(engine) else {
                return Ok(None);
            };
            let current = session.state.expect("agent sessions carry a state");
            let change = adapter.on_hook(&session, &hook);
            let mut changed = Vec::new();
            let state = match change.state {
                Some(to) if to != current => {
                    session.state = Some(to);
                    let detail = format!("{} → {}", current.as_str(), to.as_str());
                    changed.push(event(EventType::StateChanged, &session, None, detail));
                    to
                }
                _ => current,
            };
            let ext_key = adapter.ext_key();
            if let Some(value) = change.engine_data
                && session.ext.get(ext_key) != Some(&value)
            {
                session.ext.insert(ext_key.to_owned(), value);
                changed.push(event(EventType::EngineDataChanged, &session, None, ext_key));
            }
            if !changed.is_empty() {
                self.save(&mut session)?;
            }
            (state, changed)
        };
        self.emit(changed);
        Ok(Some(state))
    }

    pub fn set_session_tags(
        &self,
        id: &SessionId,
        tags: Vec<String>,
    ) -> Result<Session, CoreError> {
        let (session, tagged) = {
            let _session = self.locks.session(id).map_err(CoreError::Lock)?;
            let mut session = self.get_session(id)?;
            session.tags = tags;
            self.save(&mut session)?;
            let tagged = event(EventType::Tagged, &session, None, session.tags.join(", "));
            (session, tagged)
        };
        self.emit(vec![tagged]);
        Ok(session)
    }

    /// One `list-sessions`; a `Running` record without a live `@tx-id` becomes `Exited`.
    pub fn refresh_session(&self, id: &SessionId) -> Result<RefreshResult, CoreError> {
        let (result, exited) = {
            let _session = self.locks.session(id).map_err(CoreError::Lock)?;
            let session = self.get_session(id)?;
            if session.lifecycle.is_terminal() || self.tmux.list_live()?.contains(id) {
                (RefreshResult::Unchanged(session), None)
            } else {
                let (session, exited) = self.mark_exited(session)?;
                (RefreshResult::Exited(session), Some(exited))
            }
        };
        self.emit(exited.into_iter().collect());
        Ok(result)
    }

    /// One `list-sessions` for every record; returns the records that changed. Records are
    /// scanned before tmux is listed: a record exists only after its tmux session does, so a
    /// record missing from a later listing has truly gone (the other order would mark a session
    /// created between the two calls as exited).
    pub fn refresh_all(&self) -> Result<Vec<Session>, CoreError> {
        let records = self.store.all()?;
        let live = self.tmux.list_live()?;
        let mut changed = Vec::new();
        for session in records {
            if session.lifecycle != Lifecycle::Running || live.contains(&session.id) {
                continue;
            }
            let exited = {
                let _lock = self.locks.session(&session.id).map_err(CoreError::Lock)?;
                // Re-load under the lock: another process may have stopped it meanwhile.
                match self.store.load(&session.id)? {
                    Some(current) if current.lifecycle == Lifecycle::Running => {
                        Some(self.mark_exited(current)?)
                    }
                    _ => None,
                }
            };
            // Emitted per record, right after its lock is released: a later failure cannot
            // swallow an exit that was already saved, and sinks see each exit as it happens.
            if let Some((session, exited)) = exited {
                self.emit(vec![exited]);
                changed.push(session);
            }
        }
        Ok(changed)
    }

    /// Saves the record as `Exited`; the caller emits the returned event once its lock is gone.
    fn mark_exited(&self, mut session: Session) -> Result<(Session, Event), CoreError> {
        mark_ended(&mut session, Lifecycle::Exited);
        self.save(&mut session)?;
        let exited = event(EventType::Exited, &session, None, "");
        Ok((session, exited))
    }

    pub fn get_execution_location(&self, id: &SessionId) -> Result<ExecutionLocation, CoreError> {
        if self.get_session(id)?.lifecycle.is_terminal() {
            return Err(CoreError::NotRunning(id.clone()));
        }
        let live = self.live_or_not_running(id)?;
        Ok(ExecutionLocation {
            socket: self.tmux.socket().to_owned(),
            session_target: live.session_target,
            pane_target: live.pane_target,
        })
    }

    /// The recipient's engine frames the message when it is registered here; a `Process`
    /// session, or an agent whose engine this core does not know, gets the default envelope.
    fn envelope_for(&self, recipient: &Session, sender: Option<&str>, text: &str) -> String {
        let adapter = match &recipient.launch {
            Launch::Agent { engine, .. } => self.engines.get(engine),
            Launch::Process { .. } => None,
        };
        match adapter {
            Some(adapter) => adapter.envelope(sender, text),
            None => build_envelope(sender, text),
        }
    }

    fn live_or_not_running(&self, id: &SessionId) -> Result<Live, CoreError> {
        self.tmux
            .is_live(id)?
            .ok_or_else(|| CoreError::NotRunning(id.clone()))
    }

    /// `kill-session`, then `Stopped`. A record whose session had already vanished becomes
    /// `Exited` instead (what happened, not what was asked).
    pub fn stop_session(&self, id: &SessionId) -> Result<Session, CoreError> {
        let (session, ended) = {
            let _session = self.locks.session(id).map_err(CoreError::Lock)?;
            let mut session = self.get_session(id)?;
            if session.lifecycle.is_terminal() {
                return Err(CoreError::NotRunning(id.clone()));
            }
            match self.tmux.is_live(id)? {
                None => self.mark_exited(session)?,
                Some(live) => {
                    self.tmux.kill_session(&live.session_target)?;
                    mark_ended(&mut session, Lifecycle::Stopped);
                    self.save(&mut session)?;
                    let stopped = event(EventType::Stopped, &session, None, "");
                    (session, stopped)
                }
            }
        };
        self.emit(vec![ended]);
        Ok(session)
    }

    /// Deletes the record (and its profile file). The tmux session, if any, keeps running and is
    /// no longer managed. `Ok(false)` when there was no record.
    pub fn forget_session(&self, id: &SessionId) -> Result<bool, CoreError> {
        let (existed, name) = {
            let _session = self.locks.session(id).map_err(CoreError::Lock)?;
            let name = self
                .store
                .load(id)
                .ok()
                .flatten()
                .map(|session| session.name);
            let existed = self.store.delete(id)?;
            let _ = std::fs::remove_file(self.profile_path(id));
            (existed, name)
        };
        if existed {
            self.emit(vec![Event::new(
                EventType::Forgotten,
                id.clone(),
                name.unwrap_or_default(),
                None,
                "",
            )]);
        }
        Ok(existed)
    }

    // ----- messaging ---------------------------------------------------------------------------

    pub fn send_message(
        &self,
        to: &SessionId,
        text: &str,
        opts: SendOptions,
    ) -> Result<DeliveryReceipt, CoreError> {
        let sender_name = match &opts.sender {
            Some(sender) => Some(self.get_session(sender)?.name),
            None => None,
        };
        let (receipt, sent) = {
            let _session = self.locks.session(to).map_err(CoreError::Lock)?;
            let recipient = self.get_session(to)?;
            if recipient.lifecycle != Lifecycle::Running {
                return Err(CoreError::NotRunning(to.clone()));
            }
            let live = self.live_or_not_running(to)?;
            let envelope = self.envelope_for(&recipient, sender_name.as_deref(), text);
            deliver(&self.tmux, &live.pane_target, &envelope, DELIVERY_PAUSE)?;
            let submitted_at = now();
            let sent = event(
                EventType::MessageSent,
                &recipient,
                opts.sender.clone(),
                format!("from {}", sender_name.as_deref().unwrap_or("user")),
            );
            (
                DeliveryReceipt {
                    recipient: to.clone(),
                    pane: live.pane_target,
                    submitted_at,
                },
                sent,
            )
        };
        self.emit(vec![sent]);
        Ok(receipt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Directories;
    use crate::engine::{EngineError, LaunchPlan};

    fn dir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("txc")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[test]
    fn open_creates_the_layout_and_a_default_conf() {
        let dir = dir();
        let directories = Directories::under(dir.path());
        let core = TxCore::open(CoreConfig::new(directories.clone())).unwrap();
        for sub in [
            directories.data.join("sessions"),
            directories.state.join("locks"),
            directories.state.join("profiles"),
        ] {
            assert!(sub.is_dir(), "{}", sub.display());
        }
        let run = std::fs::metadata(&directories.run).unwrap();
        assert_eq!(run.permissions().mode() & 0o777, 0o700);
        let conf = std::fs::read_to_string(directories.data.join("tmux.conf")).unwrap();
        assert!(conf.starts_with("# tx-core"), "{conf}");
        assert!(conf.contains("exit-empty"), "{conf}");
        assert_eq!(core.socket_path(), directories.run.join("tmux.sock"));
        assert_eq!(core.config().directories, directories);
        assert!(!directories.config.exists());
        assert!(!directories.cache.exists());
        assert!(core.engine_ids().is_empty());
    }

    #[test]
    fn open_keeps_an_existing_conf() {
        let dir = dir();
        let directories = Directories::under(dir.path());
        std::fs::create_dir_all(&directories.data).unwrap();
        std::fs::write(directories.data.join("tmux.conf"), "mine\n").unwrap();
        TxCore::open(CoreConfig::new(directories.clone())).unwrap();
        assert_eq!(
            std::fs::read_to_string(directories.data.join("tmux.conf")).unwrap(),
            "mine\n"
        );
    }

    #[test]
    fn open_with_a_long_socket_path_creates_nothing() {
        let dir = dir();
        let root = dir.path().join("s".repeat(120));
        let error = TxCore::open(CoreConfig::new(Directories::under(&root))).unwrap_err();
        assert!(
            matches!(error, CoreError::SocketPathTooLong(_)),
            "{error:?}"
        );
        assert!(!root.exists());
        let mut config = CoreConfig::new(Directories::under(dir.path().join("short")));
        config.tmux_socket = Some(dir.path().join("s".repeat(120)));
        assert!(TxCore::open(config).is_err());
        assert!(!dir.path().join("short").exists());
    }

    #[test]
    fn open_fails_when_the_data_dir_is_a_file() {
        let dir = dir();
        let directories = Directories::under(dir.path());
        std::fs::write(&directories.data, "").unwrap();
        assert!(matches!(
            TxCore::open(CoreConfig::new(directories)).unwrap_err(),
            CoreError::Store(_)
        ));
    }

    struct Stub;
    impl LlmEngine for Stub {
        fn on_hook(&self, _: &Session, _: &crate::LlmEngineHook) -> crate::LlmEngineChange {
            crate::LlmEngineChange::NONE
        }

        fn id(&self) -> &str {
            "stub"
        }

        fn ext_key(&self) -> &'static str {
            "stub"
        }

        fn configure(
            &self,
            _: &Session,
            options: &serde_json::Value,
        ) -> Result<serde_json::Value, EngineError> {
            Ok(options.clone())
        }

        fn launch(&self, _: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
            Ok(LaunchPlan::default())
        }
    }

    #[test]
    fn register_engine_refuses_duplicates() {
        let dir = dir();
        let mut core =
            TxCore::open(CoreConfig::new(Directories::under(dir.path().join("s")))).unwrap();
        core.register_engine(Box::new(Stub)).unwrap();
        assert!(matches!(
            core.register_engine(Box::new(Stub)).unwrap_err(),
            CoreError::EngineTaken(_)
        ));
        assert_eq!(core.engine_ids(), ["stub"]);
    }

    #[test]
    fn env_keys_are_validated() {
        assert!(validate_env(&[("A".into(), "1".into()), ("B_c-d".into(), "".into())]).is_ok());
        for (key, value) in [
            ("", "1"),
            ("A=B", "1"),
            ("A", "x\0y"),
            ("A\0", "1"),
            ("A B", "1"),
        ] {
            assert!(
                matches!(validate_env(&[(key.into(), value.into())]), Err(CoreError::InvalidEnvKey(k)) if k == key),
                "{key:?}"
            );
        }
    }

    #[test]
    fn env_merge_replaces_in_place_and_appends() {
        let mut env = vec![
            ("A".to_owned(), "1".to_owned()),
            ("B".to_owned(), "2".to_owned()),
        ];
        merge_env(
            &mut env,
            &[("B".into(), "3".into()), ("C".into(), "4".into())],
        );
        assert_eq!(
            env,
            [
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "3".to_owned()),
                ("C".to_owned(), "4".to_owned())
            ]
        );
    }

    #[test]
    fn create_fails_fast_without_touching_tmux() {
        // No tmux binary needed: the failures come before any call.
        let dir = dir();
        let directories = Directories::under(dir.path().join("s"));
        let mut config = CoreConfig::new(directories.clone());
        config.env = vec![("OK".into(), "1".into())];
        let core = TxCore::open(config).unwrap();
        let cwd = dir.path().to_path_buf();
        let opts = |launch: LaunchOptions| CreateOptions::new("n", cwd.clone(), launch);
        let process = || LaunchOptions::process("sleep", ["1"]);
        let mut bad = opts(process());
        bad.name = "no spaces".into();
        assert!(matches!(
            core.create_session(bad).unwrap_err(),
            CoreError::InvalidName(_)
        ));
        let mut bad = opts(process());
        bad.env = vec![("=".into(), "1".into())];
        assert!(matches!(
            core.create_session(bad).unwrap_err(),
            CoreError::InvalidEnvKey(_)
        ));
        let mut bad = opts(process());
        bad.cwd = dir.path().join("missing");
        assert!(matches!(
            core.create_session(bad).unwrap_err(),
            CoreError::NotADirectory(_)
        ));
        let bad = opts(LaunchOptions::agent("ghost"));
        assert!(
            matches!(core.create_session(bad).unwrap_err(), CoreError::UnknownEngine(e) if e == "ghost")
        );
        let bad = opts(LaunchOptions::process("", Vec::<String>::new()));
        assert!(matches!(
            core.create_session(bad).unwrap_err(),
            CoreError::InvalidName(_)
        ));
        assert!(
            std::fs::read_dir(directories.data.join("sessions"))
                .unwrap()
                .next()
                .is_none()
        );
        assert!(
            std::fs::read_dir(directories.state.join("profiles"))
                .unwrap()
                .next()
                .is_none()
        );
        assert!(!directories.data.join("log.jsonl").exists());
    }

    #[test]
    fn only_a_gone_session_counts_as_vanished() {
        assert!(session_vanished("no server running on /tmp/x.sock"));
        assert!(session_vanished(
            "error connecting to /tmp/x.sock (No such file or directory)"
        ));
        assert!(session_vanished("can't find session: =x"));
        assert!(session_vanished("server exited unexpectedly"));
        assert!(session_vanished("no such session: =quick:"));
        for unknown in [
            "error connecting to /tmp/x.sock (Resource temporarily unavailable)",
            "error connecting to /tmp/x.sock (Socket operation on non-socket)",
            "lost server",
            "",
        ] {
            assert!(!session_vanished(unknown), "{unknown:?}");
        }
    }

    #[test]
    fn filters_match_records() {
        let session = |parent: Option<&str>, tags: &[&str], lifecycle| Session {
            id: "i".parse().unwrap(),
            name: "n".into(),
            parent: parent.map(|p| p.parse().unwrap()),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            cwd: "/".into(),
            created_at: 1.0,
            ended_at: None,
            last_modified_at: 1.0,
            lifecycle,
            launch: Launch::Process {
                program: "x".into(),
                args: vec![],
            },
            ext: std::collections::BTreeMap::new(),
            state: None,
        };
        let filter = ListFilter::new()
            .parent("p".parse().unwrap())
            .tag("t")
            .lifecycle(Lifecycle::Running);
        assert!(filter.matches(&session(Some("p"), &["a", "t"], Lifecycle::Running)));
        assert!(!filter.matches(&session(None, &["t"], Lifecycle::Running)));
        assert!(!filter.matches(&session(Some("p"), &["a"], Lifecycle::Running)));
        assert!(!filter.matches(&session(Some("p"), &["t"], Lifecycle::Exited)));
        assert!(ListFilter::default().matches(&session(None, &[], Lifecycle::Stopped)));
    }
}
