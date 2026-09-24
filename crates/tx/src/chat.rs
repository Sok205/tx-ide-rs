//! Chat operations (chat.py): fork / handover / rollover, the idempotent `_chat-op-finish` and
//! its detached `_chat-op-watch` backstop.
//!
//! - **fork**: a NEW tx session resumed onto the source chat with `--fork-session` (full
//!   history); a pending `ChatRef{role:fork}` the capture hook fills.
//! - **handover**: a temporary distiller writes a brief, then triggers `_chat-op-finish`, which
//!   spawns a fresh worker seeded to read it (`--self-catch-up` skips the distiller).
//! - **rollover**: the SAME session's pane is respawned onto a fresh chat seeded with a note.
//!
//! The finish is single-shot through an atomic `claim/` mkdir. Detached children re-exec this
//! binary (`Env::exe`) in a new session (`setsid`), so they survive the caller's pane being
//! killed. FIX quirks: Q9 (a missing / malformed op-spec is one error line, never a crash), Q12
//! (watcher timings from `TX_CHAT_OP_POLL_S` / `TX_CHAT_OP_GRACE_S` / `TX_CHAT_OP_TIMEOUT_S`).

use std::cell::RefCell;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use serde_json::{Map, Number, Value};

use crate::app::Env;
use crate::engines::adapter::env_set;
use crate::engines::claude::{bundle_dir, bundle_transcript_path};
use crate::engines::{EngineAdapter, EngineError, EngineRegistry};
use crate::events::{self, EventLog};
use crate::history::{History, HistoryError, IngestMode};
use crate::pyjson;
use crate::service::{ServiceError, SessionService};
use crate::session::{ChatRef, Engine, Origin, Session};
use crate::shlex::{shlex_join, shlex_quote};
use crate::spawn::SpawnSpec;
use crate::storage::Home;
use crate::store::StoreError;
use crate::tmux::TmuxError;

/// Tag of the throwaway distiller (plus the op kind), so the in-flight helper shows in `tx ls`.
pub const DISTILLER_TAG: &str = "temporary";

pub const POLL_ENV: &str = "TX_CHAT_OP_POLL_S";
pub const GRACE_ENV: &str = "TX_CHAT_OP_GRACE_S";
pub const TIMEOUT_ENV: &str = "TX_CHAT_OP_TIMEOUT_S";

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error("{op}: source session '{token}' not found")]
    SourceNotFound { op: &'static str, token: String },
    #[error("{op}: '{name}' has no {missing}")]
    NoChat {
        op: &'static str,
        name: String,
        missing: &'static str,
    },
    #[error("rollover: run inside a tmux session or pass <session>")]
    NotInsideTmux,
    #[error("rollover: session '{0}' not found")]
    RolloverTargetNotFound(String),
    #[error("rollover: could not resolve a pane for '{0}'")]
    NoPane(String),
    #[error("_chat-op-finish: source record '{0}' not found")]
    FinishSourceGone(String),
    #[error("_chat-op-finish: record '{0}' not found")]
    FinishRecordGone(String),
    /// Q9 FIX: the reference crashed with a traceback here.
    #[error("chat-op '{op_id}' has no readable spec at {}: {source}", path.display())]
    SpecUnreadable {
        op_id: String,
        path: PathBuf,
        source: io::Error,
    },
    #[error("chat-op '{op_id}' has an invalid spec: {reason}")]
    SpecInvalid { op_id: String, reason: String },
    #[error("no engine adapter is registered for {0}")]
    EngineNotRegistered(Engine),
    #[error("session '{0}' vanished from the store")]
    RecordVanished(String),
    #[error("session '{0}' is not an llm session and hosts no chats")]
    NotAnLlm(String),
    #[error("could not start `tx {verb}` detached: {source}")]
    Detach { verb: String, source: io::Error },
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Tmux(#[from] TmuxError),
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> ChatError + '_ {
    move |source| ChatError::Io {
        path: path.to_owned(),
        source,
    }
}

type Result<T, E = ChatError> = std::result::Result<T, E>;

/// The chat an op targets: the last `ChatRef` with a real id, preferring one still open.
pub fn active_chat(session: &Session) -> Option<&ChatRef> {
    let candidates: Vec<&ChatRef> = session
        .chats()
        .iter()
        .filter(|chat| chat.id.is_some())
        .collect();
    candidates
        .iter()
        .rev()
        .find(|chat| chat.ended_at.is_none())
        .or_else(|| candidates.last())
        .copied()
}

/// `_slug`: a filesystem-safe slug for `handover-<slug>.md`.
pub fn slug(text: &str) -> String {
    let cleaned: String = text
        .to_lowercase()
        .chars()
        .map(|character| {
            if character.is_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    let joined = cleaned
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug: String = joined.chars().take(48).collect();
    if slug.is_empty() {
        "task".to_owned()
    } else {
        slug
    }
}

/// `_env_prefix`: `env K=V … ` for a respawn command (respawn-pane takes no `-e`).
fn env_prefix(env: &[(String, String)]) -> String {
    if env.is_empty() {
        return String::new();
    }
    let pairs: Vec<String> = env
        .iter()
        .map(|(key, value)| format!("{}={}", shlex_quote(key), shlex_quote(value)))
        .collect();
    format!("env {} ", pairs.join(" "))
}

// ----- the op-spec -------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Handover,
    Rollover,
}

impl OpKind {
    fn as_str(self) -> &'static str {
        match self {
            OpKind::Handover => "handover",
            OpKind::Rollover => "rollover",
        }
    }
}

/// `$TX_IDE_HOME/chat-ops/<op-id>/spec.json`: everything the finish needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatOpSpec {
    pub op_id: String,
    /// Kept as written: the reference treats anything but `handover` as a rollover.
    pub kind: String,
    pub source_txid: String,
    pub source_chat: String,
    pub cwd: String,
    /// The brief / note the distiller writes; `""` = self-catch-up.
    pub artifact_path: String,
    pub self_catch_up: bool,
    pub read_only: bool,
    pub worker_name: String,
    pub pane: String,
    pub distiller_name: String,
}

