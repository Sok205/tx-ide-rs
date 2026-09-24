//! Hook entry points (hooks.py): `tx hook <event>` drives session `state` from engine + tmux hooks.
//!
//! Pure dispatch over `SessionService`: the transition rules live in `record_state`; this module
//! decides which event maps to which state, captures the chat id off the payload, and fires the
//! detached history ingest on a real transition into WAITING / IDLE.

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use crate::app::Env;
use crate::engines::EngineRegistry;
use crate::engines::adapter::CapturedChat;
use crate::events::EventLog;
use crate::history::{History, HistoryError, IngestMode};
use crate::service::{ServiceError, SessionService};
use crate::session::{LlmSession, Session, State};
use crate::storage::Home;
use crate::store::{SessionStore, StoreError};

/// The env var every tx-spawned session carries: "which record fired this hook".
pub const SESSION_ID_ENV: &str = "TX_SESSION_ID";

/// `Notification` subtypes meaning "the agent yielded — needs you" → WAITING.
const WAITING_NOTIFICATION_TYPES: [&str; 3] =
    ["idle_prompt", "permission_prompt", "elicitation_dialog"];

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    History(#[from] HistoryError),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

/// Everything a hook touches, passed in explicitly.
pub struct Hooks<'a> {
    pub service: &'a SessionService,
    pub store: &'a SessionStore,
    pub events: &'a EventLog,
    pub engines: &'a EngineRegistry,
    pub home: &'a Home,
    pub env: &'a Env,
}

impl Hooks<'_> {
    /// Apply the hook named by `argv[0]`. `Ok(0)` on every handled path — "not ours" (D4) is a
    /// success; only a failure the reference would also raise surfaces as `Err`.
    pub fn dispatch(&self, argv: &[String], stdin: &mut dyn Read) -> Result<i32, HookError> {
        let event = argv.first().map(String::as_str).unwrap_or("");

        if event == "session-closed" {
            self.service.reconcile()?;
            return Ok(0);
        }
        if event == "ingest" {
            let session_id = argv
                .get(1)
                .map(String::as_str)
                .unwrap_or_else(|| self.session_id_env());
            self.run_ingest(session_id)?;
            return Ok(0);
        }

        let session_id = self.session_id_env();
        if session_id.is_empty() {
            return Ok(0);
        }
        let Some(session) = self.store.load(session_id)? else {
            return Ok(0);
        };
        if session.llm().is_none() {
            return Ok(0);
        }

        if matches!(event, "session-start" | "prompt-submit") {
            // A hook must never fail a turn: the next capture event retries.
            let _ = self.capture_chat_ref(session, &read_payload(stdin));
        }

        let state = match event {
            "notification" => notification_is_yield(&read_payload(stdin)).then_some(State::Waiting),
            other => state_for_event(other),
        };
        let Some(state) = state else {
            return Ok(0);
        };
        let changed = self.service.record_state(session_id, state)?;
        if changed && matches!(state, State::Waiting | State::Idle) {
            self.trigger_ingest(session_id)?;
        }
        Ok(0)
    }

    fn session_id_env(&self) -> &str {
        self.env.var(SESSION_ID_ENV).unwrap_or("")
    }

    fn capture_chat_ref(&self, session: Session, payload: &Value) -> Result<(), HookError> {
        let Some(llm) = session.llm() else {
            return Ok(());
        };
        let Some(adapter) = self.engines.get(llm.engine) else {
            return Ok(());
        };
        let Some(captured) = adapter.capture_session_id(payload) else {
            return Ok(());
        };
        self.complete_pending(session, captured)
    }

    /// Stamp the captured chat onto the latest pending `ChatRef`, unless it is already recorded,
    /// nothing is pending, a lazy fork reports its source id, or another session owns the chat.
    fn complete_pending(
        &self,
        mut session: Session,
        captured: CapturedChat,
    ) -> Result<(), HookError> {
        let Some(index) = session
            .llm()
            .and_then(|llm| pending_index(llm, &captured.session_id))
        else {
            return Ok(());
        };
        if self.owned_by_other_session(&session.id, &captured.session_id) {
            let msg = format!(
                "{}: chat {} owned by another session — refused cross-bind",
                session.name, captured.session_id
            );
            return self
                .events
                .append("capture-skip", &msg, self.env.actor())
                .map_err(|source| HookError::Io {
                    path: self.events.path().to_path_buf(),
                    source,
                });
        }
        if let Some(llm) = session.llm_mut() {
            let pending = &mut llm.chats[index];
            pending.id = Some(captured.session_id);
            pending.transcript_path = captured.transcript_path;
        }
        self.store.save(&session)?;
        Ok(())
    }

    fn owned_by_other_session(&self, session_id: &str, captured_id: &str) -> bool {
        self.store.all().iter().any(|other| {
            other.id != session_id
                && other.llm().is_some_and(|llm| {
                    llm.chats
                        .iter()
                        .any(|chat| chat.id.as_deref() == Some(captured_id))
                })
        })
    }

    /// Re-exec `tx hook ingest <id>` in a new session with detached stdio, so the originating
    /// hook returns before the (possibly slow) mirror runs.
    fn trigger_ingest(&self, session_id: &str) -> Result<(), HookError> {
        spawn_detached(&self.env.exe, &self.env.vars, session_id).map_err(|source| HookError::Io {
            path: self.env.exe.clone(),
            source,
        })
    }

    fn run_ingest(&self, session_id: &str) -> Result<(), HookError> {
        if !session_id.is_empty() {
            History::new(self.home, self.engines).ingest_session(
                self.store,
                session_id,
                IngestMode::Coalesce,
            )?;
        }
        Ok(())
    }
}

