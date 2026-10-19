//! Chat ops: `fork`, `handover`, `rollover`, `resume`, the idempotent `_chat-op-finish` and its
//! detached `_chat-op-watch` backstop. Port of main's `chat.rs` / `verbs/chat.rs`.
//!
//! - **fork**: a NEW Process session running the engine's resume argv rewritten to fork; a
//!   pending `fork` ref the capture hook fills from the fork's first new chat id.
//! - **handover**: a temporary distiller writes a brief, then runs `_chat-op-finish`, which
//!   creates a fresh worker seeded to read it (`--self-catch-up` skips the distiller).
//! - **rollover**: the SAME session's pane is respawned onto a fresh chat seeded with a note.
//!
//! An op lives in `<chat_dir>/ops/<op>/{spec.json,claim/,done,watch.lock}`. The finish is
//! single-shot through the atomic `claim/` mkdir. A watcher holds `watch.lock` for its
//! lifetime; an op whose watcher died is finished by the next public chat verb.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value, json};
use tx_core::core::{SESSION_ID_ENV, STATE_DIR_ENV, TMUX_SOCKET_ENV};
use tx_core::events::now;
use tx_core::{
    CoreError, CreateOptions, LaunchOptions, Lifecycle, ListFilter, PluginStatus, Session,
    SessionId, TxCore, Verb, VerbError, VerbIo,
};
use tx_ui::tmux::{Ui, shell_quote};

use crate::detach::detach;
use crate::engines::prepare_chat_for_cwd;
use crate::engines::{BUNDLE_TRANSCRIPT_NAME, Locator, agent_plan, distiller, fork_args};
use crate::ext::{
    AgentLaunch, ChatExt, ChatRef, Origin, Role, active_chat, agent_launch, chat_ext, history_ext,
};
use crate::history_verbs::say;
use crate::mirror::{IngestMode, bundle_dir, ingest_session};
use crate::{CHAT_ID, ChatError, HISTORY_ID, data_dir, ensure_dir, write_atomic};

/// Tag of the throwaway distiller (plus the op kind), so the in-flight helper shows in `ls`.
pub const DISTILLER_TAG: &str = "temporary";

pub const POLL_ENV: &str = "TX_CHAT_OP_POLL_S";
pub const GRACE_ENV: &str = "TX_CHAT_OP_GRACE_S";
pub const TIMEOUT_ENV: &str = "TX_CHAT_OP_TIMEOUT_S";
/// Set on a detached `_chat-op-finish`, so it never re-detaches.
pub const DETACHED_ENV: &str = "TX_CHAT_OP_DETACHED";

const SPEC_FILE: &str = "spec.json";
const CLAIM_DIR: &str = "claim";
const DONE_FILE: &str = "done";
const WATCH_LOCK: &str = "watch.lock";
const FINISH_VERB: &str = "_chat-op-finish";
const WATCH_VERB: &str = "_chat-op-watch";

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> ChatError + '_ {
    move |source| ChatError::Io {
        path: path.to_owned(),
        source,
    }
}

