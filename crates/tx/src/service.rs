//! `SessionService` (service.py): the use-case core. Every mutation flows through it and logs
//! exactly one `EventLog` line (D8). It composes the record store, the tmux adapter, the
//! reconciler and the worktree manager; its collaborators are handed in by the caller (the
//! component wiring), and everything the reference reads from `os.environ` comes from the
//! [`Env`] snapshot.
//!
//! Resolution accepts an id or a name: the record file first, then a live `@tx_id` on a session
//! of that name, then a store name lookup preferring the live record (D7 — names are reusable).
//!
//! FIX quirks implemented here: Q25/Q39 (a worker named `""`, `.` or `..` is refused instead of
//! becoming `-2` / `.-2` / `..-2`), Q27 (every by-name tmux target is exact), Q32 (`revive` is the
//! explicit way back for an exited record whose session is still live), T-MSG-02 (the envelope is
//! typed literally, so a body that is a key name is not interpreted).

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use serde_json::Number;

use crate::app::Env;
use crate::engines::EngineRegistry;
use crate::engines::adapter::{EngineAdapter, EngineError, env_set};
use crate::events::{self, EventLog};
use crate::messages::{TAG_AGENT, TAG_USER, build_envelope};
use crate::read_only::{ReadOnlySandboxError, SandboxHost, System, wrap_read_only_command};
use crate::reconcile::{ReconcileError, Reconciler};
use crate::session::{
    ChatRef, Engine, LlmSession, NvimSocket, Origin, OtherRole, OtherSession, READ_ONLY_ENV,
    REQUIRE_WORKTREE_ENV, Role, SCHEMA_VERSION, Session, SessionKind, State,
};
use crate::shlex::shlex_quote;
use crate::spawn::{SpawnSpec, nvim_listen_command};
use crate::storage::Home;
use crate::store::{SessionStore, StoreError};
use crate::tmux::{MAX_COMMAND_BYTES, Tmux, TmuxError, format_envelope};
use crate::worktree::{WorktreeError, WorktreeManager};

/// The pause between typing a message and pressing Enter (an agent's input box drops an Enter
/// that arrives too fast).
const DELIVERY_PAUSE: Duration = Duration::from_millis(300);

/// Error from a caller's `before_spawn` hook.
pub type HookError = Box<dyn std::error::Error>;