fn spawn_detached(exe: &Path, vars: &BTreeMap<String, String>, session_id: &str) -> io::Result<()> {
    let mut command = Command::new(exe);
    command
        .args(["hook", "ingest", session_id])
        .env_clear()
        .envs(vars)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: `setsid` is async-signal-safe and touches no parent state (start_new_session).
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().map(drop)
}

/// The static event → state table; `notification` is payload-dependent and handled apart.
fn state_for_event(event: &str) -> Option<State> {
    match event {
        "prompt-submit" | "working" => Some(State::Working),
        "stop" => Some(State::Waiting),
        "session-end" => Some(State::Idle),
        _ => None,
    }
}

fn notification_is_yield(payload: &Value) -> bool {
    payload
        .get("notification_type")
        .and_then(Value::as_str)
        .is_some_and(|kind| WAITING_NOTIFICATION_TYPES.contains(&kind))
}

/// The hook payload from stdin, tolerantly: `{}` for none / unreadable / bad JSON / non-object
/// (the reference crashed on a non-object `Notification` payload; a hook never fails here).
fn read_payload(stdin: &mut dyn Read) -> Value {
    let empty = || Value::Object(Map::new());
    let mut raw = String::new();
    if stdin.read_to_string(&mut raw).is_err() {
        return empty();
    }
    let raw = raw.trim();
    if raw.is_empty() {
        return empty();
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(value @ Value::Object(_)) => value,
        _ => empty(),
    }
}

/// Index of the latest pending ref to fill with `captured_id`, or `None` when the id is already
/// recorded, nothing is pending, or the pending ref is a lazy fork still reporting its source id.
fn pending_index(llm: &LlmSession, captured_id: &str) -> Option<usize> {
    if llm
        .chats
        .iter()
        .any(|chat| chat.id.as_deref() == Some(captured_id))
    {
        return None;
    }
    let index = llm.chats.iter().rposition(|chat| chat.id.is_none())?;
    let pending = &llm.chats[index];
    if pending.role == "fork" && pending.origin.chat_id.as_deref() == Some(captured_id) {
        return None;
    }
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(text: &str) -> Value {
        read_payload(&mut text.as_bytes())
    }

    #[test]
    fn payload_degrades_to_empty_object() {
        for raw in ["", "  \n", "garbage", "[1]", "3", "null", "\"s\""] {
            assert_eq!(payload(raw), Value::Object(Map::new()), "{raw:?}");
        }
        assert_eq!(payload(" {\"a\":1}\n")["a"], 1);
    }

    #[test]
    fn notification_yield_subtypes_only() {
        for kind in WAITING_NOTIFICATION_TYPES {
            assert!(notification_is_yield(
                &serde_json::json!({ "notification_type": kind })
            ));
        }
        for other in [
            serde_json::json!({"notification_type": "auth_success"}),
            serde_json::json!({"notification_type": ["idle_prompt"]}),
            serde_json::json!({}),
        ] {
            assert!(!notification_is_yield(&other));
        }
    }

    #[test]
    fn event_table() {
        assert_eq!(state_for_event("prompt-submit"), Some(State::Working));
        assert_eq!(state_for_event("working"), Some(State::Working));
        assert_eq!(state_for_event("stop"), Some(State::Waiting));
        assert_eq!(state_for_event("session-end"), Some(State::Idle));
        for none in ["session-start", "notification", "bogus", ""] {
            assert_eq!(state_for_event(none), None);
        }
    }
}