impl ChatOpSpec {
    fn new(op_id: String, kind: OpKind, source_txid: &str, source_chat: &str) -> Self {
        Self {
            op_id,
            kind: kind.as_str().to_owned(),
            source_txid: source_txid.to_owned(),
            source_chat: source_chat.to_owned(),
            cwd: String::new(),
            artifact_path: String::new(),
            self_catch_up: false,
            read_only: false,
            worker_name: String::new(),
            pane: String::new(),
            distiller_name: String::new(),
        }
    }

    pub fn dir(&self, home: &Home) -> PathBuf {
        home.chat_ops_dir().join(&self.op_id)
    }

    fn is_handover(&self) -> bool {
        self.kind == "handover"
    }

    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "op_id": self.op_id,
            "kind": self.kind,
            "source_txid": self.source_txid,
            "source_chat": self.source_chat,
            "cwd": self.cwd,
            "artifact_path": self.artifact_path,
            "self_catch_up": self.self_catch_up,
            "read_only": self.read_only,
            "worker_name": self.worker_name,
            "pane": self.pane,
            "distiller_name": self.distiller_name,
        })
    }

    pub fn save(&self, home: &Home) -> Result<()> {
        let directory = self.dir(home);
        std::fs::create_dir_all(&directory).map_err(io_err(&directory))?;
        let path = directory.join("spec.json");
        std::fs::write(&path, pyjson::dumps(&self.to_value())).map_err(io_err(&path))
    }

    pub fn load(home: &Home, op_id: &str) -> Result<Self> {
        let path = home.chat_ops_dir().join(op_id).join("spec.json");
        let text = std::fs::read_to_string(&path).map_err(|source| ChatError::SpecUnreadable {
            op_id: op_id.to_owned(),
            path: path.clone(),
            source,
        })?;
        Self::parse(op_id, &text)
    }

    /// The dataclass constructor: six required string fields, the rest defaulted.
    pub fn parse(op_id: &str, text: &str) -> Result<Self> {
        let invalid = |reason: String| ChatError::SpecInvalid {
            op_id: op_id.to_owned(),
            reason,
        };
        let value: Value =
            serde_json::from_str(text).map_err(|error| invalid(format!("not JSON ({error})")))?;
        let Value::Object(data) = value else {
            return Err(invalid("not a JSON object".to_owned()));
        };
        let text_field = |key: &str, required: bool| -> Result<String> {
            match data.get(key) {
                Some(Value::String(text)) => Ok(text.clone()),
                Some(_) => Err(invalid(format!("'{key}' is not a string"))),
                None if required => Err(invalid(format!("missing key '{key}'"))),
                None => Ok(String::new()),
            }
        };
        let flag = |key: &str| -> Result<bool> { flag_of(&data, key).map_err(invalid) };
        Ok(Self {
            op_id: text_field("op_id", true)?,
            kind: text_field("kind", true)?,
            source_txid: text_field("source_txid", true)?,
            source_chat: text_field("source_chat", true)?,
            cwd: text_field("cwd", true)?,
            artifact_path: text_field("artifact_path", true)?,
            self_catch_up: flag("self_catch_up")?,
            read_only: flag("read_only")?,
            worker_name: text_field("worker_name", false)?,
            pane: text_field("pane", false)?,
            distiller_name: text_field("distiller_name", false)?,
        })
    }
}

