//! `TxCore`: the public operations. One struct, built once by `open`, owning its collaborators.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::config::{AGENTS_DIR, CoreConfig};
use crate::engine::{
    Conversation, EngineError, EngineKind, EngineRegistry, Input, LaunchRequest, LlmEngine,
    apply_workspace,
};
use crate::error::CoreError;
use crate::events::{Event, EventLog, EventSink, EventType, now};
use crate::locks::Locks;
use crate::messaging::{DELIVERY_PAUSE, EnvelopeSender, build_envelope, deliver};
use crate::profile::{Profile, Profiles, ResolvedProfile};
use crate::session::{Session, SessionId, validate_name};
use crate::state::{LlmEngineHook, LlmEngineState};
use crate::store::{SessionStore, SkippedRecord, StoreError};
use crate::tmux::{Live, Tmux, TmuxError, exact};
use crate::worktree::WorktreeManager;

/// The version-1 layout's record directory (`<data>/sessions/<id>.json`); `TxCore::open`
/// refuses while it still holds records, and `txc migrate` upgrades them.
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
    /// The name asked for. An agent may get `<name>-2` … `<name>-99` instead when an unarchived
    /// record, running or not, holds the name; read it back from the record. Its worktree
    /// directory is labelled separately, first free on disk.
    pub name: String,
    /// The starting directory: a git checkout to branch from; the agent runs in its own
    /// detached worktree at that checkout's `HEAD`, and the record's `working_directory` names
    /// the worktree.
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

/// How to start the agent. One variant: core holds agents only (Storage PR 14 flattens it).
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LaunchOptions {
    Agent {
        engine: EngineKind,
        profile: Vec<String>,
        /// Engine options as a JSON object (`{"model": "fable", "effort": "high"}`): the engine
        /// validates them (`LlmEngine::configure`) and they land in `ext[ext_key]` before the
        /// launch. Core never interprets a key.
        options: serde_json::Value,
        /// The agent's first message. Required: an engine never starts silently.
        initial_prompt: String,
    },
}

impl LaunchOptions {
    pub fn agent(engine: EngineKind) -> Self {
        Self::Agent {
            engine,
            profile: Vec::new(),
            options: serde_json::Value::Object(serde_json::Map::new()),
            initial_prompt: String::new(),
        }
    }

    /// Role names, in order.
    pub fn profile<I, S>(mut self, roles: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let Self::Agent { profile, .. } = &mut self;
        profile.extend(roles.into_iter().map(Into::into));
        self
    }

    /// Replaces the engine options with `options`, a JSON object.
    pub fn options(mut self, options: serde_json::Value) -> Self {
        let Self::Agent { options: slot, .. } = &mut self;
        *slot = options;
        self
    }

    /// Sets one engine option (`option("model", "fable")`); a repeated key replaces.
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

    pub fn initial_prompt(mut self, prompt: impl Into<String>) -> Self {
        let Self::Agent { initial_prompt, .. } = &mut self;
        *initial_prompt = prompt.into();
        self
    }
}

/// Every field is a conjunct; `None` matches everything. `archived` picks a side: `false` (the
/// default) lists only unarchived records, `true` only archived ones; `ended` picks running or
/// ended records when set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ListFilter {
    pub parent: Option<SessionId>,
    pub tag: Option<String>,
    /// `Some(false)`: running records only (`state` not `Ended`); `Some(true)`: ended ones.
    pub ended: Option<bool>,
    pub archived: bool,
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

    /// Running records only: `state` not `Ended`.
    pub fn running(mut self) -> Self {
        self.ended = Some(false);
        self
    }

    /// Ended records only.
    pub fn ended(mut self) -> Self {
        self.ended = Some(true);
        self
    }

    /// Lists the archived records instead of the unarchived ones.
    pub fn archived(mut self) -> Self {
        self.archived = true;
        self
    }

    pub fn matches(&self, session: &Session) -> bool {
        self.parent
            .as_ref()
            .is_none_or(|parent| session.parent_id.as_ref() == Some(parent))
            && self
                .tag
                .as_ref()
                .is_none_or(|tag| session.tags.contains(tag))
            && self
                .ended
                .is_none_or(|ended| (session.state == LlmEngineState::Ended) == ended)
            && session.archived_at.is_some() == self.archived
    }
}

/// Who sends a message. Core never infers the sender.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sender {
    /// The operator; `via` names where they typed (a view, an nvim buffer).
    User { via: Option<String> },
    /// Another agent.
    Agent { id: SessionId },
}

impl Default for Sender {
    fn default() -> Self {
        Self::User { via: None }
    }
}