/// A use-case precondition failed (or a collaborator did). The CLI prints `tx <verb>: <error>`
/// and exits 1; the `Display` texts are the reference's.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("session '{0}' already exists")]
    SessionExists(String),
    #[error("'{0}' is a live view session — pick another name")]
    LiveViewName(String),
    #[error("session '{0}' not found (no live @tx_id, no store record)")]
    SessionNotFound(String),
    #[error("target session '{0}' does not exist")]
    TargetNotFound(String),
    #[error("send-message must run inside tmux (needs the sender session name)")]
    NotInsideTmux,
    #[error("send-user-message must run inside a tx session ($TX_SESSION_ID is unset)")]
    NotInsideTxSession,
    #[error("a worker spawn requires an agent command")]
    NotAnAgent,
    #[error(
        "worker spawn has no engine on its spec — pass --engine (or an agent --cmd whose binary \
         tx recognizes)"
    )]
    NoEngine,
    #[error(
        "{engine} hooks are not installed in {} — the worker's chat id would never be captured \
         and the session could never be resumed. Install them: setup/engines/install.sh install \
         --engine {engine}",
        home.display()
    )]
    HooksNotInstalled { engine: Engine, home: PathBuf },
    /// The reference's registry raises `KeyError` here; the component wiring always registers
    /// every engine it lets a spawn name.
    #[error("no engine adapter is registered for {0}")]
    EngineNotRegistered(Engine),
    #[error("{0} command does not enforce the requested read-only mode")]
    ReadOnlyNotEnforced(Engine),
    /// Q25 / Q39 FIX: the reference derives `-2` / `.-2` / `..-2` from these.
    #[error("invalid worker name '{0}' — a worker name cannot be empty, '.' or '..'")]
    InvalidWorkerName(String),
    #[error("could not create worktree: {0}")]
    CreateWorktree(#[source] WorktreeError),
    #[error("could not resolve worktree name: {0}")]
    ResolveWorktreeName(#[source] WorktreeError),
    #[error("could not remove worktree: {0}")]
    RemoveWorktree(#[source] WorktreeError),
    #[error("could not enforce read-only process sandbox: {0}")]
    SandboxWorktree(#[source] WorktreeError),
    #[error("could not enforce read-only process sandbox: {0}")]
    Sandbox(#[source] ReadOnlySandboxError),
    /// `bind_artifact` on an llm record (an assertion in the reference).
    #[error("session '{0}' is not a non-llm session and cannot render an artifact")]
    NotAnArtifactView(String),
    /// A `WORKING` state for a non-llm record (an assertion in the reference).
    #[error("session '{0}' is not an llm session and cannot be working")]
    NotAnLlm(String),
    #[error("session '{0}' is archived — an archived record is never revived")]
    ReviveArchived(String),
    #[error("session '{0}' has no live tmux session to revive")]
    ReviveNotLive(String),
    #[error("{0}")]
    BeforeSpawn(HookError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Tmux(#[from] TmuxError),
    #[error(transparent)]
    Reconcile(#[from] ReconcileError),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

impl ServiceError {
    fn io(path: &Path) -> impl FnOnce(io::Error) -> ServiceError + '_ {
        move |source| ServiceError::Io {
            path: path.to_owned(),
            source,
        }
    }
}

type Result<T, E = ServiceError> = std::result::Result<T, E>;

/// What `revive` did.
#[derive(Debug)]
pub enum Revival {
    Revived(Session),
    /// The record was not terminal; nothing was written.
    AlreadyLive(Session),
}

pub struct SessionService {
    store: Rc<SessionStore>,
    tmux: Rc<Tmux>,
    events: Rc<EventLog>,
    engines: Rc<RefCell<EngineRegistry>>,
    home: Rc<Home>,
    env: Rc<Env>,
    worktrees: WorktreeManager,
    system: System,
}

impl SessionService {
    pub fn new(
        store: Rc<SessionStore>,
        tmux: Rc<Tmux>,
        events: Rc<EventLog>,
        engines: Rc<RefCell<EngineRegistry>>,
        home: Rc<Home>,
        env: Rc<Env>,
    ) -> Self {
        let worktrees = WorktreeManager::new(home.worktrees_dir());
        Self {
            store,
            tmux,
            events,
            engines,
            home,
            env,
            worktrees,
            system: System::host(),
        }
    }

    /// Override the platform the read-only sandbox targets (tests).
    pub fn with_system(mut self, system: System) -> Self {
        self.system = system;
        self
    }

    pub fn store(&self) -> &SessionStore {
        &self.store
    }

    pub fn tmux(&self) -> &Tmux {
        &self.tmux
    }

    pub fn worktrees(&self) -> &WorktreeManager {
        &self.worktrees
    }

    // ----- spawn ---------------------------------------------------------------------------

    /// Public spawn policy: every agent is a worker; non-agents launch directly.
    pub fn spawn(&self, spec: SpawnSpec) -> Result<Session> {
        if spec.role == Role::Llm {
            self.spawn_worker(spec, false)
        } else {
            self.launch(spec)
        }
    }

    /// Place an agent in a fresh tx-owned worktree (or, with `reuse_existing_worktree`, keep a
    /// linked worktree it already owns) and launch it.
    pub fn spawn_worker(&self, spec: SpawnSpec, reuse_existing_worktree: bool) -> Result<Session> {
        self.spawn_worker_with(spec, reuse_existing_worktree, |_| Ok(()))
    }

    /// [`Self::spawn_worker`] with a hook that sees the final spec just before tmux starts; a
    /// hook error aborts the spawn (and removes a worktree created for it).
    pub fn spawn_worker_with(
        &self,
        spec: SpawnSpec,
        reuse_existing_worktree: bool,
        before_spawn: impl FnOnce(&SpawnSpec) -> Result<(), HookError>,
    ) -> Result<Session> {
        if spec.role != Role::Llm {
            return Err(ServiceError::NotAnAgent);
        }
        refuse_reserved_name(&spec.name)?;
        let mut environment = spec.env.clone();
        if spec.read_only {
            env_set(&mut environment, READ_ONLY_ENV, "1".into());
            environment.retain(|(key, _)| key != REQUIRE_WORKTREE_ENV);
        } else {
            environment.retain(|(key, _)| key != READ_ONLY_ENV);
            env_set(&mut environment, REQUIRE_WORKTREE_ENV, "1".into());
        }

        if reuse_existing_worktree && self.worktrees.is_linked(Path::new(&spec.cwd)) {
            let directory = PathBuf::from(&spec.cwd);
            let prepared = self.prepare_worker_access(spec, &directory, environment)?;
            before_spawn(&prepared).map_err(ServiceError::BeforeSpawn)?;
            return self.launch(prepared);
        }

        let unavailable = self.live_names()?;
        let (name, directory) = self
            .worktrees
            .create_unique(Path::new(&spec.cwd), &spec.name, &unavailable)
            .map_err(ServiceError::CreateWorktree)?;
        let attempt = || -> Result<Session> {
            let prepared =
                self.prepare_worker_access(SpawnSpec { name, ..spec }, &directory, environment)?;
            before_spawn(&prepared).map_err(ServiceError::BeforeSpawn)?;
            self.launch(prepared)
        };
        attempt().inspect_err(|_| {
            let _ = self.worktrees.remove(&directory);
        })
    }

    fn prepare_worker_access(
        &self,
        spec: SpawnSpec,
        directory: &Path,
        environment: Vec<(String, String)>,
    ) -> Result<SpawnSpec> {
        let engine = spec.engine.ok_or(ServiceError::NoEngine)?;
        // Without hooks the chat id is never captured, so the session could never be resumed.
        if !self.home.engine_capture_shim(engine.as_str()).exists() {
            return Err(ServiceError::HooksNotInstalled {
                engine,
                home: self.home.root().to_owned(),
            });
        }
        let adapter = self.adapter(engine)?;
        let cwd = directory.to_string_lossy().into_owned();
        let command = adapter.prepare_workspace(&spec.cmd, &cwd, &environment)?;
        if spec.read_only && !adapter.is_read_only_command(&command) {
            return Err(ServiceError::ReadOnlyNotEnforced(engine));
        }
        let launch_cmd = self.worker_launch_command(&command, directory, spec.read_only)?;
        Ok(SpawnSpec {
            cwd,
            cmd: command,
            env: environment,
            launch_cmd: Some(launch_cmd),
            ..spec
        })
    }

    /// Execution command for a worker; records keep the unwrapped engine command.
    pub fn worker_launch_command(
        &self,
        command: &str,
        worktree_directory: &Path,
        read_only: bool,
    ) -> Result<String> {
        if !read_only {
            return Ok(command.to_owned());
        }
        let repository_worktrees = self
            .worktrees
            .repository_worktrees(worktree_directory)
            .map_err(ServiceError::SandboxWorktree)?;
        let git_common_directory = self
            .worktrees
            .git_common_directory(worktree_directory)
            .map_err(ServiceError::SandboxWorktree)?;
        let host = SandboxHost {
            system: &self.system,
            path: self.env.var("PATH").map(std::ffi::OsStr::new),
        };
        wrap_read_only_command(
            command,
            worktree_directory,
            &repository_worktrees,
            &git_common_directory,
            host,
        )
        .map_err(ServiceError::Sandbox)
    }

    /// Resolve the display / worktree name before an asynchronous handover is scheduled.
    pub fn next_worker_name(&self, starting_directory: &Path, base_name: &str) -> Result<String> {
        refuse_reserved_name(base_name)?;
        let unavailable = self.live_names()?;
        self.worktrees
            .next_name(starting_directory, base_name, &unavailable)
            .map_err(ServiceError::ResolveWorktreeName)
    }

    /// Remove a temporary worker's linked worktree after its session has ended.
    pub fn remove_worker_worktree(&self, directory: &Path) -> Result<()> {
        if !self.worktrees.is_linked(directory) {
            return Ok(());
        }
        self.worktrees
            .remove(directory)
            .map_err(ServiceError::RemoveWorktree)
    }

    /// D11: the companion listens on `nvim/<session-id>.sock`, recorded as `nvim_socket`.
    pub fn spawn_nvim(&self, spec: SpawnSpec) -> Result<Session> {
        self.launch_as(spec, Listen::NvimSocket)
    }

    /// Bring up a view: a live `@tx_view` tmux session, never a store record (no `@tx_id`, no
    /// tags). Returns the tmux session name (= the human name).
    pub fn spawn_view(&self, spec: SpawnSpec) -> Result<String> {
        if self.tmux.has_session(&spec.name) {
            return Err(ServiceError::SessionExists(spec.name));
        }
        self.require_name_free(&spec.name)?;
        self.tmux.new_session(
            &spec.name,
            &spec.cwd,
            spec.launch_cmd.as_deref().unwrap_or(&spec.cmd),
            &spec.env,
        )?;
        self.tmux.set_tx_view(&spec.name)?;
        let target = exact(&spec.name);
        self.tmux.set_option(&target, "status", "on", false)?;
        self.tmux
            .set_window_option(&target, "pane-border-status", "top")?;
        self.log("spawn-view", &format!("{} {}", spec.name, spec.cwd))?;
        Ok(spec.name)
    }

    /// A command past tmux's client-message limit runs through `launch/<session-id>.sh`
    /// (created 0700 before the contents land); the record keeps the full command.
    pub fn transportable_command(&self, session_id: &str, command: &str) -> Result<String> {
        if command.len() <= MAX_COMMAND_BYTES {
            return Ok(command.to_owned());
        }
        let directory = self.home.launch_dir();
        std::fs::create_dir_all(&directory).map_err(ServiceError::io(&directory))?;
        let script = directory.join(format!("{session_id}.sh"));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o700)
            .open(&script)
            .map_err(ServiceError::io(&script))?;
        file.write_all(format!("{command}\n").as_bytes())
            .map_err(ServiceError::io(&script))?;
        Ok(format!(
            "/bin/sh {}",
            shlex_quote(&script.to_string_lossy())
        ))
    }

    /// The shared spawn mechanics (`_spawn`): a detached tmux session named by a fresh uuid,
    /// `@tx_id` stamped, the record persisted, one log line.
    fn launch(&self, spec: SpawnSpec) -> Result<Session> {
        self.launch_as(spec, Listen::None)
    }

    fn launch_as(&self, spec: SpawnSpec, listen: Listen) -> Result<Session> {
        let session_id = uuid::Uuid::new_v4().to_string();
        self.require_name_free(&spec.name)?;
        if self.tmux.has_session(&session_id) {
            return Err(ServiceError::SessionExists(session_id));
        }
        let now = events::now();
        let now_number = Number::from_f64(now);
        let mut launch_env = vec![("TX_SESSION_ID".to_owned(), session_id.clone())];
        for (key, value) in &spec.env {
            env_set(&mut launch_env, key, value.clone());
        }
        let kind = match spec.role {
            Role::Llm => {
                let engine = spec.engine.ok_or(ServiceError::NoEngine)?;
                let mut chats = Vec::new();
                if !spec.records_own_chat {
                    // Capture-after-launch: a pending `original` ref the first hook fills in.
                    chats.push(ChatRef {
                        id: None,
                        role: "original".into(),
                        cwd: spec.cwd.clone(),
                        transcript_path: String::new(),
                        origin: Origin {
                            how: "spawn".into(),
                            session_id: session_id.clone(),
                            chat_id: None,
                        },
                        bundle_path: None,
                        started_at: now_number.clone(),
                        ended_at: None,
                        summary: String::new(),
                        engine: Some(engine),
                    });
                }
                SessionKind::Llm(LlmSession {
                    engine,
                    chats,
                    last_activity: now_number.clone(),
                    turn_started_at: None,
                })
            }
            role => SessionKind::Other(OtherSession {
                role: OtherRole::from_role(role).ok_or(ServiceError::NotAnAgent)?,
                artifact_id: None,
                nvim_socket: match listen {
                    Listen::None => NvimSocket::Unset,
                    Listen::NvimSocket => {
                        let directory = self.home.nvim_dir();
                        std::fs::create_dir_all(&directory)
                            .map_err(ServiceError::io(&directory))?;
                        let socket = directory.join(format!("{session_id}.sock"));
                        NvimSocket::Path(socket.to_string_lossy().into_owned())
                    }
                },
            }),
        };
        let parent = match spec.parent {
            Some(parent) => Some(parent),
            None => self.executor_parent(),
        };
        let mut command = spec.launch_cmd.clone().unwrap_or_else(|| spec.cmd.clone());
        if let Some(socket) = nvim_socket_of(&kind) {
            command = nvim_listen_command(&command, socket);
        }
        let command = self.transportable_command(&session_id, &command)?;
        let pid = self
            .tmux
            .new_session(&session_id, &spec.cwd, &command, &launch_env)?;
        self.tmux.set_tx_id(&session_id, &session_id)?;
        // No per-session `session-closed` hook (C2): liveness is the global hook + reconcile.
        let session = Session {
            id: session_id,
            name: spec.name,
            state: State::initial_for(spec.role),
            cwd: spec.cwd,
            initial_cmd: spec.cmd,
            tags: spec.tags,
            group: spec.group,
            spawn_env: spec.env,
            parent,
            pid: Some(Number::from(pid)),
            attached_to: Vec::new(),
            created_at: now_number,
            ended_at: None,
            schema_version: Number::from(SCHEMA_VERSION),
            kind,
        };
        self.store.save(&session)?;
        self.log(
            "spawn",
            &format!("{} [{}] {}", session.name, spec.role, session.cwd),
        )?;
        Ok(session)
    }

    /// The executing managed session's id (only a stable `@tx_id` is lineage), else `None`.
    fn executor_parent(&self) -> Option<String> {
        let current = self.tmux.current_session_name()?;
        self.tmux.get_tx_id(&current)
    }

    /// Refuse a spawn / rename onto a display name a live record holds, or a live view's name.
    /// Reconciles first so a vanished session's stale record does not block reuse (D7).
    pub fn require_name_free(&self, name: &str) -> Result<()> {
        if self.live_names()?.contains(name) {
            return Err(ServiceError::SessionExists(name.to_owned()));
        }
        if self.tmux.has_session(name) && self.tmux.is_view(name) {
            return Err(ServiceError::LiveViewName(name.to_owned()));
        }
        Ok(())
    }

    /// Q6: whether a live record holds `name` (after a reconcile) — the store-backed clash check
    /// `tx resume` needs.
    pub fn is_live_name(&self, name: &str) -> Result<bool> {
        Ok(self.live_names()?.contains(name))
    }

    /// Reconcile, then the display names of every live record.
    fn live_names(&self) -> Result<HashSet<String>> {
        self.reconcile()?;
        Ok(self
            .store
            .all()
            .into_iter()
            .filter(Session::is_alive)
            .map(|session| session.name)
            .collect())
    }

    // ----- lifecycle -----------------------------------------------------------------------

    /// End the tmux session (if live) and mark the record EXITED. When nothing resolves, a live
    /// `@tx_view` session of that name is ended instead (`Ok(None)`, there is no record).
    pub fn kill(&self, name_or_id: &str) -> Result<Option<Session>> {
        let Some(mut session) = self.resolve(name_or_id)? else {
            if self.tmux.has_session(name_or_id) && self.tmux.is_view(name_or_id) {
                self.tmux.kill_session(name_or_id);
                self.log("kill", name_or_id)?;
                return Ok(None);
            }
            return Err(ServiceError::SessionNotFound(name_or_id.to_owned()));
        };
        if self.tmux.has_session(session.tmux_name()) {
            self.tmux.kill_session(session.tmux_name());
        }
        let script = self.home.launch_dir().join(format!("{}.sh", session.id));
        let socket = nvim_socket_of(&session.kind).map(PathBuf::from);
        for leftover in std::iter::once(script).chain(socket) {
            match std::fs::remove_file(&leftover) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    return Err(ServiceError::io(&leftover)(error));
                }
                _ => {}
            }
        }
        if session.transition_to(State::Exited) {
            session.ended_at = Number::from_f64(events::now());
            session.attached_to.clear();
        }
        self.store.save(&session)?;
        self.log("kill", &session.name)?;
        Ok(Some(session))
    }

    /// Retire a record to ARCHIVED, keeping it.
    pub fn archive(&self, name_or_id: &str) -> Result<Session> {
        let mut session = self.require(name_or_id)?;
        if session.transition_to(State::Archived) {
            session.ended_at = Number::from_f64(events::now());
            session.attached_to.clear();
        }
        self.store.save(&session)?;
        self.log("archive", &session.name)?;
        Ok(session)
    }

    /// Q32: bring an EXITED record back to its role's initial state when its `@tx_id` session
    /// is still live (reconcile-on-read never does this). An archived record, or one whose
    /// session is gone, is refused; a non-terminal record is left untouched.
    pub fn revive(&self, name_or_id: &str) -> Result<Revival> {
        let mut session = self.require(name_or_id)?;
        match session.state {
            State::Archived => return Err(ServiceError::ReviveArchived(session.name)),
            State::Exited => {}
            _ => return Ok(Revival::AlreadyLive(session)),
        }
        let live = self.tmux.has_session(session.tmux_name())
            && self.tmux.get_tx_id(session.tmux_name()).as_deref() == Some(session.id.as_str());
        if !live {
            return Err(ServiceError::ReviveNotLive(session.name));
        }
        session.state = State::initial_for(session.role());
        session.ended_at = None;
        session.attached_to = self.tmux.attached_to(session.tmux_name());
        self.store.save(&session)?;
        self.log("revive", &format!("{} ({})", session.name, session.id))?;
        Ok(Revival::Revived(session))
    }

    /// Delete a record outright (`tx rm`); whether one was removed. A live tmux session keeps
    /// running.
    pub fn remove(&self, name_or_id: &str) -> Result<bool> {
        let Some(session) = self.resolve(name_or_id)? else {
            return Ok(false);
        };
        let removed = self.store.delete(&session.id)?;
        if removed {
            self.log("rm", &format!("{} ({})", session.name, session.id))?;
        }
        Ok(removed)
    }

    pub fn tag(&self, name_or_id: &str, tags: Vec<String>) -> Result<Session> {
        let mut session = self.require(name_or_id)?;
        session.tags = tags;
        session.attached_to = self.tmux.attached_to(session.tmux_name());
        self.store.save(&session)?;
        self.log(
            "tag",
            &format!("{} {}", session.name, session.tags.join(",")),
        )?;
        Ok(session)
    }

    /// Set (or with `None` clear) the explicit effort-group override.
    pub fn set_group(&self, name_or_id: &str, group: Option<String>) -> Result<Session> {
        let mut session = self.require(name_or_id)?;
        session.group = group;
        session.attached_to = self.tmux.attached_to(session.tmux_name());
        self.store.save(&session)?;
        let shown = session.group.as_deref().unwrap_or("(cleared)");
        self.log("group", &format!("{} {shown}", session.name))?;
        Ok(session)
    }

    /// Record which artifact a freshly spawned nvim view renders (`OtherSession.artifact_id`).
    pub fn bind_artifact(&self, session_id: &str, artifact_id: &str) -> Result<Session> {
        let mut session = self.require(session_id)?;
        let SessionKind::Other(other) = &mut session.kind else {
            return Err(ServiceError::NotAnArtifactView(session.name));
        };
        other.artifact_id = Some(artifact_id.to_owned());
        self.store.save(&session)?;
        self.log(
            "bind-artifact",
            &format!("{} → {artifact_id}", session.name),
        )?;
        Ok(session)
    }

    /// Rename the display name: a pure store write (tmux names the session by its id).
    pub fn rename(&self, name_or_id: &str, new_name: &str) -> Result<Session> {
        let mut session = self.require(name_or_id)?;
        if session.name == new_name {
            return Ok(session);
        }
        self.require_name_free(new_name)?;
        let previous = std::mem::replace(&mut session.name, new_name.to_owned());
        session.attached_to = self.tmux.attached_to(session.tmux_name());
        self.store.save(&session)?;
        self.log("rename", &format!("{previous} → {new_name}"))?;
        Ok(session)
    }

    // ----- state (hook entry) --------------------------------------------------------------

    /// Apply a hook-driven state change to one record. `false` when the id is not ours (D4) or
    /// the transition is a no-op (C3 terminal absorbing, C4 dirty-check).
    pub fn record_state(&self, session_id: &str, new_state: State) -> Result<bool> {
        let Some(mut session) = self.store.load(session_id)? else {
            return Ok(false);
        };
        if !session.transition_to(new_state) {
            return Ok(false);
        }
        let now = Number::from_f64(events::now());
        if new_state == State::Working {
            // Turn start arms the C5 clock; WORKING is llm-only.
            let name = session.name.clone();
            let llm = session.llm_mut().ok_or(ServiceError::NotAnLlm(name))?;
            llm.turn_started_at.clone_from(&now);
            llm.last_activity.clone_from(&now);
        }
        if new_state.is_terminal() {
            session.ended_at = now;
            session.attached_to.clear();
        } else {
            session.attached_to = self.tmux.attached_to(session.tmux_name());
        }
        self.store.save(&session)?;
        self.log("state", &format!("{} → {new_state}", session.name))?;
        Ok(true)
    }

    // ----- messaging -----------------------------------------------------------------------

    /// Peer-message another session (`<from-agent session="…">`); the sender is the tmux
    /// session that ran the command.
    pub fn send_message(&self, target: &str, body: &str) -> Result<()> {
        let sender = self.client_session_name()?;
        let record = self.deliver(target, &sender, body, TAG_AGENT)?;
        self.log("send-message", &format!("→ {}", record.name))
    }

    /// Message another session as the operator (`<from-user session="…">`); the sender is the
    /// originating session (`$TX_SESSION_ID`), not the hosting view.
    pub fn send_user_message(&self, target: &str, body: &str) -> Result<()> {
        let sender = self.origin_session_name()?;
        let record = self.deliver(target, &sender, body, TAG_USER)?;
        self.log("send-user-message", &format!("→ {}", record.name))
    }

    fn deliver(&self, target: &str, sender: &str, body: &str, tag: &str) -> Result<Session> {
        let record = self
            .resolve(target)?
            .filter(|record| self.tmux.has_session(record.tmux_name()))
            .ok_or_else(|| ServiceError::TargetNotFound(target.to_owned()))?;
        let pane = exact(record.tmux_name());
        self.tmux
            .send_keys(&pane, &build_envelope(sender, body, tag), true)?;
        std::thread::sleep(DELIVERY_PAUSE);
        self.tmux.send_keys(&pane, "Enter", false)?;
        Ok(record)
    }

    fn client_session_name(&self) -> Result<String> {
        let current = self
            .tmux
            .current_session_name()
            .ok_or(ServiceError::NotInsideTmux)?;
        Ok(match self.resolve(&current)? {
            Some(sender) => sender.name,
            None => current,
        })
    }

    fn origin_session_name(&self) -> Result<String> {
        let session_id = self.env.actor();
        if session_id.is_empty() {
            return Err(ServiceError::NotInsideTxSession);
        }
        Ok(match self.store.load(session_id)? {
            Some(record) => record.name,
            None => session_id.to_owned(),
        })
    }

    // ----- reads ---------------------------------------------------------------------------

    pub fn reconcile(&self) -> Result<Vec<Session>> {
        let engines = self.engines.borrow();
        let reconciler = Reconciler {
            store: &self.store,
            tmux: &self.tmux,
            events: &self.events,
            engines: &engines,
            home: &self.home,
            actor: self.env.actor(),
        };
        Ok(reconciler.reconcile()?)
    }

    /// Reconcile, then the live records with a fresh in-memory `attached_to` (one attachment
    /// sweep; not persisted).
    pub fn live_sessions(&self) -> Result<Vec<Session>> {
        self.reconcile()?;
        let attachment = self.tmux.attachment_map();
        let mut live: Vec<Session> = self
            .store
            .all()
            .into_iter()
            .filter(Session::is_alive)
            .collect();
        for session in &mut live {
            session.attached_to = attachment
                .iter()
                .find(|(inner, _)| inner == session.tmux_name())
                .map(|(_, locations)| locations.clone())
                .unwrap_or_default();
        }
        Ok(live)
    }

    pub fn get(&self, name_or_id: &str) -> Result<Option<Session>> {
        self.resolve(name_or_id)
    }

    /// The `<tx-command-prompt …/>` envelope for a pane, joined with the record kind / tags of
    /// the firing and the inner session. Empty when the pane is gone.
    pub fn focus_envelope(&self, pane_id: &str) -> Result<String> {
        let Some(mut attrs) = self.tmux.focus_attrs(pane_id) else {
            return Ok(String::new());
        };
        let outer = attr(&attrs, "session-name").map(str::to_owned);
        self.add_record_attrs(&mut attrs, outer.as_deref(), "session-kind", "session-tag")?;
        if attr(&attrs, "inner-remote").is_none_or(str::is_empty) {
            let inner = attr(&attrs, "inner-session-name").map(str::to_owned);
            self.add_record_attrs(
                &mut attrs,
                inner.as_deref(),
                "inner-session-kind",
                "inner-session-tag",
            )?;
        }
        Ok(format_envelope(&attrs))
    }

    fn add_record_attrs(
        &self,
        attrs: &mut Vec<(String, String)>,
        name: Option<&str>,
        kind_key: &str,
        tag_key: &str,
    ) -> Result<()> {
        let Some(name) = name.filter(|name| !name.is_empty()) else {
            return Ok(());
        };
        if self.tmux.has_session(name) && self.tmux.is_view(name) {
            env_set(attrs, kind_key, "view".into());
            return Ok(());
        }
        let Some(record) = self.resolve(name)? else {
            return Ok(());
        };
        env_set(attrs, kind_key, "process".into());
        env_set(attrs, tag_key, record.tags.join(","));
        Ok(())
    }

    // ----- resolution ----------------------------------------------------------------------

    /// By id (record file), then a live `@tx_id` on a session named `token`, then by name
    /// preferring the live record.
    fn resolve(&self, token: &str) -> Result<Option<Session>> {
        if let Some(session) = self.store.load(token)? {
            return Ok(Some(session));
        }
        if let Some(live_id) = self.tmux.get_tx_id(token)
            && let Some(session) = self.store.load(&live_id)?
        {
            return Ok(Some(session));
        }
        Ok(self.resolve_name(token))
    }

    /// Among records sharing `name`, the live one (in tmux) first, then the most recent; ties
    /// keep the first in store order (Python's `max`).
    fn resolve_name(&self, name: &str) -> Option<Session> {
        let matches: Vec<Session> = self
            .store
            .all()
            .into_iter()
            .filter(|session| session.name == name)
            .collect();
        let (live, dead): (Vec<Session>, Vec<Session>) = matches
            .into_iter()
            .partition(|session| self.tmux.has_session(session.tmux_name()));
        let pool = if live.is_empty() { dead } else { live };
        let mut best: Option<Session> = None;
        for session in pool {
            let created = created_key(&session);
            if best
                .as_ref()
                .is_none_or(|current| created > created_key(current))
            {
                best = Some(session);
            }
        }
        best
    }

    fn require(&self, token: &str) -> Result<Session> {
        self.resolve(token)?
            .ok_or_else(|| ServiceError::SessionNotFound(token.to_owned()))
    }

    fn adapter(&self, engine: Engine) -> Result<Rc<dyn EngineAdapter>> {
        self.engines
            .borrow()
            .get(engine)
            .ok_or(ServiceError::EngineNotRegistered(engine))
    }

    fn log(&self, type_: &str, msg: &str) -> Result<()> {
        self.events
            .append(type_, msg, self.env.actor())
            .map_err(ServiceError::io(self.events.path()))
    }
}

/// Q25 / Q39: names a worktree label can never take.
/// Whether a launch gives nvim a `--listen` socket (D11).
#[derive(Clone, Copy)]
enum Listen {
    None,
    NvimSocket,
}

fn nvim_socket_of(kind: &SessionKind) -> Option<&str> {
    match kind {
        SessionKind::Other(other) => other.nvim_socket.path(),
        SessionKind::Llm(_) => None,
    }
}

fn refuse_reserved_name(name: &str) -> Result<()> {
    if matches!(name, "" | "." | "..") {
        return Err(ServiceError::InvalidWorkerName(name.to_owned()));
    }
    Ok(())
}

/// Q27: an exact, pane-typed target for the session named `name`.
fn exact(name: &str) -> String {
    format!("={name}:")
}

/// `created_at or 0.0`.
fn created_key(session: &Session) -> f64 {
    session
        .created_at
        .as_ref()
        .and_then(Number::as_f64)
        .unwrap_or(0.0)
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    use serde_json::Value;

    use super::*;
    use crate::spawn::testing::registry;
    use crate::store::IgnoreSkips;
    use crate::tmux::TmuxEnv;

    /// A private tmux server (`-L`, `-f /dev/null`, socket under /private/tmp) behind a wrapper
    /// script, plus a scratch `$TX_IDE_HOME`; the server is killed on drop.
    struct Fixture {
        dir: tempfile::TempDir,
        wrapper: PathBuf,
        home: Rc<Home>,
    }

    impl Fixture {
        fn start() -> Option<Self> {
            Command::new("tmux").arg("-V").output().ok()?;
            let dir = tempfile::Builder::new()
                .prefix("txs")
                .tempdir_in("/private/tmp")
                .ok()?;
            let wrapper = dir.path().join("tmux");
            let script = format!(
                "#!/bin/sh\nTMUX_TMPDIR='{}' exec tmux -L s -f /dev/null \"$@\"\n",
                dir.path().display()
            );
            std::fs::write(&wrapper, script).ok()?;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).ok()?;
            let home = Home::new(dir.path().join("home"));
            home.ensure().ok()?;
            Some(Self {
                dir,
                wrapper,
                home: Rc::new(home),
            })
        }

        fn tmux(&self) -> Tmux {
            Tmux::new(&self.wrapper, TmuxEnv::default())
        }

        fn service(&self, vars: &[(&str, &str)]) -> SessionService {
            let env = Env {
                vars: vars
                    .iter()
                    .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                    .collect::<BTreeMap<_, _>>(),
                cwd: self.dir.path().to_owned(),
                exe: PathBuf::from("tx"),
            };
            SessionService::new(
                Rc::new(SessionStore::new(
                    self.home.sessions_dir(),
                    Rc::new(IgnoreSkips),
                )),
                Rc::new(self.tmux()),
                Rc::new(EventLog::new(self.home.log_path())),
                Rc::new(RefCell::new(registry())),
                Rc::clone(&self.home),
                Rc::new(env),
            )
        }

        /// `(actor, type, msg)` of every log line.
        fn log(&self) -> Vec<(String, String, String)> {
            let text = std::fs::read_to_string(self.home.log_path()).unwrap_or_default();
            text.lines()
                .map(|line| {
                    let value: Value = serde_json::from_str(line).unwrap();
                    let field = |key: &str| value[key].as_str().unwrap().to_owned();
                    (field("actor"), field("type"), field("msg"))
                })
                .collect()
        }

        fn run(&self, args: &[&str]) -> String {
            let out = Command::new(&self.wrapper).args(args).output().unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        }

        fn git_repo(&self) -> PathBuf {
            let repo = self.dir.path().join("repo");
            std::fs::create_dir(&repo).unwrap();
            let git = |args: &[&str]| {
                let status = Command::new("git")
                    .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                    .args(args)
                    .current_dir(&repo)
                    .output()
                    .unwrap();
                assert!(status.status.success(), "{status:?}");
            };
            git(&["init", "-q"]);
            std::fs::write(repo.join("README.md"), "hi\n").unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", "init"]);
            repo.canonicalize().unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = Command::new(&self.wrapper).arg("kill-server").output();
        }
    }

    macro_rules! fixture {
        () => {
            match Fixture::start() {
                Some(fixture) => fixture,
                None => {
                    eprintln!("tmux not available; skipping");
                    return;
                }
            }
        };
    }

    fn shell_spec(name: &str, cwd: &Path) -> SpawnSpec {
        SpawnSpec::for_process(
            name,
            vec!["t".into()],
            cwd.to_string_lossy(),
            "sleep 60",
            &registry(),
        )
    }

    #[test]
    fn spawn_kill_and_reconcile() {
        let fx = fixture!();
        let service = fx.service(&[("TX_SESSION_ID", "actor-1")]);
        let cwd = fx.dir.path();

        let session = service.spawn(shell_spec("sh1", cwd)).expect("spawn");
        assert_eq!(
            uuid::Uuid::parse_str(&session.id)
                .unwrap()
                .get_version_num(),
            4
        );
        assert_eq!((session.role(), session.state), (Role::Other, State::Alive));
        assert!(fx.tmux().has_session(&session.id));
        assert_eq!(
            fx.tmux().get_tx_id(&session.id).as_deref(),
            Some(session.id.as_str())
        );
        let saved = service.store().load(&session.id).unwrap().unwrap();
        assert_eq!(saved, session);
        assert_eq!(saved.to_value()["artifact_id"], Value::Null);
        let spawned_env = fx.run(&["show-environment", "-t", &session.id, "TX_SESSION_ID"]);
        assert_eq!(spawned_env.trim(), format!("TX_SESSION_ID={}", session.id));

        // A live record's name is taken; a live view's name is refused with its own message.
        let clash = service.spawn(shell_spec("sh1", cwd)).unwrap_err();
        assert_eq!(clash.to_string(), "session 'sh1' already exists");

        // Resolution by name and by id.
        assert_eq!(service.get("sh1").unwrap().unwrap().id, session.id);
        assert_eq!(service.get(&session.id).unwrap().unwrap().name, "sh1");
        assert!(service.get("nope").unwrap().is_none());

        // The session vanishes under us: reconcile marks it exited (clearing a stale
        // attachment), once.
        let mut stale = saved.clone();
        stale.attached_to = vec![crate::session::Location {
            host: "stale".into(),
            window_index: "9".into(),
            window_name: "x".into(),
            pane_id: "%99".into(),
            pane_index: "9".into(),
        }];
        service.store().save(&stale).unwrap();
        fx.tmux().kill_session(&session.id);
        let changed = service.reconcile().unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].state, State::Exited);
        assert!(changed[0].ended_at.is_some());
        assert!(
            service
                .get(&session.id)
                .unwrap()
                .unwrap()
                .attached_to
                .is_empty()
        );
        assert!(service.reconcile().unwrap().is_empty());

        // Name reuse after the record went terminal.
        let second = service.spawn(shell_spec("sh1", cwd)).expect("respawn");
        let killed = service.kill("sh1").unwrap().expect("record");
        assert_eq!(
            (killed.id.as_str(), killed.state),
            (second.id.as_str(), State::Exited)
        );
        assert!(!fx.tmux().has_session(&second.id));

        let missing = service.kill("nope").unwrap_err();
        assert_eq!(
            missing.to_string(),
            "session 'nope' not found (no live @tx_id, no store record)"
        );
        let actor = "actor-1".to_owned();
        let log: Vec<(String, String)> = fx
            .log()
            .into_iter()
            .inspect(|(who, _, _)| assert_eq!(who, &actor))
            .map(|(_, kind, msg)| (kind, msg))
            .collect();
        let cwd = cwd.display();
        assert_eq!(
            log,
            [
                ("spawn".into(), format!("sh1 [other] {cwd}")),
                ("reconcile".into(), "sh1 → exited (vanished)".into()),
                ("spawn".into(), format!("sh1 [other] {cwd}")),
                ("kill".into(), "sh1".into()),
            ]
        );
    }

    #[test]
    fn views_are_live_markers_only() {
        let fx = fixture!();
        let service = fx.service(&[]);
        let cwd = fx.dir.path().to_string_lossy().into_owned();
        let view = SpawnSpec::for_view("Views", &cwd, "sleep 60", &registry());
        assert_eq!(service.spawn_view(view.clone()).unwrap(), "Views");
        assert!(fx.tmux().is_view("Views"));
        assert_eq!(
            fx.run(&["show-options", "-vt", "=Views:", "status"]).trim(),
            "on"
        );
        assert!(
            std::fs::read_dir(fx.home.sessions_dir())
                .unwrap()
                .next()
                .is_none()
        );

        assert_eq!(
            service.spawn_view(view).unwrap_err().to_string(),
            "session 'Views' already exists"
        );
        assert_eq!(
            service
                .spawn(shell_spec("Views", fx.dir.path()))
                .unwrap_err()
                .to_string(),
            "'Views' is a live view session — pick another name"
        );
        assert!(service.kill("Views").unwrap().is_none());
        assert!(!fx.tmux().has_session("Views"));
        let kinds: Vec<String> = fx
            .log()
            .into_iter()
            .map(|(_, kind, msg)| format!("{kind} {msg}"))
            .collect();
        assert_eq!(
            kinds,
            [format!("spawn-view Views {cwd}"), "kill Views".into()]
        );
    }

    #[test]
    fn stuck_working_demotion_and_threshold() {
        let fx = fixture!();
        let service = fx.service(&[]);
        let mut session = service.spawn(shell_spec("w", fx.dir.path())).unwrap();
        // Turn the record into a WORKING llm whose turn started 20 minutes ago.
        session.kind = SessionKind::Llm(LlmSession {
            engine: Engine::Claude,
            chats: Vec::new(),
            last_activity: None,
            turn_started_at: Number::from_f64(events::now() - 1200.0),
        });
        session.state = State::Working;
        service.store().save(&session).unwrap();

        std::fs::write(
            fx.home.config_path(),
            r#"{"stuck_working_threshold_seconds": 3600}"#,
        )
        .unwrap();
        assert!(service.reconcile().unwrap().is_empty());

        std::fs::write(fx.home.config_path(), r#"{"other": 1}"#).unwrap();
        let changed = service.reconcile().unwrap();
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].state, State::Idle);
        assert!(changed[0].ended_at.is_none());
        assert_eq!(fx.log().last().unwrap().2, "w working → idle (stuck)");

        // A malformed config fails every pass, candidate or not (reconcile.py reads it first).
        std::fs::write(fx.home.config_path(), "{").unwrap();
        assert!(matches!(
            service.reconcile().unwrap_err(),
            ServiceError::Reconcile(ReconcileError::Config(_))
        ));
    }

    #[test]
    fn launch_scripts_are_swept_after_the_grace_period() {
        let fx = fixture!();
        let service = fx.service(&[]);
        let live = service.spawn(shell_spec("s", fx.dir.path())).unwrap();
        let launch = fx.home.launch_dir();
        let age = |name: &str, seconds: u64| {
            let path = launch.join(name);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            let past = std::time::SystemTime::now() - Duration::from_secs(seconds);
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(past)
                .unwrap();
        };
        age("dead.sh", 120);
        age("young.sh", 5);
        age(&format!("{}.sh", live.id), 3600);
        age("dead.txt", 3600);
        service.reconcile().unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&launch)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        let mut expected = vec![
            "dead.txt".to_owned(),
            format!("{}.sh", live.id),
            "young.sh".into(),
        ];
        expected.sort();
        assert_eq!(left, expected);
    }

    #[test]
    fn revive_only_an_exited_record_whose_session_lives() {
        let fx = fixture!();
        let service = fx.service(&[]);
        let session = service.spawn(shell_spec("w", fx.dir.path())).unwrap();
        let mut exited = session.clone();
        exited.state = State::Exited;
        exited.ended_at = Number::from_f64(1234.5);
        service.store().save(&exited).unwrap();
        // Reconcile never revives (Q32).
        assert!(service.reconcile().unwrap().is_empty());
        assert_eq!(service.get("w").unwrap().unwrap().state, State::Exited);

        let Revival::Revived(revived) = service.revive("w").unwrap() else {
            panic!("expected a revival");
        };
        assert_eq!(
            (revived.state, revived.ended_at.clone()),
            (State::Alive, None)
        );
        assert_eq!(fx.log().last().unwrap().1, "revive");
        assert_eq!(fx.log().last().unwrap().2, format!("w ({})", session.id));
        assert!(matches!(
            service.revive("w").unwrap(),
            Revival::AlreadyLive(_)
        ));

        let mut archived = revived.clone();
        archived.state = State::Archived;
        service.store().save(&archived).unwrap();
        assert_eq!(
            service.revive("w").unwrap_err().to_string(),
            "session 'w' is archived — an archived record is never revived"
        );

        let mut gone = revived;
        gone.state = State::Exited;
        service.store().save(&gone).unwrap();
        fx.tmux().kill_session(&gone.id);
        assert_eq!(
            service.revive("w").unwrap_err().to_string(),
            "session 'w' has no live tmux session to revive"
        );
        assert!(matches!(
            service.revive("nope").unwrap_err(),
            ServiceError::SessionNotFound(_)
        ));
    }

    #[test]
    fn record_mutations_log_once_each() {
        let fx = fixture!();
        let service = fx.service(&[]);
        let session = service.spawn(shell_spec("a", fx.dir.path())).unwrap();
        service.tag("a", vec!["x".into(), "y".into()]).unwrap();
        service.set_group("a", Some("g".into())).unwrap();
        service.set_group("a", None).unwrap();
        service.rename("a", "b").unwrap();
        service.rename("b", "b").unwrap();
        service.bind_artifact(&session.id, "art-1").unwrap();
        assert!(!service.record_state("not-ours", State::Idle).unwrap());
        assert!(service.record_state(&session.id, State::Exited).unwrap());
        assert!(!service.record_state(&session.id, State::Alive).unwrap());
        service.archive("b").unwrap();
        assert!(service.remove("b").unwrap());
        assert!(!service.remove("b").unwrap());
        let log: Vec<String> = fx
            .log()
            .into_iter()
            .skip(1)
            .map(|(_, kind, msg)| format!("{kind}: {msg}"))
            .collect();
        assert_eq!(
            log,
            [
                "tag: a x,y".to_owned(),
                "group: a g".into(),
                "group: a (cleared)".into(),
                "rename: a → b".into(),
                "bind-artifact: b → art-1".into(),
                "state: b → exited".into(),
                "archive: b".into(),
                format!("rm: b ({})", session.id),
            ]
        );
    }

    #[test]
    fn long_commands_go_through_a_private_launch_script() {
        let fx = fixture!();
        let service = fx.service(&[]);
        let short = "echo hi";
        assert_eq!(service.transportable_command("id1", short).unwrap(), short);
        let long = format!("echo {}", "x".repeat(MAX_COMMAND_BYTES));
        let launched = service.transportable_command("id1", &long).unwrap();
        let script = fx.home.launch_dir().join("id1.sh");
        assert_eq!(launched, format!("/bin/sh {}", script.display()));
        assert_eq!(
            std::fs::read_to_string(&script).unwrap(),
            format!("{long}\n")
        );
        let mode = std::fs::metadata(&script).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & !0o700, 0, "{mode:o}");
    }

    #[test]
    fn user_messages_are_typed_literally_then_entered() {
        let fx = fixture!();
        let service = fx.service(&[("TX_SESSION_ID", "deadbeef")]);
        let mut spec = shell_spec("bob", fx.dir.path());
        spec.cmd = "cat".into();
        let target = service.spawn(spec).unwrap();
        service.send_user_message("bob", "Enter").unwrap();
        let envelope = "<from-user session=\"deadbeef\">Enter</from-user>";
        let mut capture = String::new();
        for _ in 0..50 {
            capture = fx.run(&["capture-pane", "-p", "-t", &target.id]);
            if capture.matches(envelope).count() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(capture.matches(envelope).count(), 2, "{capture}");
        assert_eq!(fx.log().last().unwrap().2, "→ bob");

        assert_eq!(
            service
                .send_user_message("nobody", "x")
                .unwrap_err()
                .to_string(),
            "target session 'nobody' does not exist"
        );
        let outside = fx.service(&[("TX_SESSION_ID", "")]);
        assert!(matches!(
            outside.send_user_message("bob", "x").unwrap_err(),
            ServiceError::NotInsideTxSession
        ));
        assert!(matches!(
            service.send_message("bob", "x").unwrap_err(),
            ServiceError::NotInsideTmux
        ));
    }

    fn worker_spec(name: &str, cwd: &Path) -> SpawnSpec {
        let mut spec = shell_spec(name, cwd);
        spec.role = Role::Llm;
        spec.engine = Some(Engine::Claude);
        spec
    }

    #[test]
    fn workers_get_a_fresh_worktree() {
        let fx = fixture!();
        let service = fx.service(&[]);
        let repo = fx.git_repo();

        // No hooks installed: refused, and the worktree created for it is removed again.
        let refused = service.spawn(worker_spec("w", &repo)).unwrap_err();
        assert_eq!(
            refused.to_string(),
            format!(
                "claude hooks are not installed in {} — the worker's chat id would never be \
                 captured and the session could never be resumed. Install them: \
                 setup/engines/install.sh install --engine claude",
                fx.home.root().display()
            )
        );
        let leftover: Vec<_> = walk(&fx.home.worktrees_dir());
        assert!(
            leftover
                .iter()
                .all(|path| path.is_dir() && walk(path).is_empty()),
            "{leftover:?}"
        );

        let shim = fx.home.engine_capture_shim("claude");
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(&shim, "").unwrap();

        let first = service.spawn(worker_spec("w", &repo)).unwrap();
        assert_eq!(first.name, "w");
        assert!(
            first
                .cwd
                .starts_with(&fx.home.worktrees_dir().to_string_lossy().into_owned())
        );
        assert!(first.cwd.ends_with("--w"), "{}", first.cwd);
        assert_eq!(first.env_var(REQUIRE_WORKTREE_ENV), Some("1"));
        assert_eq!(first.env_var(READ_ONLY_ENV), None);
        let chat = &first.chats()[0];
        assert_eq!((chat.id.as_deref(), chat.role.as_str()), (None, "original"));
        assert_eq!(chat.origin.session_id, first.id);
        assert_eq!(first.llm().unwrap().turn_started_at, None);

        let second = service.spawn(worker_spec("w", &repo)).unwrap();
        assert_eq!(second.name, "w-2");

        // Q25: reserved names are refused before anything is created.
        for name in ["", ".", ".."] {
            let error = service.spawn(worker_spec(name, &repo)).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "invalid worker name '{name}' — a worker name cannot be empty, '.' or '..'"
                )
            );
            assert!(service.next_worker_name(&repo, name).is_err());
        }
        assert_eq!(service.next_worker_name(&repo, "w").unwrap(), "w-3");

        // Read-only needs the engine's own read-only controls.
        let mut read_only = worker_spec("ro", &repo);
        read_only.read_only = true;
        assert_eq!(
            service.spawn(read_only).unwrap_err().to_string(),
            "claude command does not enforce the requested read-only mode"
        );
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
            .unwrap_or_default()
    }
}