/// A defaulted bool field; Python truthiness for the scalar shapes a spec could carry.
fn flag_of(data: &Map<String, Value>, key: &str) -> Result<bool, String> {
    match data.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(Value::Number(number)) => Ok(number.as_f64().is_some_and(|value| value != 0.0)),
        Some(Value::String(text)) => Ok(!text.is_empty()),
        Some(_) => Err(format!("'{key}' is not a boolean")),
    }
}

// ----- watcher timings (Q12 FIX) -----------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WatchTimings {
    pub poll: Duration,
    pub grace: Duration,
    pub timeout: Duration,
}

impl Default for WatchTimings {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(2),
            grace: Duration::from_secs(20),
            timeout: Duration::from_secs(600),
        }
    }
}

impl WatchTimings {
    /// Seconds (float) from env; an unset, malformed or out-of-range value keeps its default
    /// (the poll must be positive, grace / timeout may be zero).
    pub fn from_env(env: &Env) -> Self {
        let defaults = Self::default();
        let seconds = |name: &str, fallback: Duration, allow_zero: bool| {
            env.var(name)
                .and_then(|raw| raw.trim().parse::<f64>().ok())
                .filter(|value| *value > 0.0 || allow_zero && *value == 0.0)
                .and_then(|value| Duration::try_from_secs_f64(value).ok())
                .unwrap_or(fallback)
        };
        Self {
            poll: seconds(POLL_ENV, defaults.poll, false),
            grace: seconds(GRACE_ENV, defaults.grace, true),
            timeout: seconds(TIMEOUT_ENV, defaults.timeout, true),
        }
    }

    /// `int(GRACE / POLL)`: the post-finish wait for a finisher still holding the claim.
    fn settle_polls(&self) -> u64 {
        // Truncation is the reference's `int()`; both operands are finite and non-negative.
        (self.grace.as_secs_f64() / self.poll.as_secs_f64()) as u64
    }
}

// ----- the operations ----------------------------------------------------------------------

/// fork / handover / rollover over the `SessionService` surface.
pub struct ChatOps<'a> {
    pub service: &'a SessionService,
    pub engines: &'a RefCell<EngineRegistry>,
    pub home: &'a Home,
    pub events: &'a EventLog,
    pub env: &'a Env,
}