/// The default sender is the user, with nothing saying where from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SendOptions {
    pub sender: Sender,
}

impl SendOptions {
    pub fn from_agent(id: SessionId) -> Self {
        Self {
            sender: Sender::Agent { id },
        }
    }
}

impl From<Sender> for SendOptions {
    fn from(sender: Sender) -> Self {
        Self { sender }
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

fn exited_before_registration(session: &Session) -> Event {
    event(
        EventType::Exited,
        session,
        None,
        "exited before registration",
    )
}

/// Under an agent's archive directory: the lock one archive holds while it runs.
pub const ARCHIVE_LOCK: &str = ".archive.lock";

/// The per-session archive lock, taken without waiting: `None` when another archive holds it.
/// Creates the directory. Released when the file is dropped.
fn archive_lock(directory: &Path) -> Result<Option<std::fs::File>, EngineError> {
    use crate::mirror::io_error;
    std::fs::create_dir_all(directory).map_err(io_error(directory))?;
    let path = directory.join(ARCHIVE_LOCK);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(io_error(&path))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(source)) => Err(io_error(&path)(source)),
    }
}

/// The process is gone, whatever the adapter last said: the state becomes `Ended`. No
/// `StateChanged`: `Exited` / `Stopped` already say so, and the save stamps when.
fn mark_ended(session: &mut Session) {
    session.state = LlmEngineState::Ended;
}

/// How long `stop` waits for the pane's process to exit on its own: first after the engine's
/// own stop input, then after `kill-session`, before killing it outright.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// How long `stop` waits after `SIGKILL`.
const KILL_WAIT: Duration = Duration::from_secs(2);
const EXIT_POLL: Duration = Duration::from_millis(50);

/// Whether a process with `pid` still exists (`kill(pid, 0)`). One we may not signal exists.
fn process_exists(pid: u32) -> bool {
    // SAFETY: signal 0 delivers nothing; the call only checks the pid.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Polls until the process is gone or `timeout` has passed; whether it is gone.
fn wait_for_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while process_exists(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(EXIT_POLL);
    }
    true
}

/// `kill-session` closed the pane, but the CLI may still be wrapping up (its exit hooks fire
/// through `txi hook`). Waits for it, then kills it outright and waits once more, so no late
/// hook of this run can reach a respawn. Whether it is gone.
fn wait_or_kill(pid: u32) -> bool {
    if wait_for_exit(pid, STOP_WAIT) {
        return true;
    }
    // SAFETY: a plain signal to a pid read from the pane moments ago.
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    wait_for_exit(pid, KILL_WAIT)
}

/// The engine side of a launch: the adapter, the roles composed for it, and how it opens the
/// conversation.
struct EngineLaunch<'a> {
    adapter: &'a dyn LlmEngine,
    resolved: ResolvedProfile,
    opening: Opening<'a>,
}

/// Which of the engine's launch methods opens the conversation, with its arguments.
enum Opening<'a> {
    Start {
        prompt: &'a str,
    },
    Resume,
    Fork {
        source: &'a Session,
        prompt: &'a str,
    },
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
            .field("engines", &self.engines.kinds())
            .finish_non_exhaustive()
    }
}

impl TxCore {
    /// Checks the socket path, creates the layout (`<data>/agents`, `<state>/locks`,
    /// `<state>/profiles`, `<run>` private to the user), writes a default `tmux.conf` if none.
    /// Does not start tmux. Refuses with `NeedsMigration` while `<data>/sessions/` still holds
    /// version-1 records: only `txc migrate` ([`TxCore::open_for_migration`]) reads those.
    pub fn open(config: CoreConfig) -> Result<Self, CoreError> {
        config.check_socket_path()?;
        let sessions = config.directories.data.join(SESSIONS_DIR);
        let records = crate::migrate::version_one_records(&config.directories.data)
            .map_err(store_io(&sessions))?;
        if !records.is_empty() {
            return Err(CoreError::NeedsMigration {
                directory: sessions,
                records: records.len(),
            });
        }
        Self::open_unchecked(config)
    }

    /// [`TxCore::open`] without the version-1 check, for the migration itself.
    pub fn open_for_migration(config: CoreConfig) -> Result<Self, CoreError> {
        config.check_socket_path()?;
        Self::open_unchecked(config)
    }