/// `_slug`: a filesystem-safe slug for `handover-<slug>.md`.
pub fn slug(text: &str) -> String {
    let cleaned: String = text
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
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

// ----- watcher timings ---------------------------------------------------------------------

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
    /// Verb-side only. Seconds (float); an unset, malformed or out-of-range value keeps its
    /// default (the poll must be positive, grace / timeout may be zero).
    pub fn from_env() -> Self {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    fn from_lookup(var: impl Fn(&str) -> Option<String>) -> Self {
        let defaults = Self::default();
        let seconds = |name: &str, fallback: Duration, allow_zero: bool| {
            var(name)
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
}

// ----- the op spec -------------------------------------------------------------------------

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

/// `<chat_dir>/ops/<op>/spec.json`: everything the finish needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatOpSpec {
    pub op_id: String,
    pub kind: OpKind,
    /// The source session's id.
    pub source: String,
    pub source_chat: String,
    pub cwd: String,
    /// The brief / note the distiller writes; `""` = self-catch-up.
    pub artifact_path: String,
    pub self_catch_up: bool,
    pub worker_name: String,
    pub pane: String,
    pub distiller_name: String,
}

impl ChatOpSpec {
    fn new(kind: OpKind, source: &SessionId, source_chat: &str, cwd: &Path) -> Self {
        Self {
            op_id: uuid::Uuid::new_v4().to_string(),
            kind,
            source: source.to_string(),
            source_chat: source_chat.to_owned(),
            cwd: cwd.to_string_lossy().into_owned(),
            artifact_path: String::new(),
            self_catch_up: false,
            worker_name: String::new(),
            pane: String::new(),
            distiller_name: String::new(),
        }
    }

    pub fn to_value(&self) -> Value {
        json!({
            "op_id": self.op_id,
            "kind": self.kind.as_str(),
            "source": self.source,
            "source_chat": self.source_chat,
            "cwd": self.cwd,
            "artifact_path": self.artifact_path,
            "self_catch_up": self.self_catch_up,
            "worker_name": self.worker_name,
            "pane": self.pane,
            "distiller_name": self.distiller_name,
        })
    }

    /// Six required string fields, the rest defaulted; anything but `handover` is a rollover.
    pub fn parse(op_id: &str, text: &str) -> Result<Self, ChatError> {
        let invalid = |reason: String| ChatError::SpecInvalid {
            op_id: op_id.to_owned(),
            reason,
        };
        let value: Value =
            serde_json::from_str(text).map_err(|error| invalid(format!("not JSON ({error})")))?;
        let Value::Object(data) = value else {
            return Err(invalid("not a JSON object".to_owned()));
        };
        let text_field = |key: &str, required: bool| -> Result<String, ChatError> {
            match data.get(key) {
                Some(Value::String(text)) => Ok(text.clone()),
                Some(_) => Err(invalid(format!("'{key}' is not a string"))),
                None if required => Err(invalid(format!("missing key '{key}'"))),
                None => Ok(String::new()),
            }
        };
        let op = text_field("op_id", true)?;
        let kind = if text_field("kind", true)? == "handover" {
            OpKind::Handover
        } else {
            OpKind::Rollover
        };
        Ok(Self {
            op_id: op,
            kind,
            source: text_field("source", true)?,
            source_chat: text_field("source_chat", true)?,
            cwd: text_field("cwd", true)?,
            artifact_path: text_field("artifact_path", true)?,
            self_catch_up: flag_of(&data, "self_catch_up").map_err(invalid)?,
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

/// Fall back to the bundle pointer: self-catch-up, or the distiller never wrote its file.
fn artifact_missing(spec: &ChatOpSpec) -> bool {
    spec.self_catch_up || !Path::new(&spec.artifact_path).exists()
}

fn pending_ref(role: Role, origin_session: &SessionId, origin_chat: &str) -> ChatRef {
    let how = serde_json::to_value(role)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    ChatRef {
        id: None,
        role,
        how,
        since: now(),
        ended_at: None,
        origin: Some(Origin {
            session: origin_session.to_string(),
            chat: origin_chat.to_owned(),
            rest: Map::new(),
        }),
        transcript: None,
        rest: Map::new(),
    }
}

/// `env K=V … ` for a respawn command (respawn-pane takes no `-e`).
fn env_prefix(env: &[(String, String)]) -> String {
    if env.is_empty() {
        return String::new();
    }
    let pairs: Vec<String> = env
        .iter()
        .map(|(key, value)| format!("{}={}", shell_quote(key), shell_quote(value)))
        .collect();
    format!("env {} ", pairs.join(" "))
}

fn set_env(env: &mut Vec<(String, String)>, key: &str, value: String) {
    env.retain(|(k, _)| k != key);
    env.push((key.to_owned(), value));
}

// ----- the operations ----------------------------------------------------------------------

pub struct Ops<'a> {
    pub core: &'a TxCore,
    /// `<state_dir>/plugins/chat/`.
    pub chat_dir: PathBuf,
    /// `<state_dir>/plugins/history/` when the `history` plugin is active.
    pub history_dir: Option<PathBuf>,
    /// The `txc` a distiller seed and the detached finish / watch run.
    pub exe: PathBuf,
    /// Bounds the rollover finish's wait for the agent's turn to end.
    pub timings: WatchTimings,
}

/// The pieces of a resolved source the ops share.
struct Source {
    session: Session,
    launch: AgentLaunch,
    chat_id: String,
}

impl<'a> Ops<'a> {
    /// The verb-side constructor: `history_dir` only while `history` is active.
    pub fn new(core: &'a TxCore, exe: PathBuf, timings: WatchTimings) -> Self {
        let state = &core.config().state_dir;
        let history_active = core
            .plugin_status()
            .iter()
            .any(|(id, status)| id == HISTORY_ID && *status == PluginStatus::Active);
        Self {
            core,
            chat_dir: data_dir(state, CHAT_ID),
            history_dir: history_active.then(|| data_dir(state, HISTORY_ID)),
            exe,
            timings,
        }
    }

    fn ops_dir(&self) -> PathBuf {
        self.chat_dir.join("ops")
    }

    fn op_dir(&self, op_id: &str) -> PathBuf {
        self.ops_dir().join(op_id)
    }

    fn notes_dir(&self, session: &SessionId) -> PathBuf {
        self.chat_dir.join("notes").join(session.to_string())
    }

    fn profile_file(&self, name: &str) -> PathBuf {
        self.chat_dir.join("profiles").join(format!("{name}.md"))
    }

    /// `base`, `base-2`, `base-3` … : the first not carried by a Running record.
    pub fn next_free_name(&self, base: &str) -> Result<String, ChatError> {
        let mut running = Vec::new();
        for id in self
            .core
            .list_sessions(&ListFilter::new().lifecycle(Lifecycle::Running))?
        {
            match self.core.get_session(&id) {
                Ok(session) => running.push(session.name),
                Err(CoreError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        let taken = |name: &str| running.iter().any(|n| n == name);
        if !taken(base) {
            return Ok(base.to_owned());
        }
        Ok((2..)
            .map(|n| format!("{base}-{n}"))
            .find(|name| !taken(name))
            .expect("an unbounded range always has a free name"))
    }

    fn lookup(&self, op: &str, token: &str) -> Result<Session, ChatError> {
        let not_found = || ChatError::NotFound {
            op: op.to_owned(),
            token: token.to_owned(),
        };
        let id = match self.core.resolve(token) {
            Ok(id) => id,
            Err(CoreError::NotFound(_)) => return Err(not_found()),
            Err(error) => return Err(error.into()),
        };
        match self.core.get_session(&id) {
            Ok(session) => Ok(session),
            Err(CoreError::NotFound(_)) => Err(not_found()),
            Err(error) => Err(error.into()),
        }
    }

    /// A record with an agent launch and an active chat, else `<op>: '<name>' has no <missing>`.
    fn source(&self, op: &str, token: &str, missing: &str) -> Result<Source, ChatError> {
        let session = self.lookup(op, token)?;
        let ext = chat_ext(&session)?;
        let no_chat = || ChatError::NoChat {
            op: op.to_owned(),
            name: session.name.clone(),
            missing: missing.to_owned(),
        };
        let launch = agent_launch(&session, &ext).ok_or_else(no_chat)?;
        let chat_id = active_chat(&ext)
            .and_then(|chat| chat.id.clone())
            .ok_or_else(no_chat)?;
        Ok(Source {
            session,
            launch,
            chat_id,
        })
    }

    /// Mirror the source first (only with `history` active): a reader gets the durable copy.
    fn ingest(&self, id: &SessionId) -> Result<(), ChatError> {
        if let Some(history_dir) = &self.history_dir {
            ingest_session(self.core, history_dir, id, IngestMode::Wait)?;
        }
        Ok(())
    }

    /// The bundle pointer a seed names: the mirrored bundle dir with `history` active, else the
    /// live transcript the chat ref recorded.
    fn bundle_pointer(&self, session: &Session, chat_id: &str) -> PathBuf {
        if let Some(history_dir) = &self.history_dir {
            return bundle_dir(history_dir, &session.id, chat_id);
        }
        let from_chat = chat_ext(session).ok().and_then(|ext| {
            ext.chats
                .into_iter()
                .find(|chat| chat.id.as_deref() == Some(chat_id))
                .and_then(|chat| chat.transcript)
        });
        let from_history = || {
            history_ext(session)
                .ok()
                .and_then(|ext| ext.chats.get(chat_id).map(|c| c.transcript.clone()))
        };
        PathBuf::from(from_chat.or_else(from_history).unwrap_or_default())
    }

    /// The bundle's transcript file: `<bundle>/transcript.jsonl`, or the live transcript itself.
    fn bundle_transcript(&self, session: &Session, chat_id: &str) -> PathBuf {
        let pointer = self.bundle_pointer(session, chat_id);
        if self.history_dir.is_some() {
            pointer.join(BUNDLE_TRANSCRIPT_NAME)
        } else {
            pointer
        }
    }

    /// `<'exe'> --state-dir <'state'> _chat-op-finish <'op'>`.
    fn finish_command(&self, op_id: &str) -> String {
        format!(
            "{} --state-dir {} {FINISH_VERB} {}",
            shell_quote(&self.exe.to_string_lossy()),
            shell_quote(&self.core.config().state_dir.to_string_lossy()),
            shell_quote(op_id),
        )
    }

    fn detach(&self, verb: &str, op_id: &str) -> Result<(), ChatError> {
        let env: &[(&str, &str)] = if verb == FINISH_VERB {
            &[(DETACHED_ENV, "1")]
        } else {
            &[]
        };
        detach(
            &self.exe,
            &self.core.config().state_dir,
            &[verb, op_id],
            env,
        )
        .map_err(io_err(&self.exe))
    }

    fn save_spec(&self, spec: &ChatOpSpec) -> Result<(), ChatError> {
        let dir = self.op_dir(&spec.op_id);
        ensure_dir(&dir).map_err(io_err(&dir))?;
        let path = dir.join(SPEC_FILE);
        let text = serde_json::to_vec(&spec.to_value()).expect("a JSON value always serializes");
        write_atomic(&path, &text).map_err(io_err(&path))
    }

    fn load_spec(&self, op_id: &str) -> Result<ChatOpSpec, ChatError> {
        let path = self.op_dir(op_id).join(SPEC_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| ChatError::SpecUnreadable {
            op_id: op_id.to_owned(),
            path: path.clone(),
            source,
        })?;
        ChatOpSpec::parse(op_id, &text)
    }

    fn update_chat(&self, id: &SessionId, f: impl FnOnce(&mut ChatExt)) -> Result<(), ChatError> {
        self.core.update_ext(id, CHAT_ID, |value| {
            let Ok(mut ext) = (match &*value {
                Value::Null => Ok(ChatExt::default()),
                other => serde_json::from_value::<ChatExt>(other.clone()),
            }) else {
                return;
            };
            f(&mut ext);
            if let Ok(updated) = serde_json::to_value(&ext) {
                *value = updated;
            }
        })?;
        Ok(())
    }

    fn agent_options(launch: &AgentLaunch) -> LaunchOptions {
        let mut options = LaunchOptions::agent(&launch.engine).profile(launch.profile.clone());
        if let Some(model) = &launch.model {
            options = options.model(model);
        }
        if let Some(effort) = launch.effort {
            options = options.effort(effort);
        }
        options
    }

    // ----- fork --------------------------------------------------------------------------

    /// Branch the source's active chat into a NEW session carrying its full history.
    pub fn fork(&self, source: &str, name: Option<&str>) -> Result<Session, ChatError> {
        let src = self.source("fork", source, "chat to fork")?;
        let base = match name.filter(|name| !name.is_empty()) {
            Some(name) => name.to_owned(),
            None => format!("{}-fork", src.session.name),
        };
        let name = self.next_free_name(&base)?;
        let cwd = &src.session.cwd;
        let mut plan = agent_plan(
            self.core,
            &src.launch,
            cwd,
            &self.profile_file(&name),
            Some(&src.chat_id),
            None,
        )?;
        fork_args(&src.launch.engine, &mut plan.args);
        tx_core::engine::apply_workspace(cwd, &plan.workspace)?;
        let mut opts = CreateOptions::new(
            &name,
            cwd,
            LaunchOptions::process(&plan.program, &plan.args),
        )
        .parent(src.session.id.clone())
        .tags(src.session.tags.clone());
        for (key, value) in &plan.env {
            opts = opts.env(key, value);
        }
        let id = self.core.create_session(opts)?;
        let launch = src.launch.clone();
        let fork = pending_ref(Role::Fork, &src.session.id, &src.chat_id);
        self.update_chat(&id, |ext| {
            ext.engine = Some(launch.engine.clone());
            ext.launch = Some(launch);
            ext.chats = vec![fork];
        })?;
        Ok(self.core.get_session(&id)?)
    }

    // ----- handover ----------------------------------------------------------------------

    /// Distill the source chat into a brief for a NEW worker; returns the worker's name.
    pub fn handover(
        &self,
        source: &str,
        task: &str,
        name: Option<&str>,
        self_catch_up: bool,
    ) -> Result<String, ChatError> {
        let src = self.source("handover", source, "chat to hand over")?;
        self.ingest(&src.session.id)?;
        let base = match name.filter(|name| !name.is_empty()) {
            Some(name) => name.to_owned(),
            None => format!("{}-handover", src.session.name),
        };
        let worker_name = self.next_free_name(&base)?;
        let notes = self.notes_dir(&src.session.id);
        let brief_path = notes.join(format!("handover-{}.md", slug(task)));

        let mut spec = ChatOpSpec::new(
            OpKind::Handover,
            &src.session.id,
            &src.chat_id,
            &src.session.cwd,
        );
        if !self_catch_up {
            spec.artifact_path = brief_path.to_string_lossy().into_owned();
        }
        spec.self_catch_up = self_catch_up;
        spec.worker_name.clone_from(&worker_name);

        if self_catch_up {
            self.save_spec(&spec)?;
            self.finish(&spec.op_id)?;
            return Ok(worker_name);
        }

        ensure_dir(&notes).map_err(io_err(&notes))?;
        spec.distiller_name = self.next_free_name(&format!("{worker_name}-distill"))?;
        self.save_spec(&spec)?;
        let seed = format!(
            "You are a tx-ide handover distiller (a temporary helper). Read the predecessor chat \
             bundle at {} . Distill a focused, self-contained brief for this task and write it to \
             {} : «{task}». Capture only what the new worker needs to start — relevant context, \
             current state, constraints, and key file paths — not the whole history. When the \
             brief file is saved, run exactly this command and nothing else: {}",
            self.bundle_transcript(&src.session, &src.chat_id).display(),
            brief_path.display(),
            self.finish_command(&spec.op_id),
        );
        self.spawn_distiller(&spec, OpKind::Handover, &src, &seed)?;
        self.detach(WATCH_VERB, &spec.op_id)?;
        Ok(worker_name)
    }

    /// The temporary distiller: the source engine with the distiller model and effort, the seed
    /// as its initial prompt, tagged `temporary` + the op kind.
    fn spawn_distiller(
        &self,
        spec: &ChatOpSpec,
        kind: OpKind,
        src: &Source,
        seed: &str,
    ) -> Result<SessionId, ChatError> {
        let (model, effort) = distiller(&src.launch.engine);
        let mut options = LaunchOptions::agent(&src.launch.engine).initial_prompt(seed);
        if let Some(model) = model {
            options = options.model(model);
        }
        if let Some(effort) = effort {
            options = options.effort(effort);
        }
        let opts = CreateOptions::new(&spec.distiller_name, &src.session.cwd, options)
            .tags([DISTILLER_TAG, kind.as_str()]);
        Ok(self.core.create_session(opts)?)
    }

    // ----- rollover ----------------------------------------------------------------------

    /// Rotate the SAME session onto a fresh chat in the same pane. `pane_env` is `$TMUX_PANE`.
    pub fn rollover(
        &self,
        target: Option<&str>,
        pane_env: Option<&str>,
        self_catch_up: bool,
    ) -> Result<(), ChatError> {
        let ui = Ui::from_core(self.core);
        let current = ui.current_session_name();
        let target = match target.filter(|name| !name.is_empty()) {
            Some(name) => name.to_owned(),
            None => current.clone().ok_or(ChatError::NotInsideTmux)?,
        };
        let src = self.source("rollover", &target, "active chat to roll over")?;
        let record = &src.session;
        let pane = match pane_env.filter(|pane| !pane.is_empty()) {
            Some(pane) if current.as_deref() == Some(record.name.as_str()) => pane.to_owned(),
            _ => ui
                .display_message("#{pane_id}", Some(&format!("={}:", record.name)))
                .ok_or_else(|| ChatError::NoPane(record.name.clone()))?,
        };
        let note_path = self.next_rollover_note(&record.id);

        let mut spec = ChatOpSpec::new(OpKind::Rollover, &record.id, &src.chat_id, &record.cwd);
        if !self_catch_up {
            spec.artifact_path = note_path.to_string_lossy().into_owned();
        }
        spec.self_catch_up = self_catch_up;
        spec.pane = pane;

        if self_catch_up {
            self.save_spec(&spec)?;
            // The caller may BE the pane being respawned: the finish must outlive it.
            return self.detach(FINISH_VERB, &spec.op_id);
        }

        self.ingest(&record.id)?;
        let notes = self.notes_dir(&record.id);
        ensure_dir(&notes).map_err(io_err(&notes))?;
        spec.distiller_name = self.next_free_name(&format!("{}-rollover-distill", record.name))?;
        self.save_spec(&spec)?;
        let seed = format!(
            "You are a tx-ide rollover summariser (a temporary helper). Read the predecessor chat \
             bundle at {} . Write a concise hand-off note — the current task, what is done, the \
             immediate next steps, and key files and decisions — to {} . When the note file is \
             saved, run exactly this command and nothing else: {}",
            self.bundle_transcript(record, &src.chat_id).display(),
            note_path.display(),
            self.finish_command(&spec.op_id),
        );
        self.spawn_distiller(&spec, OpKind::Rollover, &src, &seed)?;
        self.detach(WATCH_VERB, &spec.op_id)
    }

    /// `notes/<sid>/rollover-<n>.md`, n past the existing notes.
    fn next_rollover_note(&self, id: &SessionId) -> PathBuf {
        let dir = self.notes_dir(id);
        let existing = std::fs::read_dir(&dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.starts_with("rollover-") && name.ends_with(".md")
                    })
                    .count()
            })
            .unwrap_or(0);
        dir.join(format!("rollover-{}.md", existing + 1))
    }

    // ----- resume ------------------------------------------------------------------------

    /// Re-create a past record's agent resumed onto its active chat.
    pub fn resume(
        &self,
        target: &str,
        as_name: Option<&str>,
        cwd: Option<&Path>,
        loc: &Locator,
    ) -> Result<Session, ChatError> {
        let src = self.source("resume", target, "chat to resume")?;
        let record = &src.session;
        let name = as_name
            .filter(|name| !name.is_empty())
            .unwrap_or(&record.name)
            .to_owned();
        if self.running_name(&name)? {
            return Err(ChatError::LiveName(name));
        }
        let explicit = cwd.filter(|cwd| !cwd.as_os_str().is_empty());
        let cwd = explicit.unwrap_or(&record.cwd).to_owned();
        if !cwd.is_dir() {
            return Err(ChatError::CwdMissing {
                cwd: cwd.display().to_string(),
                remedy: match explicit {
                    None => "pass --cwd <dir>".to_owned(),
                    Some(_) => format!("'{}' is not a directory", cwd.display()),
                },
            });
        }
        let engine = &src.launch.engine;
        if cwd != record.cwd {
            prepare_chat_for_cwd(loc, engine, &src.chat_id, &record.cwd, &cwd)?;
        }
        let transcript = loc
            .locate_transcript(engine, &src.chat_id, Some(&cwd.to_string_lossy()))
            .map(|path| path.to_string_lossy().into_owned());
        let opts = CreateOptions::new(
            &name,
            &cwd,
            Self::agent_options(&src.launch).resume(&src.chat_id),
        )
        .parent(record.id.clone())
        .tags(record.tags.clone());
        let id = self.core.create_session(opts)?;
        let resumed = ChatRef {
            id: Some(src.chat_id.clone()),
            role: Role::Primary,
            how: "resume".into(),
            since: now(),
            ended_at: None,
            origin: Some(Origin {
                session: record.id.to_string(),
                chat: src.chat_id.clone(),
                rest: Map::new(),
            }),
            transcript,
            rest: Map::new(),
        };
        let engine = engine.clone();
        self.update_chat(&id, |ext| {
            ext.engine = Some(engine);
            ext.chats = vec![resumed];
        })?;
        Ok(self.core.get_session(&id)?)
    }

    fn running_name(&self, name: &str) -> Result<bool, ChatError> {
        for id in self
            .core
            .list_sessions(&ListFilter::new().lifecycle(Lifecycle::Running))?
        {
            match self.core.get_session(&id) {
                Ok(session) if session.name == name => return Ok(true),
                Ok(_) | Err(CoreError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }

    // ----- the idempotent finish + its watchdog ------------------------------------------

    /// Complete a handover / rollover from its spec. Single execution via the `claim/` mkdir;
    /// a loser (or an already finished op) is a no-op.
    pub fn finish(&self, op_id: &str) -> Result<(), ChatError> {
        let spec = self.load_spec(op_id)?;
        let dir = self.op_dir(op_id);
        let done = dir.join(DONE_FILE);
        if done.exists() {
            return Ok(());
        }
        let claim = dir.join(CLAIM_DIR);
        match std::fs::create_dir(&claim) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
            Err(error) => return Err(io_err(&claim)(error)),
        }
        match spec.kind {
            OpKind::Handover => self.finish_handover(&spec)?,
            OpKind::Rollover => self.finish_rollover(&spec)?,
        }
        std::fs::write(&done, "").map_err(io_err(&done))?;
        // Self-catch-up has no watchdog to tear the op down.
        if spec.distiller_name.is_empty() {
            self.cleanup(&spec);
        }
        Ok(())
    }

    /// The `_chat-op-finish` verb. A rollover finish waits for the turn to end, so run in the
    /// foreground (a distiller's Bash tool) it re-runs itself detached, outliving its caller.
    pub fn finish_verb(&self, op_id: &str, detached: bool) -> Result<(), ChatError> {
        if !detached && self.load_spec(op_id)?.kind == OpKind::Rollover {
            return self.detach(FINISH_VERB, op_id);
        }
        self.finish(op_id)
    }

    fn spec_source(&self, spec: &ChatOpSpec) -> Result<Session, ChatError> {
        let not_found = || ChatError::NotFound {
            op: FINISH_VERB.to_owned(),
            token: spec.source.clone(),
        };
        let id = spec.source.parse::<SessionId>().map_err(|_| not_found())?;
        match self.core.get_session(&id) {
            Ok(session) => Ok(session),
            Err(CoreError::NotFound(_)) => Err(not_found()),
            Err(error) => Err(error.into()),
        }
    }

    fn spec_launch(&self, source: &Session) -> Result<AgentLaunch, ChatError> {
        agent_launch(source, &chat_ext(source)?)
            .ok_or_else(|| ChatError::NotAnAgent(source.name.clone()))
    }

    /// Create the fresh handover worker seeded to read the brief (or the bundle pointer).
    fn finish_handover(&self, spec: &ChatOpSpec) -> Result<(), ChatError> {
        let source = self.spec_source(spec)?;
        let launch = self.spec_launch(&source)?;
        let bundle = self.bundle_pointer(&source, &spec.source_chat);
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
        // parent = SOURCE, not whoever runs this finish.
        let opts = CreateOptions::new(
            &spec.worker_name,
            &spec.cwd,
            Self::agent_options(&launch).initial_prompt(seed),
        )
        .parent(source.id.clone())
        .tags(source.tags.clone());
        let worker = self.core.create_session(opts)?;
        let pending = pending_ref(Role::Handover, &source.id, &spec.source_chat);
        self.update_chat(&worker, |ext| {
            ext.engine = Some(launch.engine.clone());
            ext.chats.push(pending);
        })
    }

    /// Respawn the pane onto a fresh chat in place, after the turn ends and a final re-ingest.
    fn finish_rollover(&self, spec: &ChatOpSpec) -> Result<(), ChatError> {
        let record = self.spec_source(spec)?;
        self.wait_until_idle(&record.id)?;
        self.ingest(&record.id)?;
        let record = self.core.get_session(&record.id)?;
        let launch = self.spec_launch(&record)?;
        let catch_up = self.bundle_pointer(&record, &spec.source_chat);
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
        let plan = agent_plan(
            self.core,
            &launch,
            &record.cwd,
            &self.profile_file(&record.name),
            None,
            Some(&seed),
        )?;
        tx_core::engine::apply_workspace(&record.cwd, &plan.workspace)?;
        let config = self.core.config();
        let mut env = vec![
            (SESSION_ID_ENV.to_owned(), record.id.to_string()),
            (
                TMUX_SOCKET_ENV.to_owned(),
                self.core.socket_path().to_string_lossy().into_owned(),
            ),
            (
                STATE_DIR_ENV.to_owned(),
                config.state_dir.to_string_lossy().into_owned(),
            ),
        ];
        for (key, value) in config.env.iter().chain(&plan.env) {
            set_env(&mut env, key, value.clone());
        }
        let argv: Vec<String> = std::iter::once(&plan.program)
            .chain(&plan.args)
            .map(|arg| shell_quote(arg))
            .collect();
        let command = env_prefix(&env) + &argv.join(" ");
        Ui::from_core(self.core).respawn_pane(&spec.pane, &command)?;
        let source_chat = spec.source_chat.clone();
        let pending = pending_ref(Role::Rollover, &record.id, &spec.source_chat);
        self.update_chat(&record.id, |ext| {
            let ended = now();
            for chat in &mut ext.chats {
                if chat.id.as_deref() == Some(source_chat.as_str()) && chat.ended_at.is_none() {
                    chat.ended_at = Some(ended);
                }
            }
            let already = ext.chats.iter().any(|chat| {
                chat.id.is_none()
                    && chat.role == Role::Rollover
                    && chat.origin.as_ref().map(|o| o.chat.as_str()) == Some(source_chat.as_str())
            });
            if !already {
                ext.chats.push(pending);
            }
        })
    }

    /// Wait while `ext.hooks.state == "working"`, bounded by the timeout.
    fn wait_until_idle(&self, id: &SessionId) -> Result<(), ChatError> {
        let deadline = Instant::now() + self.timings.timeout;
        loop {
            let session = self.core.get_session(id)?;
            let working = session
                .ext
                .get("hooks")
                .and_then(|hooks| hooks.get("state"))
                .and_then(Value::as_str)
                == Some("working");
            if !working || Instant::now() >= deadline {
                return Ok(());
            }
            std::thread::sleep(self.timings.poll);
        }
    }

    /// Detached backstop: wait for the artifact, give the distiller a grace window to finish
    /// itself, finish if nobody did, then tear the distiller + spec down. Holds
    /// `<op>/watch.lock` for its lifetime; a second watcher returns at once.
    pub fn watch(&self, op_id: &str, t: WatchTimings) -> Result<(), ChatError> {
        let spec = self.load_spec(op_id)?;
        let dir = self.op_dir(op_id);
        let Some(_lock) = try_lock(&dir.join(WATCH_LOCK))? else {
            return Ok(());
        };
        let done = dir.join(DONE_FILE);
        let artifact = (!spec.artifact_path.is_empty()).then(|| PathBuf::from(&spec.artifact_path));
        let deadline = Instant::now() + t.timeout;
        while Instant::now() < deadline && !done.exists() {
            if artifact.as_ref().is_none_or(|path| path.exists()) {
                break;
            }
            std::thread::sleep(t.poll);
        }
        let grace_end = Instant::now() + t.grace;
        while Instant::now() < grace_end && !done.exists() {
            std::thread::sleep(t.poll);
        }
        if !done.exists() {
            self.finish(op_id)?;
        }
        // A finisher holding the claim may wait out a turn: never tear it down early.
        let claim = dir.join(CLAIM_DIR);
        let settle_end = Instant::now() + t.timeout;
        while Instant::now() < settle_end && claim.exists() && !done.exists() {
            std::thread::sleep(t.poll);
        }
        self.cleanup(&spec);
        Ok(())
    }

    /// Stop the distiller and remove the op dir. Best effort.
    fn cleanup(&self, spec: &ChatOpSpec) {
        if !spec.distiller_name.is_empty()
            && let Ok(id) = self.core.resolve(&spec.distiller_name)
            && self
                .core
                .get_session(&id)
                .is_ok_and(|s| s.lifecycle == Lifecycle::Running)
        {
            let _ = self.core.stop_session(&id);
        }
        let _ = std::fs::remove_dir_all(self.op_dir(&spec.op_id));
    }

    /// Ops whose watcher is gone: the `watch.lock` is free and `spec.json` is older than
    /// `timeout + grace`. Unfinished ones are finished; each is then torn down. Returns the
    /// failures; a failed op keeps its `claim/`, so the next sweep only tears it down.
    pub fn finish_abandoned(&self, t: WatchTimings) -> Vec<(String, ChatError)> {
        let Ok(entries) = std::fs::read_dir(self.ops_dir()) else {
            return Vec::new();
        };
        let stale_after = t.timeout + t.grace;
        let mut failures = Vec::new();
        for entry in entries.filter_map(Result::ok) {
            let op_id = entry.file_name().to_string_lossy().into_owned();
            let dir = entry.path();
            let spec_path = dir.join(SPEC_FILE);
            let Ok(modified) = std::fs::metadata(&spec_path).and_then(|m| m.modified()) else {
                continue;
            };
            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default();
            if age <= stale_after {
                continue;
            }
            let _lock = match try_lock(&dir.join(WATCH_LOCK)) {
                Ok(Some(lock)) => lock,
                Ok(None) => continue,
                Err(error) => {
                    failures.push((op_id, error));
                    continue;
                }
            };
            let result = self.finish(&op_id).and_then(|()| self.load_spec(&op_id));
            match result {
                Ok(spec) => self.cleanup(&spec),
                // `finish` tore a self-catch-up op down itself.
                Err(ChatError::SpecUnreadable { .. }) if !spec_path.exists() => {}
                Err(error) => failures.push((op_id, error)),
            }
        }
        failures
    }
}

/// An exclusive non-blocking `flock`; `None` when another process holds it.
fn try_lock(path: &Path) -> Result<Option<File>, ChatError> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(io_err(path))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(io_err(path)(error)),
    }
}

// ----- verbs -------------------------------------------------------------------------------

pub(crate) fn ops(core: &TxCore) -> Result<Ops<'_>, VerbError> {
    let exe =
        std::env::current_exe().map_err(|e| VerbError::Failed(format!("current_exe: {e}")))?;
    Ok(Ops::new(core, exe, WatchTimings::from_env()))
}

/// Run before every public chat verb: finish abandoned ops, report failures, never fail.
pub(crate) fn sweep_abandoned(core: &TxCore, stderr: &mut dyn Write) {
    let Ok(ops) = ops(core) else {
        return;
    };
    for (op_id, error) in ops.finish_abandoned(WatchTimings::from_env()) {
        let _ = writeln!(stderr, "chat: abandoned op {op_id}: {error}");
    }
}

/// A `ChatError` the user caused is one line on stderr and exit 1; a core error propagates.
fn report(io: &mut VerbIo<'_>, verb: &str, error: ChatError) -> Result<i32, VerbError> {
    let text = match &error {
        ChatError::NotFound { .. } | ChatError::NoChat { .. } => error.to_string(),
        ChatError::LiveName(_) | ChatError::CwdMissing { .. } => error.to_string(),
        ChatError::NotInsideTmux => {
            "rollover: run inside a tmux session or pass <session>".to_owned()
        }
        ChatError::Core(_) => return Err(error.into()),
        _ => format!("{verb}: {error}"),
    };
    say(io.stderr, &format!("{text}\n"))?;
    Ok(1)
}

fn short(id: &str) -> String {
    id.chars().take(8).collect()
}

pub fn verbs() -> Vec<Arc<dyn Verb>> {
    vec![
        Arc::new(Fork),
        Arc::new(Handover),
        Arc::new(Rollover),
        Arc::new(Resume),
        Arc::new(OpFinish),
        Arc::new(OpWatch),
    ]
}

/// Splits `argv` into positionals and the `--self-catch-up` flag; any other `-…` is `None`.
fn positionals_and_catch_up(argv: &[String]) -> Option<(Vec<&str>, bool)> {
    let mut positionals = Vec::new();
    let mut catch_up = false;
    for arg in argv {
        match arg.as_str() {
            "--self-catch-up" => catch_up = true,
            flag if flag.starts_with('-') => return None,
            other => positionals.push(other),
        }
    }
    Some((positionals, catch_up))
}

struct Fork;

const FORK_USAGE: &str = "usage: txc fork <source> [<new-name>]\n";

impl Verb for Fork {
    fn name(&self) -> &'static str {
        "fork"
    }

    fn usage(&self) -> &'static str {
        "fork <source> [<new-name>]   a NEW session on a fork of the source's chat"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let (source, name) = match positionals_and_catch_up(argv)
            .as_ref()
            .map(|(p, c)| (&p[..], c))
        {
            Some((&[source], false)) => (source, None),
            Some((&[source, name], false)) => (source, Some(name)),
            _ => {
                say(io.stderr, FORK_USAGE)?;
                return Ok(2);
            }
        };
        sweep_abandoned(core, io.stderr);
        match ops(core)?.fork(source, name) {
            Ok(new) => {
                say(
                    io.stdout,
                    &format!(
                        "Forked '{source}' → '{}' (chat pending, cwd={})\n",
                        new.name,
                        new.cwd.display()
                    ),
                )?;
                Ok(0)
            }
            Err(error) => report(io, "fork", error),
        }
    }
}

struct Handover;

const HANDOVER_USAGE: &str = "usage: txc handover <source> <task> [<new-name>] [--self-catch-up]\n";

impl Verb for Handover {
    fn name(&self) -> &'static str {
        "handover"
    }

    fn usage(&self) -> &'static str {
        "handover <source> <task> [<new-name>] [--self-catch-up]   a NEW worker from a brief"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let parsed = positionals_and_catch_up(argv);
        let (source, task, name, catch_up) = match parsed.as_ref().map(|(p, c)| (&p[..], *c)) {
            Some((&[source, task], c)) => (source, task, None, c),
            Some((&[source, task, name], c)) => (source, task, Some(name), c),
            _ => {
                say(io.stderr, HANDOVER_USAGE)?;
                return Ok(2);
            }
        };
        sweep_abandoned(core, io.stderr);
        match ops(core)?.handover(source, task, name, catch_up) {
            Ok(worker) => {
                let how = if catch_up {
                    "self-catch-up"
                } else {
                    "distilling brief"
                };
                say(
                    io.stdout,
                    &format!(
                        "Handover '{source}' → worker '{worker}' ({how}; launches when ready)\n"
                    ),
                )?;
                Ok(0)
            }
            Err(error) => report(io, "handover", error),
        }
    }
}