impl ChatOps<'_> {
    fn adapter(&self, engine: Engine) -> Result<Rc<dyn EngineAdapter>> {
        self.engines
            .borrow()
            .get(engine)
            .ok_or(ChatError::EngineNotRegistered(engine))
    }

    fn log(&self, type_: &str, msg: &str) -> Result<()> {
        self.events
            .append(type_, msg, self.env.actor())
            .map_err(io_err(self.events.path()))
    }

    fn process_spec(&self, name: &str, tags: Vec<String>, cwd: &str, cmd: String) -> SpawnSpec {
        SpawnSpec::for_process(name, tags, cwd, cmd, &self.engines.borrow())
    }

    /// A blocking mirror of every captured chat of `session_id`.
    fn ingest(&self, session_id: &str) -> Result<()> {
        let engines = self.engines.borrow();
        History::new(self.home, &engines).ingest_session(
            self.service.store(),
            session_id,
            IngestMode::Wait,
        )?;
        Ok(())
    }

    fn load(&self, session_id: &str) -> Result<Session> {
        self.service
            .store()
            .load(session_id)?
            .ok_or_else(|| ChatError::RecordVanished(session_id.to_owned()))
    }

    // ----- fork ----------------------------------------------------------------------------

    /// Branch the source's active chat into a NEW session carrying its full history. `parent`
    /// is the source; the group is only the explicit override.
    pub fn fork(
        &self,
        source: &str,
        new_name: Option<&str>,
        read_only: bool,
        group: Option<String>,
    ) -> Result<Session> {
        let source_session =
            self.service
                .get(source)?
                .ok_or_else(|| ChatError::SourceNotFound {
                    op: "fork",
                    token: source.to_owned(),
                })?;
        let source_chat =
            active_chat(&source_session)
                .cloned()
                .ok_or_else(|| ChatError::NoChat {
                    op: "fork",
                    name: source_session.name.clone(),
                    missing: "chat to fork",
                })?;
        let chat_id = source_chat.id.clone().unwrap_or_default();
        let cwd = source_chat.cwd.clone();
        let base_name = match new_name.filter(|name| !name.is_empty()) {
            Some(name) => name.to_owned(),
            None => format!("{}-fork", source_session.name),
        };
        let name = self.service.next_worker_name(Path::new(&cwd), &base_name)?;
        let engine = llm_engine(&source_session)?;
        let adapter = self.adapter(engine)?;
        let cmd =
            shlex_join(&adapter.fork_command(&source_session.initial_cmd, &chat_id, read_only)?);
        let mut spec = self.process_spec(&name, source_session.tags.clone(), &cwd, cmd);
        spec.env = source_session.spawn_env.clone();
        spec.records_own_chat = true;
        spec.engine = Some(engine);
        spec.read_only = read_only;
        spec.group = group;
        spec.parent = Some(source_session.id.clone());
        let new_session = self.service.spawn_worker_with(spec, false, |prepared| {
            adapter
                .prepare_chat_for_cwd(&chat_id, &source_chat.cwd, &prepared.cwd)
                .map_err(Into::into)
        })?;
        self.record_fork_chat(
            &new_session.id,
            &new_session.cwd,
            &source_session.id,
            &chat_id,
        )?;
        self.log(
            "fork",
            &format!(
                "{} → {} (chat pending)",
                source_session.name, new_session.name
            ),
        )?;
        self.load(&new_session.id)
    }

    /// The fork's pending `ChatRef` (idempotent: one per source chat).
    fn record_fork_chat(
        &self,
        new_txid: &str,
        cwd: &str,
        source_txid: &str,
        source_chat: &str,
    ) -> Result<()> {
        let mut session = self.load(new_txid)?;
        let name = session.name.clone();
        let llm = session.llm_mut().ok_or(ChatError::NotAnLlm(name))?;
        if llm
            .chats
            .iter()
            .any(|chat| chat.role == "fork" && chat.origin.chat_id.as_deref() == Some(source_chat))
        {
            return Ok(());
        }
        let engine = llm.engine;
        llm.chats.push(pending_chat(
            "fork",
            cwd,
            source_txid,
            source_chat,
            Number::from_f64(events::now()),
            engine,
        ));
        self.service.store().save(&session)?;
        Ok(())
    }

    // ----- handover ------------------------------------------------------------------------

    /// Distill the source chat into a brief for a NEW worker; returns the worker's name.
    pub fn handover(
        &self,
        source: &str,
        task: &str,
        new_name: Option<&str>,
        self_catch_up: bool,
        read_only: bool,
    ) -> Result<String> {
        let source_session =
            self.service
                .get(source)?
                .ok_or_else(|| ChatError::SourceNotFound {
                    op: "handover",
                    token: source.to_owned(),
                })?;
        let source_chat =
            active_chat(&source_session)
                .cloned()
                .ok_or_else(|| ChatError::NoChat {
                    op: "handover",
                    name: source_session.name.clone(),
                    missing: "chat to hand over",
                })?;
        let chat_id = source_chat.id.clone().unwrap_or_default();
        let engine = llm_engine(&source_session)?;

        // Mirror the bundle first: the distiller / worker reads the durable copy.
        self.ingest(&source_session.id)?;

        let base_name = match new_name.filter(|name| !name.is_empty()) {
            Some(name) => name.to_owned(),
            None => format!("{}-handover", source_session.name),
        };
        let worker_name = self
            .service
            .next_worker_name(Path::new(&source_chat.cwd), &base_name)?;
        let brief_path = self
            .home
            .history_dir()
            .join(&source_session.id)
            .join(format!("handover-{}.md", slug(task)));

        let mut spec = ChatOpSpec::new(new_op_id(), OpKind::Handover, &source_session.id, &chat_id);
        spec.cwd.clone_from(&source_chat.cwd);
        if !self_catch_up {
            spec.artifact_path = brief_path.to_string_lossy().into_owned();
        }
        spec.self_catch_up = self_catch_up;
        spec.read_only = read_only;
        spec.worker_name.clone_from(&worker_name);

        if self_catch_up {
            spec.save(self.home)?;
            self.chat_op_finish(&spec.op_id)?;
            self.log(
                "handover",
                &format!("{} → {worker_name} (self-catch-up)", source_session.name),
            )?;
            return Ok(worker_name);
        }

        spec.distiller_name = self.service.next_worker_name(
            Path::new(&source_chat.cwd),
            &format!("{worker_name}-distill"),
        )?;
        spec.save(self.home)?;
        let seed = format!(
            "You are a tx-ide handover distiller (a temporary helper). Read the predecessor chat \
             bundle at {} . Distill a focused, self-contained brief for this task and write it to \
             {} : «{task}». Capture only what the new worker needs to start — relevant context, \
             current state, constraints, and key file paths — not the whole history. When the \
             brief file is saved, run exactly this command and nothing else: {} _chat-op-finish {}",
            bundle_transcript_path(self.home, &source_session.id, &chat_id).display(),
            brief_path.display(),
            self.tx_invocation(),
            shlex_quote(&spec.op_id),
        );
        let distiller = self.spawn_distiller(
            &spec.distiller_name,
            OpKind::Handover,
            &source_chat.cwd,
            &seed,
            engine,
        )?;
        if distiller.name != spec.distiller_name {
            spec.distiller_name = distiller.name;
            spec.save(self.home)?;
        }
        self.detach("_chat-op-watch", &spec.op_id)?;
        self.log(
            "handover",
            &format!("{} → {worker_name} (distilling)", source_session.name),
        )?;
        Ok(worker_name)
    }

    // ----- rollover ------------------------------------------------------------------------

    /// Rotate the SAME session onto a fresh chat in the same pane.
    pub fn rollover(&self, session: Option<&str>, self_catch_up: bool) -> Result<()> {
        let target = match session.filter(|name| !name.is_empty()) {
            Some(name) => name.to_owned(),
            None => self
                .service
                .tmux()
                .current_session_name()
                .ok_or(ChatError::NotInsideTmux)?,
        };
        let record = self
            .service
            .get(&target)?
            .ok_or_else(|| ChatError::RolloverTargetNotFound(target.clone()))?;
        let current_chat = active_chat(&record)
            .cloned()
            .ok_or_else(|| ChatError::NoChat {
                op: "rollover",
                name: record.name.clone(),
                missing: "active chat to roll over",
            })?;
        let chat_id = current_chat.id.clone().unwrap_or_default();

        let pane = self.resolve_pane(&record)?;
        let note_path = self.next_rollover_note(&record.id);

        let mut spec = ChatOpSpec::new(new_op_id(), OpKind::Rollover, &record.id, &chat_id);
        spec.cwd.clone_from(&current_chat.cwd);
        if !self_catch_up {
            spec.artifact_path = note_path.to_string_lossy().into_owned();
        }
        spec.self_catch_up = self_catch_up;
        spec.pane = pane;

        if self_catch_up {
            spec.save(self.home)?;
            // The caller may BE the pane being respawned: the finish must outlive it.
            self.detach("_chat-op-finish", &spec.op_id)?;
            return self.log("rollover", &format!("{} (self-catch-up)", record.name));
        }

        self.ingest(&record.id)?;
        spec.distiller_name = self.service.next_worker_name(
            Path::new(&current_chat.cwd),
            &format!("{}-rollover-distill", record.name),
        )?;
        spec.save(self.home)?;
        let seed = format!(
            "You are a tx-ide rollover summariser (a temporary helper). Read the predecessor chat \
             bundle at {} . Write a concise hand-off note — the current task, what is done, the \
             immediate next steps, and key files and decisions — to {} . When the note file is \
             saved, run exactly this command and nothing else: {} _chat-op-finish {}",
            bundle_transcript_path(self.home, &record.id, &chat_id).display(),
            note_path.display(),
            self.tx_invocation(),
            shlex_quote(&spec.op_id),
        );
        let engine = llm_engine(&record)?;
        let distiller = self.spawn_distiller(
            &spec.distiller_name,
            OpKind::Rollover,
            &current_chat.cwd,
            &seed,
            engine,
        )?;
        if distiller.name != spec.distiller_name {
            spec.distiller_name = distiller.name;
            spec.save(self.home)?;
        }
        self.detach("_chat-op-watch", &spec.op_id)?;
        self.log("rollover", &format!("{} (distilling)", record.name))
    }

    // ----- the idempotent finish + its watchdog --------------------------------------------

    /// Complete a handover / rollover from its spec. Single execution via the `claim/` mkdir;
    /// a loser (or an already finished op) is a no-op.
    pub fn chat_op_finish(&self, op_id: &str) -> Result<()> {
        let spec = ChatOpSpec::load(self.home, op_id)?;
        let directory = spec.dir(self.home);
        let done = directory.join("done");
        if done.exists() {
            return Ok(());
        }
        let claim = directory.join("claim");
        match std::fs::create_dir(&claim) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(error) => return Err(io_err(&claim)(error)),
        }
        if spec.is_handover() {
            self.finish_handover(&spec)?;
        } else {
            self.finish_rollover(&spec)?;
        }
        std::fs::write(&done, "").map_err(io_err(&done))?;
        // Self-catch-up has no watchdog to tear the op down.
        if spec.distiller_name.is_empty() {
            self.cleanup_op(&spec);
        }
        Ok(())
    }

    /// Spawn the fresh handover worker seeded to read only the brief (or the bundle pointer).
    fn finish_handover(&self, spec: &ChatOpSpec) -> Result<()> {
        let source = self
            .service
            .store()
            .load(&spec.source_txid)?
            .ok_or_else(|| ChatError::FinishSourceGone(spec.source_txid.clone()))?;
        let bundle = bundle_dir(self.home, &spec.source_txid, &spec.source_chat);
        let seed = if artifact_missing(spec) {
            format!(
                "You are taking over work via tx handover. There is no pre-written brief — read \
                 the predecessor bundle at {}/ (transcript.jsonl + subagents/ + tool-results/), \
                 write yourself a short brief of the task and its state, then begin.",
                bundle.display()
            )
        } else {
            format!(
                "Your task brief is at {} — read it and begin. Fuller predecessor history, only \
                 if the brief is insufficient: {}/ .",
                spec.artifact_path,
                bundle.display()
            )
        };
        let engine = llm_engine(&source)?;
        let launch = shlex_join(&self.adapter(engine)?.seed_command(
            &source.initial_cmd,
            &seed,
            spec.read_only,
        )?);
        let mut worker_spec =
            self.process_spec(&spec.worker_name, source.tags.clone(), &spec.cwd, launch);
        worker_spec.env = source.spawn_env.clone();
        worker_spec.records_own_chat = true;
        worker_spec.engine = Some(engine);
        worker_spec.read_only = spec.read_only;
        // parent = SOURCE, not whoever runs this finish.
        worker_spec.parent = Some(spec.source_txid.clone());
        let worker = self.service.spawn_worker(worker_spec, false)?;
        self.record_seeded_chat(
            &worker.id,
            &worker.cwd,
            "handover",
            &spec.source_txid,
            &spec.source_chat,
            None,
        )?;
        self.log(
            "handover-finish",
            &format!("{} (chat pending)", worker.name),
        )
    }

    /// Respawn the pane onto a fresh chat in place, after a final re-ingest of the source.
    fn finish_rollover(&self, spec: &ChatOpSpec) -> Result<()> {
        let record = self
            .service
            .store()
            .load(&spec.source_txid)?
            .ok_or_else(|| ChatError::FinishRecordGone(spec.source_txid.clone()))?;
        // Capture the predecessor's final state right before the hard kill.
        self.ingest(&spec.source_txid)?;
        let catch_up = bundle_dir(self.home, &spec.source_txid, &spec.source_chat);
        let seed = if artifact_missing(spec) {
            format!(
                "Continuing prior work in a fresh chat (rollover). The predecessor bundle is at \
                 {}/ (transcript.jsonl + subagents/ + tool-results/) — read what you need to \
                 resume, then continue.",
                catch_up.display()
            )
        } else {
            format!(
                "Continuing prior work in a fresh chat (rollover). Hand-off note: {} — read it \
                 and continue. If it looks truncated or you are missing the most recent context, \
                 catch up from the predecessor transcript at {}/transcript.jsonl .",
                spec.artifact_path,
                catch_up.display()
            )
        };
        let mut env = record.spawn_env.clone();
        env_set(&mut env, "TX_SESSION_ID", spec.source_txid.clone());
        let adapter = self.adapter(llm_engine(&record)?)?;
        let read_only = record.read_only();
        let engine_command =
            shlex_join(&adapter.seed_command(&record.initial_cmd, &seed, read_only)?);
        // The respawn bypasses spawn_worker, so rebind the workspace here (idempotent).
        let engine_command = adapter.prepare_workspace(&engine_command, &record.cwd, &env)?;
        let command = env_prefix(&env) + &engine_command;
        let command =
            self.service
                .worker_launch_command(&command, Path::new(&record.cwd), read_only)?;
        let command = self
            .service
            .transportable_command(&spec.source_txid, &command)?;
        self.service.tmux().respawn_pane(&spec.pane, &command)?;
        self.record_seeded_chat(
            &spec.source_txid,
            &spec.cwd,
            "rollover",
            &spec.source_txid,
            &spec.source_chat,
            Some(&spec.source_chat),
        )?;
        self.log(
            "rollover-finish",
            &format!("{} (chat pending)", record.name),
        )
    }

    /// Detached backstop: wait for the artifact, give the distiller a grace window to finish
    /// itself, finish if nobody did, then tear the distiller + spec down.
    pub fn chat_op_watch(&self, op_id: &str, timings: WatchTimings) -> Result<()> {
        let spec = ChatOpSpec::load(self.home, op_id)?;
        let done = spec.dir(self.home).join("done");
        let artifact = (!spec.artifact_path.is_empty()).then(|| PathBuf::from(&spec.artifact_path));
        let deadline = Instant::now() + timings.timeout;

        // 1) wait for the distiller's artifact (or its own finish).
        while Instant::now() < deadline && !done.exists() {
            if artifact.as_ref().is_none_or(|path| path.exists()) {
                break;
            }
            std::thread::sleep(timings.poll);
        }
        // 2) the grace window for the distiller's own finish.
        let grace_end = Instant::now() + timings.grace;
        while Instant::now() < grace_end && !done.exists() {
            std::thread::sleep(timings.poll);
        }
        // 3) ensure completion; a finisher holding the claim gets time to write `done`, so the
        // teardown never kills a distiller mid-spawn.
        if !done.exists() {
            self.chat_op_finish(op_id)?;
        }
        for _ in 0..timings.settle_polls() {
            if done.exists() {
                break;
            }
            std::thread::sleep(timings.poll);
        }
        self.cleanup_op(&spec);
        Ok(())
    }

    /// Kill the distiller, drop its linked worktree, remove the spec dir. Best-effort: a missing
    /// session or dir is success.
    fn cleanup_op(&self, spec: &ChatOpSpec) {
        if !spec.distiller_name.is_empty() {
            let distiller = self.service.get(&spec.distiller_name).ok().flatten();
            let _ = self.service.kill(&spec.distiller_name);
            if let Some(distiller) = distiller {
                let _ = self
                    .service
                    .remove_worker_worktree(Path::new(&distiller.cwd));
            }
        }
        let _ = std::fs::remove_dir_all(spec.dir(self.home));
    }

    // ----- pane / note resolution ----------------------------------------------------------

    /// `$TMUX_PANE` when running inside the target itself, else the target's active pane.
    fn resolve_pane(&self, record: &Session) -> Result<String> {
        let tmux = self.service.tmux();
        if let Some(pane) = self.env.var("TMUX_PANE").filter(|pane| !pane.is_empty())
            && tmux.current_session_name().as_deref() == Some(record.tmux_name())
        {
            return Ok(pane.to_owned());
        }
        tmux.display_message("#{pane_id}", Some(record.tmux_name()))
            .ok_or_else(|| ChatError::NoPane(record.name.clone()))
    }

    /// `history/<txid>/rollover-<n>.md`, n past the existing notes (glob `rollover-*.md`).
    fn next_rollover_note(&self, txid: &str) -> PathBuf {
        let directory = self.home.history_dir().join(txid);
        let existing = std::fs::read_dir(&directory)
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with("rollover-") && name.ends_with(".md")
                    })
                    .count()
            })
            .unwrap_or(0);
        directory.join(format!("rollover-{}.md", existing + 1))
    }

    // ----- ChatRef + distiller spawn -------------------------------------------------------

    /// Append a seeded op's pending `ChatRef` (idempotent per role + origin); `close_chat`
    /// stamps `ended_at` on the rotated-out chat in the same save.
    fn record_seeded_chat(
        &self,
        txid: &str,
        cwd: &str,
        role: &str,
        origin_txid: &str,
        origin_chat: &str,
        close_chat: Option<&str>,
    ) -> Result<()> {
        let now = Number::from_f64(events::now());
        let mut session = self.load(txid)?;
        let name = session.name.clone();
        let llm = session.llm_mut().ok_or(ChatError::NotAnLlm(name))?;
        if let Some(close) = close_chat {
            for chat in &mut llm.chats {
                if chat.id.as_deref() == Some(close) && chat.ended_at.is_none() {
                    chat.ended_at.clone_from(&now);
                }
            }
        }
        let already = llm.chats.iter().any(|chat| {
            chat.id.is_none()
                && chat.role == role
                && chat.origin.chat_id.as_deref() == Some(origin_chat)
        });
        if !already {
            let engine = llm.engine;
            llm.chats.push(pending_chat(
                role,
                cwd,
                origin_txid,
                origin_chat,
                now,
                engine,
            ));
        }
        self.service.store().save(&session)?;
        Ok(())
    }

    /// The temporary distiller: the source engine's fixed distiller command with the seed as
    /// its initial prompt, tagged `temporary` + the op kind.
    fn spawn_distiller(
        &self,
        name: &str,
        kind: OpKind,
        cwd: &str,
        seed: &str,
        engine: Engine,
    ) -> Result<Session> {
        let cmd = shlex_join(&self.adapter(engine)?.distiller_command(seed));
        let tags = vec![DISTILLER_TAG.to_owned(), kind.as_str().to_owned()];
        let mut spec = self.process_spec(name, tags, cwd, cmd);
        spec.engine = Some(engine);
        Ok(self.service.spawn(spec)?)
    }

    /// This binary as a distiller seed runs it (the reference's resolved `bin/tx`).
    fn tx_program(&self) -> PathBuf {
        std::fs::canonicalize(&self.env.exe).unwrap_or_else(|_| self.env.exe.clone())
    }

    /// `env TX_IDE_HOME=<home> <tx>`, so the distiller's finish hits the same home.
    fn tx_invocation(&self) -> String {
        format!(
            "env TX_IDE_HOME={} {}",
            shlex_quote(&self.home.root().to_string_lossy()),
            shlex_quote(&self.tx_program().to_string_lossy())
        )
    }

    /// Fire `tx <verb> <op-id>` detached: a new session (`setsid`, the reference's
    /// `start_new_session`), stdio on /dev/null, the captured environment — so it survives the
    /// caller's pane being killed and never sits on the caller's latency path.
    fn detach(&self, verb: &str, op_id: &str) -> Result<()> {
        let mut command = Command::new(self.tx_program());
        command
            .args([verb, op_id])
            .env_clear()
            .envs(&self.env.vars)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: the closure runs in the forked child before exec and only calls setsid(2),
        // which is async-signal-safe and touches no parent memory.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn().map_err(|source| ChatError::Detach {
            verb: verb.to_owned(),
            source,
        })?;
        Ok(())
    }
}