    fn open_unchecked(config: CoreConfig) -> Result<Self, CoreError> {
        let directories = &config.directories;
        for dir in [
            directories.data.join(AGENTS_DIR),
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
            store: SessionStore::new(directories.data.join(AGENTS_DIR)),
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

    pub(crate) fn store(&self) -> &SessionStore {
        &self.store
    }

    pub(crate) fn engines(&self) -> &EngineRegistry {
        &self.engines
    }

    pub(crate) fn locks(&self) -> &Locks {
        &self.locks
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

    /// The kinds with an adapter registered, sorted: every enabled engine plugin's.
    pub fn engine_kinds(&self) -> Vec<EngineKind> {
        self.engines.kinds()
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

    /// The ids of the process records in the store: written before PR 11, when core still
    /// held non-LLM sessions. Every read skips them (a front end reports them at startup);
    /// Storage PR 14's migration moves them out.
    pub fn skipped_records(&self) -> Result<Vec<SkippedRecord>, CoreError> {
        Ok(self.store.skipped()?)
    }

    pub fn get_session(&self, id: &SessionId) -> Result<Session, CoreError> {
        self.store
            .load(id)?
            .ok_or_else(|| CoreError::NotFound(id.to_string()))
    }

    /// The session's current conversation, asked from its engine. `None`: not known yet.
    /// `EngineDisabled` when the record's engine is not registered (its plugin disabled).
    pub fn conversation(&self, id: &SessionId) -> Result<Option<Conversation>, CoreError> {
        let session = self.get_session(id)?;
        let adapter = self
            .engines
            .get(session.engine)
            .ok_or(CoreError::EngineDisabled(session.engine))?;
        Ok(adapter.conversation(&session))
    }

    /// An id first, then a name: the `Running` record with it, else the most recently created
    /// unarchived one, else the most recently created archived one (so an archived agent can
    /// still be shown and respawned by name).
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
            .find(|session| session.state != LlmEngineState::Ended)
        {
            return Ok(running.id.clone());
        }
        if let Some(unarchived) = named
            .iter()
            .rev()
            .find(|session| session.archived_at.is_none())
        {
            return Ok(unarchived.id.clone());
        }
        named
            .pop()
            .map(|session| session.id)
            .ok_or_else(|| CoreError::NotFound(name_or_id.to_owned()))
    }

    // ----- create ------------------------------------------------------------------------------

    /// Starts an agent and returns its id. It runs in its own detached worktree of the
    /// checkout `opts.cwd`, under the first free name (`opts.name`, else `opts.name-2` …); the
    /// record carries the name and the worktree.
    pub fn create_session(&self, opts: CreateOptions) -> Result<SessionId, CoreError> {
        let (id, events) = self.create_locked(opts, None)?;
        self.emit(events);
        Ok(id)
    }

    /// A new agent continuing `source`'s conversation in a branch, in a new worktree made from
    /// the source's working directory at its committed `HEAD` (uncommitted changes stay
    /// behind). It runs the source's engine with its roles and a copy of its engine data (model
    /// and effort included) under `options.options`, carries `parent = source`, and starts with
    /// `options.prompt`. The source keeps running. Whether the source has a conversation to
    /// branch is the engine's answer, surfaced as its `EngineError`.
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
        let name = options
            .name
            .unwrap_or_else(|| format!("{}-fork", source.name));
        let launch = LaunchOptions::Agent {
            engine: source.engine,
            profile: source.roles.clone(),
            options: options.options,
            initial_prompt: options.prompt,
        };
        let opts = CreateOptions {
            name,
            cwd: source.working_directory.clone(),
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
        // The real directory: git is asked about the real checkout, and the record and every
        // workspace path say so.
        let cwd = std::fs::canonicalize(&opts.cwd).map_err(store_io(&opts.cwd))?;
        let opts = CreateOptions { cwd, ..opts };
        // Pure checks before the lock: an unknown engine, a missing prompt or an unknown role never
        // touches state.
        let LaunchOptions::Agent {
            engine,
            profile,
            initial_prompt,
            ..
        } = &opts.launch;
        let adapter = self
            .engines
            .get(*engine)
            .ok_or(CoreError::EngineDisabled(*engine))?;
        if initial_prompt.trim().is_empty() {
            return Err(CoreError::PromptRequired);
        }
        let engine = (adapter, self.profiles.resolve(profile)?);
        let roles = profile.clone();

        let _create = self.locks.create().map_err(CoreError::Lock)?;
        // The agent gets its own detached worktree at the starting checkout's `HEAD`, under the
        // first name free among the unarchived records (`name`, `name-2`, … `name-99`), in a
        // directory free on disk.
        // A directory that is not a checkout fails here, with git's own words, before anything
        // else exists. Whatever fails after this point takes the worktree with it.
        let unavailable_names = self.unarchived_names()?;
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
    /// runs in: a fresh record, its engine data, then the shared launch path. A fork
    /// (`fork_of`) is launched by the engine's `fork` and announced as `Forked`, not `Created`.
    fn launch_locked(
        &self,
        opts: CreateOptions,
        engine: (&dyn LlmEngine, ResolvedProfile),
        roles: Vec<String>,
        fork_of: Option<&Session>,
    ) -> Result<(SessionId, Vec<Event>), CoreError> {
        let id = SessionId::mint();
        // The record exists before the engine is asked: the engine reads its id, cwd and data
        // from it. A fork starts from a copy of the source's data (its options among it), which
        // the engine's `configure` then adjusts and its fork plan replaces with the fork's own.
        // Core copies the value whole; it never reads into it.
        let mut ext = std::collections::BTreeMap::new();
        let (adapter, resolved) = engine;
        let LaunchOptions::Agent {
            engine,
            options,
            initial_prompt,
            ..
        } = opts.launch;
        if let Some(data) = fork_of.and_then(|source| source.ext.get(adapter.ext_key())) {
            ext.insert(adapter.ext_key().to_owned(), data.clone());
        }
        let created_at = now();
        let mut session = Session {
            id: id.clone(),
            name: opts.name,
            engine,
            working_directory: opts.cwd,
            roles,
            // `Starting` until the adapter's first change.
            state: LlmEngineState::Starting,
            created_at,
            last_modified_at: created_at,
            archived_at: None,
            tags: opts.tags,
            parent_id: opts.parent,
            ext,
            other: serde_json::Map::new(),
        };
        // The engine validates the options and merges them into its data, which the launch
        // then reads. A refusal here has written nothing yet.
        let engine_data = adapter.configure(&session, &options)?;
        session
            .ext
            .insert(adapter.ext_key().to_owned(), engine_data);
        let engine = EngineLaunch {
            adapter,
            resolved,
            opening: match fork_of {
                Some(source) => Opening::Fork {
                    source,
                    prompt: &initial_prompt,
                },
                None => Opening::Start {
                    prompt: &initial_prompt,
                },
            },
        };
        let exited_at_once = self.start_locked(&mut session, engine, &opts.env)?;
        let mut events = vec![match fork_of {
            Some(source) => event(EventType::Forked, &session, None, source.id.as_str()),
            None => event(EventType::Created, &session, None, ""),
        }];
        if exited_at_once {
            events.push(exited_before_registration(&session));
        }
        Ok((id, events))
    }

    /// The shared launch path, under the create lock: `session` is the record to run, already
    /// `Running` (and `Starting`), under a free name, in its `cwd`. The profile is written,
    /// the engine's plan taken and its workspace applied; then tmux starts the program with
    /// `extra_env` on top of core's environment, `@tx-id` links the session, and the record is
    /// saved. A program that exits at once takes its session (and, if it was the
    /// last one, the server) with it before `@tx-id` can be set: that is a session that ran and
    /// exited, not a failure to start one; the record says so and `true` comes back. A failure
    /// leaves no tmux session and no profile behind.
    fn start_locked(
        &self,
        session: &mut Session,
        engine: EngineLaunch<'_>,
        extra_env: &[(String, String)],
    ) -> Result<bool, CoreError> {
        let profile_file = self.profile_path(&session.id);
        let cleanup_profile = || {
            let _ = std::fs::remove_file(&profile_file);
        };

        let mut env = vec![
            (SESSION_ID_ENV.to_owned(), session.id.to_string()),
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
        merge_env(&mut env, extra_env);

        let profile =
            Profile::write(engine.resolved, &profile_file).map_err(store_io(&profile_file))?;
        let request = LaunchRequest {
            session,
            profile: &profile,
            hook_command: &self.config.hook_command,
        };
        let plan = match engine.opening {
            Opening::Start { prompt } => engine.adapter.start(&request, prompt),
            Opening::Resume => engine.adapter.resume(&request),
            Opening::Fork { source, prompt } => engine.adapter.fork(&request, source, prompt),
        }
        .inspect_err(|_| cleanup_profile())?;
        apply_workspace(&session.working_directory, &plan.workspace)
            .inspect_err(|_| cleanup_profile())?;
        validate_env(&plan.env).inspect_err(|_| cleanup_profile())?;
        merge_env(&mut env, &plan.env);
        if plan.program.is_empty() {
            cleanup_profile();
            return Err(CoreError::InvalidName(plan.program));
        }
        if let Some(engine_data) = plan.engine_data {
            session
                .ext
                .insert(engine.adapter.ext_key().to_owned(), engine_data);
        }
        let (program, args) = (plan.program, plan.args);

        if let Err(error) = self.tmux.new_session(
            &session.name,
            &session.working_directory,
            &program,
            &args,
            &env,
        ) {
            cleanup_profile();
            return Err(match error {
                TmuxError::Failed { stderr, .. } if stderr.starts_with("duplicate session") => {
                    CoreError::NameTaken(session.name.clone())
                }
                other => CoreError::Tmux(other),
            });
        }
        let target = exact(&session.name);
        let undo = |error: CoreError| {
            let _ = self.tmux.kill_session(&target);
            cleanup_profile();
            error
        };
        let exited_at_once = match self.tmux.set_tx_id(&session.name, &session.id) {
            Ok(()) => false,
            Err(TmuxError::Failed { stderr, .. }) if session_vanished(&stderr) => true,
            Err(error) => return Err(undo(CoreError::Tmux(error))),
        };
        if exited_at_once {
            mark_ended(session);
        }
        if let Err(error) = self.save(session) {
            return Err(undo(CoreError::Store(error)));
        }
        Ok(exited_at_once)
    }

    /// Starts a new process for an ended agent in its own record: the same id, worktree and
    /// conversation (the engine's `resume` continues the one it recorded under its `ext`
    /// key). `StillRunning` while the record is `Running` or tmux still has the
    /// session; `WorktreeMissing` when `cwd` is gone, since a respawn never recreates one;
    /// `NameHeld` when an unarchived record took the name meanwhile. Comes back `Starting`,
    /// with `archived_at` cleared, and announced as `Respawned`.
    pub fn respawn_session(&self, id: &SessionId) -> Result<Session, CoreError> {
        let (session, events) = self.respawn_locked(id)?;
        self.emit(events);
        Ok(session)
    }

    fn respawn_locked(&self, id: &SessionId) -> Result<(Session, Vec<Event>), CoreError> {
        let _create = self.locks.create().map_err(CoreError::Lock)?;
        let _session = self.locks.session(id).map_err(CoreError::Lock)?;
        let mut session = self.get_session(id)?;
        let adapter = self
            .engines
            .get(session.engine)
            .ok_or(CoreError::EngineDisabled(session.engine))?;
        let resolved = self.profiles.resolve(&session.roles)?;
        if session.state != LlmEngineState::Ended || self.tmux.is_live(id)?.is_some() {
            return Err(CoreError::StillRunning(id.clone()));
        }
        if !session.working_directory.is_dir() {
            return Err(CoreError::WorktreeMissing(
                session.working_directory.clone(),
            ));
        }
        if let Some(holder) = self.name_holder(&session.name, Some(id))? {
            return Err(CoreError::NameHeld {
                name: session.name.clone(),
                holder,
            });
        }
        session.archived_at = None;
        session.state = LlmEngineState::Starting;
        adapter.restore(&session, &self.config.archive_directory(id))?;
        let engine = EngineLaunch {
            adapter,
            resolved,
            opening: Opening::Resume,
        };
        let exited_at_once = self.start_locked(&mut session, engine, &[])?;
        let mut events = vec![event(EventType::Respawned, &session, None, "")];
        if exited_at_once {
            events.push(exited_before_registration(&session));
        }
        Ok((session, events))
    }

    /// Every record write goes through here: stamps `last_modified_at`, then saves.
    fn save(&self, session: &mut Session) -> Result<(), StoreError> {
        session.last_modified_at = now();
        self.store.save(session)
    }

    /// The names of the unarchived records, running or not: what a new agent's name must not
    /// collide with.
    fn unarchived_names(&self) -> Result<std::collections::HashSet<String>, CoreError> {
        Ok(self
            .store
            .all()?
            .into_iter()
            .filter(|session| session.archived_at.is_none())
            .map(|session| session.name)
            .collect())
    }

    /// The unarchived record other than `except`, running or not, that carries `name`.
    fn name_holder(
        &self,
        name: &str,
        except: Option<&SessionId>,
    ) -> Result<Option<SessionId>, CoreError> {
        Ok(self
            .store
            .all()?
            .into_iter()
            .find(|session| {
                session.archived_at.is_none() && session.name == name && Some(&session.id) != except
            })
            .map(|session| session.id))
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
        if self.name_holder(name, Some(id))?.is_some() {
            return Err(CoreError::NameTaken(name.to_owned()));
        }
        if session.state != LlmEngineState::Ended
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
    ///
    /// Every hook then has the engine archive the conversation into
    /// `CoreConfig::archive_directory`, after the session lock is released and under a
    /// non-blocking per-session archive lock (a concurrent archive makes this one skip). Every
    /// hook, not only one that reports `Idle`: Claude writes a turn's final reply after its
    /// `Stop` hook has returned, so the next hook (the next prompt, a `SessionEnd` before a
    /// `/clear`) is what brings the archive up to date. An archive failure is an
    /// `ArchiveFailed` event, never an error: the hook already happened and must not fail the
    /// agent.
    pub fn hook(
        &self,
        id: &SessionId,
        hook: LlmEngineHook,
    ) -> Result<Option<LlmEngineState>, CoreError> {
        let (state, changed, session) = {
            let _session = self.locks.session(id).map_err(CoreError::Lock)?;
            let mut session = self.get_session(id)?;
            if session.state == LlmEngineState::Ended {
                return Ok(None);
            }
            let Some(adapter) = self.engines.get(session.engine) else {
                return Ok(None);
            };
            let current = session.state;
            let change = adapter.on_hook(&session, &hook);
            let mut changed = Vec::new();
            let state = match change.state {
                Some(to) if to != current => {
                    session.state = to;
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
            (state, changed, session)
        };
        self.emit(changed);
        self.archive_conversation(&session);
        Ok(Some(state))
    }

    /// The engine's `archive` for an agent record, under `<archive directory>/.archive.lock`
    /// taken without waiting: a caller that finds the lock held leaves the archive to the
    /// holder. Failure is an `ArchiveFailed` event, never an error. Called after every hook, and
    /// after a stop or an exit, once the process is gone and the transcript final. An
    /// unregistered engine archives nothing.
    fn archive_conversation(&self, session: &Session) {
        let Some(adapter) = self.engines.get(session.engine) else {
            return;
        };
        let directory = self.config.archive_directory(&session.id);
        let result = archive_lock(&directory).and_then(|lock| match lock {
            Some(_held) => adapter.archive(session, &directory),
            None => Ok(()),
        });
        if let Err(error) = result {
            self.emit(vec![event(
                EventType::ArchiveFailed,
                session,
                None,
                error.to_string(),
            )]);
        }
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
            if session.state == LlmEngineState::Ended || self.tmux.list_live()?.contains(id) {
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
            if session.state == LlmEngineState::Ended || live.contains(&session.id) {
                continue;
            }
            let exited = {
                let _lock = self.locks.session(&session.id).map_err(CoreError::Lock)?;
                // Re-load under the lock: another process may have stopped it meanwhile.
                match self.store.load(&session.id)? {
                    Some(current) if current.state != LlmEngineState::Ended => {
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
        mark_ended(&mut session);
        self.save(&mut session)?;
        self.archive_conversation(&session);
        let exited = event(EventType::Exited, &session, None, "");
        Ok((session, exited))
    }

    pub fn get_execution_location(&self, id: &SessionId) -> Result<ExecutionLocation, CoreError> {
        if self.get_session(id)?.state == LlmEngineState::Ended {
            return Err(CoreError::NotRunning(id.clone()));
        }
        let live = self.live_or_not_running(id)?;
        Ok(ExecutionLocation {
            socket: self.tmux.socket().to_owned(),
            session_target: live.session_target,
            pane_target: live.pane_target,
        })
    }

    /// The recipient's engine says what to type for a message when it is registered here; an
    /// agent whose engine this core does not know gets the default envelope, submitted.
    fn input_for(&self, recipient: &Session, sender: &EnvelopeSender<'_>, text: &str) -> Input {
        match self.engines.get(recipient.engine) {
            Some(adapter) => adapter.receive_message(sender, text),
            None => Input::line(build_envelope(sender, text)),
        }
    }

    /// Aborts the agent's running turn with its engine's `interrupt` input, and applies the
    /// state the engine says the agent is in afterwards (an aborted turn fires no end-of-turn
    /// hook on Claude), with its `StateChanged`, as for a hook. `NotRunning` when the record or
    /// tmux says so; `Engine(Refused)` when the engine has no such input.
    pub fn interrupt_session(&self, id: &SessionId) -> Result<(), CoreError> {
        let changed = {
            let _session = self.locks.session(id).map_err(CoreError::Lock)?;
            let mut session = self.get_session(id)?;
            if session.state == LlmEngineState::Ended {
                return Err(CoreError::NotRunning(id.clone()));
            }
            let adapter = self
                .engines
                .get(session.engine)
                .ok_or(CoreError::EngineDisabled(session.engine))?;
            let (input, state) = adapter.interrupt(&session).ok_or_else(|| {
                EngineError::Refused(format!("{} has no way to interrupt a turn", session.engine))
            })?;
            let live = self.live_or_not_running(id)?;
            deliver(&self.tmux, &live.pane_target, &input, DELIVERY_PAUSE)?;
            let current = session.state;
            let Some(to) = state.filter(|to| *to != current) else {
                return Ok(());
            };
            session.state = to;
            self.save(&mut session)?;
            let detail = format!("{} → {}", current.as_str(), to.as_str());
            event(EventType::StateChanged, &session, None, detail)
        };
        self.emit(vec![changed]);
        Ok(())
    }

    fn live_or_not_running(&self, id: &SessionId) -> Result<Live, CoreError> {
        self.tmux
            .is_live(id)?
            .ok_or_else(|| CoreError::NotRunning(id.clone()))
    }

    /// The engine's own stop input (or `kill-session` when it has none), `Stopped`, then a
    /// wait until the pane's process has really exited: `STOP_WAIT` for the CLI to leave by
    /// itself, then `kill-session` and `STOP_WAIT` more, then killed outright; so a late hook
    /// from the old process can never reach a respawn. The record is saved and the session lock
    /// released before the wait: the process's own exit hooks need that lock, and they find the
    /// record `Stopped` and are ignored. The create lock is held across the wait instead, which
    /// is the lock a respawn takes first. The `Stopped` event's detail is empty when the CLI
    /// left by itself and `killed` when it had to be killed. A record whose session had already
    /// vanished becomes `Exited` instead (what happened, not what was asked).
    pub fn stop_session(&self, id: &SessionId) -> Result<Session, CoreError> {
        let _create = self.locks.create().map_err(CoreError::Lock)?;
        let (session, ended) = self.stop_locked(id)?;
        drop(_create);
        self.emit(vec![ended]);
        Ok(session)
    }

    /// `stop_session` under the create lock the caller holds, the wait included; the caller
    /// emits the returned event.
    fn stop_locked(&self, id: &SessionId) -> Result<(Session, Event), CoreError> {
        let (session, live) = {
            let _session = self.locks.session(id).map_err(CoreError::Lock)?;
            let mut session = self.get_session(id)?;
            if session.state == LlmEngineState::Ended {
                return Err(CoreError::NotRunning(id.clone()));
            }
            let Some(live) = self.tmux.is_live(id)? else {
                let (session, exited) = self.mark_exited(session)?;
                return Ok((session, exited));
            };
            let stop = self
                .engines
                .get(session.engine)
                .and_then(|adapter| adapter.stop(&session));
            let pid = self.tmux.pane_pid(&live.pane_target)?;
            match &stop {
                Some(input) => deliver(&self.tmux, &live.pane_target, input, DELIVERY_PAUSE)?,
                None => {
                    self.tmux.kill_session(&live.session_target)?;
                }
            }
            mark_ended(&mut session);
            self.save(&mut session)?;
            (
                session,
                pid.map(|pid| (pid, live.session_target, stop.is_some())),
            )
        };
        let mut detail = "";
        if let Some((pid, target, asked_to_leave)) = live {
            let left = asked_to_leave && wait_for_exit(pid, STOP_WAIT);
            if !left {
                detail = "killed";
                if asked_to_leave {
                    self.tmux.kill_session(&target)?;
                }
                wait_or_kill(pid);
            }
            self.archive_conversation(&session);
        }
        let stopped = event(EventType::Stopped, &session, None, detail);
        Ok((session, stopped))
    }

    /// Hides the session from listings and frees its name; nothing is deleted (record,
    /// worktree, profile and `ext` stay), and `respawn_session` brings it back. A running
    /// session is stopped first, waiting as `stop_session` does. An already archived record
    /// comes back unchanged, with no event. The create lock is held throughout, so no stop or
    /// respawn slips in between the two steps.
    pub fn archive_session(&self, id: &SessionId) -> Result<Session, CoreError> {
        let _create = self.locks.create().map_err(CoreError::Lock)?;
        let mut events = Vec::new();
        if self.get_session(id)?.state != LlmEngineState::Ended {
            let (_, ended) = self.stop_locked(id)?;
            events.push(ended);
        }
        let archived = self.archive_locked(id).map(|(session, archived)| {
            events.extend(archived);
            session
        });
        drop(_create);
        self.emit(events);
        archived
    }

    fn archive_locked(&self, id: &SessionId) -> Result<(Session, Vec<Event>), CoreError> {
        let _session = self.locks.session(id).map_err(CoreError::Lock)?;
        let mut session = self.get_session(id)?;
        if session.archived_at.is_some() {
            return Ok((session, Vec::new()));
        }
        session.archived_at = Some(now());
        self.save(&mut session)?;
        let archived = event(EventType::Archived, &session, None, "");
        Ok((session, vec![archived]))
    }

    // ----- messaging ---------------------------------------------------------------------------

    pub fn send_message(
        &self,
        to: &SessionId,
        text: &str,
        opts: SendOptions,
    ) -> Result<DeliveryReceipt, CoreError> {
        let agent_name;
        let (envelope_sender, from) = match &opts.sender {
            Sender::User { via } => (
                EnvelopeSender::User {
                    via: via.as_deref(),
                },
                None,
            ),
            Sender::Agent { id } => {
                agent_name = self.get_session(id)?.name;
                (
                    EnvelopeSender::Agent { name: &agent_name },
                    Some(id.clone()),
                )
            }
        };
        let detail = match envelope_sender {
            EnvelopeSender::User { via: None } => "from user".to_owned(),
            EnvelopeSender::User { via: Some(via) } => format!("from user via {via}"),
            EnvelopeSender::Agent { name } => format!("from {name}"),
        };
        let (receipt, sent) = {
            let _session = self.locks.session(to).map_err(CoreError::Lock)?;
            let recipient = self.get_session(to)?;
            if recipient.state == LlmEngineState::Ended {
                return Err(CoreError::NotRunning(to.clone()));
            }
            let live = self.live_or_not_running(to)?;
            let input = self.input_for(&recipient, &envelope_sender, text);
            deliver(&self.tmux, &live.pane_target, &input, DELIVERY_PAUSE)?;
            let submitted_at = now();
            let sent = event(EventType::MessageSent, &recipient, from, detail);
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
            directories.data.join("agents"),
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
        assert!(core.engine_kinds().is_empty());
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

        fn kind(&self) -> EngineKind {
            EngineKind::Claude
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

        fn start(&self, _: &LaunchRequest<'_>, _: &str) -> Result<LaunchPlan, EngineError> {
            Ok(LaunchPlan::default())
        }

        fn resume(&self, _: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
            Ok(LaunchPlan::default())
        }

        fn fork(
            &self,
            _: &LaunchRequest<'_>,
            _: &Session,
            _: &str,
        ) -> Result<LaunchPlan, EngineError> {
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
        assert_eq!(core.engine_kinds(), [EngineKind::Claude]);
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
        // No engine is registered on this core: every kind is "disabled".
        let agent = || LaunchOptions::agent(EngineKind::Codex).initial_prompt("go");
        let mut bad = opts(agent());
        bad.name = "no spaces".into();
        assert!(matches!(
            core.create_session(bad).unwrap_err(),
            CoreError::InvalidName(_)
        ));
        let mut bad = opts(agent());
        bad.env = vec![("=".into(), "1".into())];
        assert!(matches!(
            core.create_session(bad).unwrap_err(),
            CoreError::InvalidEnvKey(_)
        ));
        let mut bad = opts(agent());
        bad.cwd = dir.path().join("missing");
        assert!(matches!(
            core.create_session(bad).unwrap_err(),
            CoreError::NotADirectory(_)
        ));
        let bad = opts(agent());
        assert!(matches!(
            core.create_session(bad).unwrap_err(),
            CoreError::EngineDisabled(EngineKind::Codex)
        ));
        assert!(
            std::fs::read_dir(directories.data.join("agents"))
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
        let session = |parent: Option<&str>, tags: &[&str], state| Session {
            id: "i".parse().unwrap(),
            name: "n".into(),
            engine: EngineKind::Claude,
            working_directory: "/".into(),
            roles: vec![],
            state,
            created_at: 1.0,
            last_modified_at: 1.0,
            archived_at: None,
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            parent_id: parent.map(|p| p.parse().unwrap()),
            ext: std::collections::BTreeMap::new(),
            other: serde_json::Map::new(),
        };
        let filter = ListFilter::new()
            .parent("p".parse().unwrap())
            .tag("t")
            .running();
        assert!(filter.matches(&session(Some("p"), &["a", "t"], LlmEngineState::Idle)));
        assert!(filter.matches(&session(Some("p"), &["t"], LlmEngineState::Starting)));
        assert!(!filter.matches(&session(None, &["t"], LlmEngineState::Idle)));
        assert!(!filter.matches(&session(Some("p"), &["a"], LlmEngineState::Idle)));
        assert!(!filter.matches(&session(Some("p"), &["t"], LlmEngineState::Ended)));
        assert!(ListFilter::default().matches(&session(None, &[], LlmEngineState::Ended)));
        assert!(
            ListFilter::new()
                .ended()
                .matches(&session(None, &[], LlmEngineState::Ended))
        );
        assert!(
            !ListFilter::new()
                .ended()
                .matches(&session(None, &[], LlmEngineState::Working))
        );
    }
}