struct Rollover;

const ROLLOVER_USAGE: &str = "usage: txc rollover [<session>] [--self-catch-up]\n";

impl Verb for Rollover {
    fn name(&self) -> &'static str {
        "rollover"
    }

    fn usage(&self) -> &'static str {
        "rollover [<session>] [--self-catch-up]   the same session onto a fresh chat"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let parsed = positionals_and_catch_up(argv);
        let (target, catch_up) = match parsed.as_ref().map(|(p, c)| (&p[..], *c)) {
            Some((&[], c)) => (None, c),
            Some((&[target], c)) => (Some(target), c),
            _ => {
                say(io.stderr, ROLLOVER_USAGE)?;
                return Ok(2);
            }
        };
        sweep_abandoned(core, io.stderr);
        let pane = std::env::var("TMUX_PANE").ok();
        match ops(core)?.rollover(target, pane.as_deref(), catch_up) {
            Ok(()) => {
                let how = if catch_up {
                    "self-catch-up"
                } else {
                    "summarizing first"
                };
                say(
                    io.stdout,
                    &format!(
                        "Rollover scheduled ({how}); the same session rotates onto a fresh chat \
                         when ready\n"
                    ),
                )?;
                Ok(0)
            }
            Err(error) => report(io, "rollover", error),
        }
    }
}