/// Fall back to the bundle pointer: self-catch-up, or the distiller never wrote its file.
fn artifact_missing(spec: &ChatOpSpec) -> bool {
    spec.self_catch_up || !Path::new(&spec.artifact_path).exists()
}

fn new_op_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn llm_engine(session: &Session) -> Result<Engine> {
    session
        .llm()
        .map(|llm| llm.engine)
        .ok_or_else(|| ChatError::NotAnLlm(session.name.clone()))
}

/// A pending `ChatRef` (`id=None`) the capture hook fills from the first payload.
fn pending_chat(
    role: &str,
    cwd: &str,
    origin_txid: &str,
    origin_chat: &str,
    started_at: Option<Number>,
    engine: Engine,
) -> ChatRef {
    ChatRef {
        id: None,
        role: role.to_owned(),
        cwd: cwd.to_owned(),
        transcript_path: String::new(),
        origin: Origin {
            how: role.to_owned(),
            session_id: origin_txid.to_owned(),
            chat_id: Some(origin_chat.to_owned()),
        },
        bundle_path: None,
        started_at,
        ended_at: None,
        summary: String::new(),
        engine: Some(engine),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn slug_follows_the_reference() {
        // Values from lib/tx/chat.py `_slug` under python3.14.
        assert_eq!(slug("Fix the parser bug!"), "fix-the-parser-bug");
        assert_eq!(slug("!!!"), "task");
        assert_eq!(slug("A  b"), "a-b");
        assert_eq!(slug(&"a".repeat(60)), "a".repeat(48));
        assert_eq!(slug("Ünïcode ok"), "ünïcode-ok");
    }

    #[test]
    fn env_prefix_quotes_pairs() {
        assert_eq!(env_prefix(&[]), "");
        let env = vec![
            ("K".to_owned(), "V".to_owned()),
            ("A".to_owned(), "x y".to_owned()),
        ];
        assert_eq!(env_prefix(&env), "env K=V A='x y' ");
    }

    #[test]
    fn spec_roundtrip_and_defaults() {
        let mut spec = ChatOpSpec::new("op".into(), OpKind::Rollover, "s", "c");
        spec.cwd = "/w".into();
        spec.pane = "%1".into();
        let text = pyjson::dumps(&spec.to_value());
        assert!(text.starts_with("{\"op_id\": \"op\", \"kind\": \"rollover\""));
        assert_eq!(ChatOpSpec::parse("op", &text).unwrap(), spec);
        let minimal = r#"{"op_id":"o","kind":"handover","source_txid":"s","source_chat":"c","cwd":"/","artifact_path":""}"#;
        let parsed = ChatOpSpec::parse("o", minimal).unwrap();
        assert!(!parsed.read_only && !parsed.self_catch_up);
        assert_eq!(parsed.worker_name, "");
    }

    #[test]
    fn spec_errors_are_one_line_and_name_the_problem() {
        let missing = r#"{"op_id":"o","kind":"handover","source_txid":"s","source_chat":"c","artifact_path":""}"#;
        let error = ChatOpSpec::parse("o", missing).unwrap_err().to_string();
        assert_eq!(error, "chat-op 'o' has an invalid spec: missing key 'cwd'");
        let home = Home::new("/nonexistent-tx-home");
        let error = ChatOpSpec::load(&home, "no-such-op")
            .unwrap_err()
            .to_string();
        assert!(error.starts_with("chat-op 'no-such-op' has no readable spec at "));
        assert_eq!(error.lines().count(), 1);
    }

    #[test]
    fn watch_timings_from_env() {
        let env = |pairs: &[(&str, &str)]| Env {
            vars: pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect::<BTreeMap<_, _>>(),
            cwd: PathBuf::from("/"),
            exe: PathBuf::from("tx"),
        };
        let defaults = WatchTimings::from_env(&env(&[]));
        assert_eq!(defaults, WatchTimings::default());
        assert_eq!(defaults.settle_polls(), 10);
        let timings = WatchTimings::from_env(&env(&[
            (POLL_ENV, "0.05"),
            (GRACE_ENV, "0.2"),
            (TIMEOUT_ENV, "4.0"),
        ]));
        assert_eq!(timings.poll, Duration::from_millis(50));
        assert_eq!(timings.grace, Duration::from_millis(200));
        assert_eq!(timings.timeout, Duration::from_secs(4));
        let bad = WatchTimings::from_env(&env(&[(POLL_ENV, "0"), (GRACE_ENV, "x")]));
        assert_eq!(bad, WatchTimings::default());
    }
}