/// `<target> [--as <name>] [--cwd <dir>]`, each option at most once.
fn parse_resume(argv: &[String]) -> Option<(&str, Option<&str>, Option<&str>)> {
    let mut positionals = Vec::new();
    let (mut as_name, mut cwd) = (None, None);
    let mut args = argv.iter();
    while let Some(arg) = args.next() {
        let slot = match arg.as_str() {
            "--as" => &mut as_name,
            "--cwd" => &mut cwd,
            flag if flag.starts_with('-') => return None,
            other => {
                positionals.push(other);
                continue;
            }
        };
        if slot.replace(args.next()?.as_str()).is_some() {
            return None;
        }
    }
    match positionals[..] {
        [target] => Some((target, as_name, cwd)),
        _ => None,
    }
}

struct Resume;

const RESUME_USAGE: &str = "usage: txc resume <session> [--as <name>] [--cwd <dir>]\n";

impl Verb for Resume {
    fn name(&self) -> &'static str {
        "resume"
    }

    fn usage(&self) -> &'static str {
        "resume <session> [--as <name>] [--cwd <dir>]   re-create a past session on its chat"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some((target, as_name, cwd)) = parse_resume(argv) else {
            say(io.stderr, RESUME_USAGE)?;
            return Ok(2);
        };
        sweep_abandoned(core, io.stderr);
        let loc = Locator::from_env();
        match ops(core)?.resume(target, as_name, cwd.map(Path::new), &loc) {
            Ok(new) => {
                let ext = chat_ext(&new).unwrap_or_default();
                let chat = ext.chats.first();
                let chat_id = chat.and_then(|c| c.id.clone()).unwrap_or_default();
                if chat.is_none_or(|c| c.transcript.is_none()) {
                    say(
                        io.stderr,
                        &format!(
                            "resume: warning — transcript for chat {} not found under {}; claude \
                             --resume may start a fresh conversation\n",
                            short(&chat_id),
                            new.cwd.display()
                        ),
                    )?;
                }
                let old = new
                    .parent
                    .as_ref()
                    .and_then(|parent| core.get_session(parent).ok())
                    .map_or_else(|| target.to_owned(), |s| s.name);
                say(
                    io.stdout,
                    &format!(
                        "Resumed '{old}' as '{}' (chat {}, cwd={})\n",
                        new.name,
                        short(&chat_id),
                        new.cwd.display()
                    ),
                )?;
                Ok(0)
            }
            Err(error) => report(io, "resume", error),
        }
    }
}

struct OpFinish;

impl Verb for OpFinish {
    fn name(&self) -> &'static str {
        FINISH_VERB
    }

    fn usage(&self) -> &'static str {
        "_chat-op-finish <op>   complete a handover / rollover (idempotent)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [op_id] = argv else {
            say(io.stderr, "usage: txc _chat-op-finish <op>\n")?;
            return Ok(2);
        };
        let detached = std::env::var_os(DETACHED_ENV).is_some();
        match ops(core)?.finish_verb(op_id, detached) {
            Ok(()) => Ok(0),
            Err(error) => report(io, FINISH_VERB, error),
        }
    }
}

struct OpWatch;

impl Verb for OpWatch {
    fn name(&self) -> &'static str {
        WATCH_VERB
    }

    fn usage(&self) -> &'static str {
        "_chat-op-watch <op>   detached watchdog of a handover / rollover"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [op_id] = argv else {
            say(io.stderr, "usage: txc _chat-op-watch <op>\n")?;
            return Ok(2);
        };
        match ops(core)?.watch(op_id, WatchTimings::from_env()) {
            Ok(()) => Ok(0),
            Err(error) => report(io, WATCH_VERB, error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn slug_cases() {
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
        assert_eq!(env_prefix(&env), "env 'K'='V' 'A'='x y' ");
    }

    fn spec_text(skip: &str, overrides: &[(&str, Value)]) -> String {
        let mut value = json!({
            "op_id": "o", "kind": "handover", "source": "s", "source_chat": "c",
            "cwd": "/", "artifact_path": "",
        });
        let map = value.as_object_mut().unwrap();
        map.remove(skip);
        for (key, v) in overrides {
            map.insert((*key).to_owned(), v.clone());
        }
        value.to_string()
    }

    #[test]
    fn spec_parse_reports_missing_and_mistyped_keys() {
        let parsed = ChatOpSpec::parse("o", &spec_text("", &[])).unwrap();
        assert_eq!(parsed.kind, OpKind::Handover);
        assert!(!parsed.self_catch_up);
        assert_eq!(parsed.worker_name, "");

        let mut spec = ChatOpSpec::new(
            OpKind::Rollover,
            &"s".parse().unwrap(),
            "c",
            Path::new("/w"),
        );
        spec.pane = "%1".into();
        spec.self_catch_up = true;
        let round = ChatOpSpec::parse(&spec.op_id, &spec.to_value().to_string()).unwrap();
        assert_eq!(round, spec);

        let error = |text: String| ChatOpSpec::parse("o", &text).unwrap_err().to_string();
        assert_eq!(
            error(spec_text("cwd", &[])),
            "op o: invalid spec: missing key 'cwd'"
        );
        assert_eq!(
            error(spec_text("", &[("source", json!(3))])),
            "op o: invalid spec: 'source' is not a string"
        );
        assert_eq!(
            error(spec_text("", &[("self_catch_up", json!([]))])),
            "op o: invalid spec: 'self_catch_up' is not a boolean"
        );
        assert_eq!(
            error(spec_text("", &[("pane", json!(null))])),
            "op o: invalid spec: 'pane' is not a string"
        );
        assert_eq!(error("[1]".into()), "op o: invalid spec: not a JSON object");
        assert!(error("{".into()).starts_with("op o: invalid spec: not JSON ("));
        for (flag, want) in [
            (json!(1), true),
            (json!(0), false),
            (json!("x"), true),
            (json!(""), false),
            (json!(null), false),
        ] {
            let parsed =
                ChatOpSpec::parse("o", &spec_text("", &[("self_catch_up", flag)])).unwrap();
            assert_eq!(parsed.self_catch_up, want);
        }
        let other = ChatOpSpec::parse("o", &spec_text("", &[("kind", json!("zzz"))])).unwrap();
        assert_eq!(other.kind, OpKind::Rollover);
    }

    #[test]
    fn spec_unreadable_is_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let core = TxCore::open(tx_core::CoreConfig::new(dir.path())).unwrap();
        let ops = Ops::new(&core, PathBuf::from("txc"), WatchTimings::default());
        let error = ops.finish("no-such-op").unwrap_err().to_string();
        assert!(error.starts_with("op no-such-op: cannot read "), "{error}");
        assert_eq!(error.lines().count(), 1);
    }

    #[test]
    fn watch_timings_from_env_rules() {
        let timings = |pairs: &[(&str, &str)]| {
            let env: HashMap<String, String> = pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect();
            WatchTimings::from_lookup(|name| env.get(name).cloned())
        };
        let defaults = timings(&[]);
        assert_eq!(defaults, WatchTimings::default());
        assert_eq!(defaults.poll, Duration::from_secs(2));
        assert_eq!(defaults.grace, Duration::from_secs(20));
        assert_eq!(defaults.timeout, Duration::from_secs(600));
        let set = timings(&[(POLL_ENV, "0.05"), (GRACE_ENV, " 0.2 "), (TIMEOUT_ENV, "4")]);
        assert_eq!(set.poll, Duration::from_millis(50));
        assert_eq!(set.grace, Duration::from_millis(200));
        assert_eq!(set.timeout, Duration::from_secs(4));
        let zeros = timings(&[(POLL_ENV, "0"), (GRACE_ENV, "0"), (TIMEOUT_ENV, "0")]);
        assert_eq!(zeros.poll, Duration::from_secs(2), "poll must be positive");
        assert_eq!(zeros.grace, Duration::ZERO);
        assert_eq!(zeros.timeout, Duration::ZERO);
        let bad = timings(&[(POLL_ENV, "-1"), (GRACE_ENV, "x"), (TIMEOUT_ENV, "inf")]);
        assert_eq!(bad, WatchTimings::default());
    }

    #[test]
    fn next_free_name_skips_running_names() {
        let dir = tempfile::tempdir().unwrap();
        let core = TxCore::open(tx_core::CoreConfig::new(dir.path())).unwrap();
        let store = tx_core::SessionStore::new(dir.path().join("sessions"));
        let record = |id: &str, name: &str, lifecycle: &str| -> Session {
            serde_json::from_value(json!({
                "id": id, "name": name, "parent": null, "tags": [], "cwd": "/tmp",
                "created_at": 1.0, "ended_at": null, "lifecycle": lifecycle,
                "launch": {"kind": "process", "program": "sh", "args": []}, "ext": {},
            }))
            .unwrap()
        };
        for session in [
            record("s1", "w", "running"),
            record("s2", "w-2", "running"),
            record("s3", "w-3", "stopped"),
            record("s4", "x", "exited"),
        ] {
            store.save(&session).unwrap();
        }
        let ops = Ops::new(&core, PathBuf::from("txc"), WatchTimings::default());
        assert_eq!(ops.next_free_name("w").unwrap(), "w-3");
        assert_eq!(ops.next_free_name("x").unwrap(), "x");
        assert_eq!(ops.next_free_name("new").unwrap(), "new");
    }

    #[test]
    fn pending_refs_carry_their_role_as_how() {
        let id: SessionId = "s".parse().unwrap();
        let r = pending_ref(Role::Handover, &id, "c");
        assert_eq!(r.how, "handover");
        assert!(r.id.is_none());
        assert_eq!(r.origin.unwrap().chat, "c");
        assert_eq!(pending_ref(Role::Fork, &id, "c").how, "fork");
    }
}
