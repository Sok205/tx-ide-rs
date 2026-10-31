//! The session record: what core stores on disk for every managed session.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::state::LlmEngineState;

/// The record schema this build reads and writes.
pub const RECORD_VERSION: u64 = 1;
/// Session names are tmux session names and file-name fragments; keep them short.
pub const MAX_NAME_BYTES: usize = 128;
/// Ids are uuids (36 bytes); the slack lets other minting schemes in without a format change.
pub const MAX_ID_BYTES: usize = 64;

/// A record id: a uuid v4 minted by `create_session`. Every way to build one validates the
/// grammar `[A-Za-z0-9_-]{1,64}` (parsing, deserializing), so an id is always a safe file-name
/// fragment and tmux option value: `<id>.json`, `<id>.lock`, `@tx-id`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SessionId(String);

impl From<SessionId> for String {
    fn from(id: SessionId) -> Self {
        id.0
    }
}

impl TryFrom<String> for SessionId {
    type Error = InvalidSessionId;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        id.parse()
    }
}

impl SessionId {
    pub fn mint() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid session id {0:?} (allowed: [A-Za-z0-9_-]+, at most {MAX_ID_BYTES} bytes)")]
pub struct InvalidSessionId(pub String);

impl FromStr for SessionId {
    type Err = InvalidSessionId;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        if is_safe_fragment(id, MAX_ID_BYTES) {
            Ok(Self(id.to_owned()))
        } else {
            Err(InvalidSessionId(id.to_owned()))
        }
    }
}

/// `[A-Za-z0-9_-]{1,max}`: safe as a path fragment, a tmux exact target and an option value.
fn is_safe_fragment(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    Running,
    /// The tmux session vanished (process exit, server crash, a kill behind core's back).
    Exited,
    /// `stop_session` killed it.
    Stopped,
}

impl Lifecycle {
    pub const ALL: [Lifecycle; 3] = [Lifecycle::Running, Lifecycle::Exited, Lifecycle::Stopped];

    /// Nothing in core moves a record out of a terminal state.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Lifecycle::Running)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Lifecycle::Running => "running",
            Lifecycle::Exited => "exited",
            Lifecycle::Stopped => "stopped",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown lifecycle {0:?} (expected running, exited or stopped)")]
pub struct InvalidLifecycle(pub String);

impl FromStr for Lifecycle {
    type Err = InvalidLifecycle;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|lifecycle| lifecycle.as_str() == text)
            .ok_or_else(|| InvalidLifecycle(text.to_owned()))
    }
}

/// How the agent was started. Engine ids are strings, so a new engine needs no change here.
/// One variant: core holds agents only (the `process` kind went with PR 11; Storage PR 14
/// removes the enum).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Launch {
    Agent {
        engine: String,
        profile: Vec<String>,
        /// Written by builds before PR 12a; the model is engine data (`ext.<engine>.model`)
        /// now. Kept only so an old record round-trips until Storage PR 14 migrates it. Never
        /// read; new records write nothing here.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// As `model`: the effort of a record written before PR 12a (`ext.<engine>.effort` now).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        #[serde(default)]
        resume: Option<String>,
    },
}

/// The snapshot every read returns. Disk is the record; tmux holds only the `@tx-id` link.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub name: String,
    /// Ancestry only; core never schedules or stops children.
    pub parent: Option<SessionId>,
    pub tags: Vec<String>,
    pub cwd: PathBuf,
    pub created_at: f64,
    pub ended_at: Option<f64>,
    /// When the agent was archived: hidden from listings and its name freed, with nothing
    /// deleted. `None` for an unarchived record and for records written before this field
    /// existed; a respawn clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<f64>,
    /// When the record was last written; core stamps it on every save. A record written before
    /// this field existed loads with `created_at`.
    pub last_modified_at: f64,
    pub lifecycle: Lifecycle,
    pub launch: Launch,
    /// The agent's state as its engine adapter reported it. Core-owned. A record written
    /// before this field existed loads as `idle` while running and `ended` once ended.
    pub state: LlmEngineState,
    /// Plugin-owned data keyed by plugin id; written through `TxCore::update_ext`. Core never
    /// reads it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ext: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("record version {0} is not supported (this build reads version {RECORD_VERSION})")]
    UnsupportedVersion(u64),
    #[error("record version {0} is not an unsigned integer")]
    InvalidVersion(String),
    #[error("record has no version field")]
    MissingVersion,
    #[error("{0} is not a finite number")]
    NonFiniteTime(&'static str),
    /// A `"kind": "process"` record, written before PR 11. The store skips it until Storage
    /// PR 14 migrates it.
    #[error("a process record, written before core held agents only")]
    ProcessRecord,
}

#[derive(Serialize)]
struct RecordOut<'a> {
    version: u64,
    #[serde(flatten)]
    session: &'a Session,
}

#[derive(Deserialize)]
struct RecordIn {
    version: Option<u64>,
    #[serde(flatten)]
    session: Session,
}

impl Session {
    /// Pretty JSON, newline-terminated, `version` first. Fails for what JSON cannot carry: a
    /// non-UTF-8 `cwd` or a non-finite timestamp (serde_json would write `null`, which
    /// `from_json` then refuses — better to refuse before the record exists).
    pub fn to_json(&self) -> Result<String, RecordError> {
        if !self.created_at.is_finite() {
            return Err(RecordError::NonFiniteTime("created_at"));
        }
        if self.ended_at.is_some_and(|at| !at.is_finite()) {
            return Err(RecordError::NonFiniteTime("ended_at"));
        }
        if self.archived_at.is_some_and(|at| !at.is_finite()) {
            return Err(RecordError::NonFiniteTime("archived_at"));
        }
        if !self.last_modified_at.is_finite() {
            return Err(RecordError::NonFiniteTime("last_modified_at"));
        }
        let record = RecordOut {
            version: RECORD_VERSION,
            session: self,
        };
        let mut text = serde_json::to_string_pretty(&record)?;
        text.push('\n');
        Ok(text)
    }

    pub fn from_json(text: &str) -> Result<Self, RecordError> {
        let mut value: serde_json::Map<String, serde_json::Value> = serde_json::from_str(text)?;
        match value.get("version") {
            None => return Err(RecordError::MissingVersion),
            Some(serde_json::Value::Number(version))
                if version.as_u64() == Some(RECORD_VERSION) => {}
            Some(serde_json::Value::Number(version)) => {
                return Err(match version.as_u64() {
                    Some(other) => RecordError::UnsupportedVersion(other),
                    None => RecordError::InvalidVersion(version.to_string()),
                });
            }
            Some(other) => return Err(RecordError::InvalidVersion(other.to_string())),
        }
        if value.get("launch").and_then(|launch| launch.get("kind"))
            == Some(&serde_json::Value::String("process".to_owned()))
        {
            return Err(RecordError::ProcessRecord);
        }
        // Written before `last_modified_at` existed: the last write is unknown, creation stands in.
        if let Some(created_at) = value.get("created_at").cloned() {
            value.entry("last_modified_at").or_insert(created_at);
        }
        // Written before `state` existed: a running agent is taken as idle, an ended one as ended.
        if !value.contains_key("state") {
            let state = match value.get("lifecycle") {
                Some(serde_json::Value::String(lifecycle)) if lifecycle == "running" => {
                    LlmEngineState::Idle
                }
                _ => LlmEngineState::Ended,
            };
            value.insert("state".to_owned(), serde_json::to_value(state)?);
        }
        let record: RecordIn = serde_json::from_value(serde_json::Value::Object(value))?;
        let _ = record.version;
        Ok(record.session)
    }

    /// A plugin's own `ext` record, typed. `Ok(None)` when the plugin never wrote one; `Err`
    /// when the stored value is not a `T`.
    pub fn ext<T: serde::de::DeserializeOwned>(
        &self,
        plugin: &str,
    ) -> Result<Option<T>, serde_json::Error> {
        self.ext.get(plugin).map(T::deserialize).transpose()
    }
}

/// `[A-Za-z0-9_-]+`, at most `MAX_NAME_BYTES`. The grammar keeps names safe as exact tmux targets
/// (`=name`) and as path fragments.
pub fn validate_name(name: &str) -> Result<(), CoreError> {
    if is_safe_fragment(name, MAX_NAME_BYTES) {
        Ok(())
    } else {
        Err(CoreError::InvalidName(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn sid(id: &str) -> SessionId {
        id.parse().unwrap()
    }

    fn agent() -> Session {
        Session {
            id: sid("b0000000-0000-4000-8000-000000000001"),
            name: "worker-1".into(),
            parent: Some(sid("p")),
            tags: vec!["a".into(), "b".into()],
            cwd: PathBuf::from("/tmp/w"),
            created_at: 1727000000.5,
            ended_at: None,
            archived_at: None,
            last_modified_at: 1727000000.5,
            lifecycle: Lifecycle::Running,
            launch: Launch::Agent {
                engine: "claude".into(),
                profile: vec!["COMMON".into(), "dev".into()],
                model: None,
                effort: None,
                resume: Some("chat-1".into()),
            },
            ext: std::collections::BTreeMap::new(),
            state: LlmEngineState::Working,
        }
    }

    /// An agent that was stopped.
    fn ended() -> Session {
        Session {
            id: sid("x"),
            name: "done".into(),
            parent: None,
            tags: vec![],
            cwd: PathBuf::from("/"),
            created_at: 1.0,
            ended_at: Some(2.0),
            archived_at: None,
            last_modified_at: 1.0,
            lifecycle: Lifecycle::Stopped,
            launch: Launch::Agent {
                engine: "codex".into(),
                profile: vec![],
                model: None,
                effort: None,
                resume: None,
            },
            ext: std::collections::BTreeMap::new(),
            state: LlmEngineState::Ended,
        }
    }

    #[test]
    fn agent_round_trips_through_json() {
        let text = agent().to_json().unwrap();
        assert_eq!(Session::from_json(&text).unwrap(), agent());
    }

    #[test]
    fn an_ended_agent_round_trips_through_json() {
        let text = ended().to_json().unwrap();
        assert_eq!(Session::from_json(&text).unwrap(), ended());
    }

    #[test]
    fn json_shape_is_the_documented_one() {
        let value: serde_json::Value = serde_json::from_str(&agent().to_json().unwrap()).unwrap();
        assert_eq!(value["version"], RECORD_VERSION);
        assert_eq!(value["lifecycle"], "running");
        assert_eq!(value["launch"]["kind"], "agent");
        assert_eq!(value["launch"]["resume"], "chat-1");
        assert_eq!(value["launch"].get("model"), None);
        assert_eq!(value["launch"].get("effort"), None);
        assert_eq!(value["parent"], "p");
        assert_eq!(value["ended_at"], serde_json::Value::Null);
        assert_eq!(value["state"], "working");
        let value: serde_json::Value = serde_json::from_str(&ended().to_json().unwrap()).unwrap();
        assert_eq!(value["launch"]["kind"], "agent");
        assert_eq!(value["lifecycle"], "stopped");
        assert_eq!(value["ended_at"], 2.0);
        assert_eq!(value["state"], "ended");
    }

    #[test]
    fn json_is_pretty_and_newline_terminated() {
        let text = ended().to_json().unwrap();
        assert!(text.ends_with("}\n"), "{text:?}");
        assert!(text.contains("\n  \"id\""), "{text:?}");
    }

    #[test]
    fn optional_fields_may_be_null() {
        let text = r#"{"version":1,"id":"i","name":"n","parent":null,"tags":[],"cwd":"/","created_at":1.0,"ended_at":null,"lifecycle":"exited","launch":{"kind":"agent","engine":"e","profile":[],"model":null,"effort":null}}"#;
        let session = Session::from_json(text).unwrap();
        assert_eq!(session.parent, None);
        assert_eq!(session.ended_at, None);
        assert_eq!(session.lifecycle, Lifecycle::Exited);
        assert!(
            matches!(session.launch, Launch::Agent { ref model, ref effort, ref resume, .. } if model.is_none() && effort.is_none() && resume.is_none())
        );
    }

    /// A record written before PR 12a carries `model` and `effort` in `launch`. They are engine
    /// data now (`ext.<engine>`), never read here, but they round-trip until Storage PR 14
    /// migrates them, so a hook re-saving the record loses nothing.
    #[test]
    fn launch_model_and_effort_of_an_old_record_round_trip() {
        let text = r#"{"version":1,"id":"i","name":"n","parent":null,"tags":[],"cwd":"/","created_at":1.0,"ended_at":null,"lifecycle":"exited","launch":{"kind":"agent","engine":"e","profile":[],"model":"opus","effort":"xhigh"}}"#;
        let session = Session::from_json(text).unwrap();
        assert!(matches!(
            session.launch,
            Launch::Agent { ref model, ref effort, .. }
                if model.as_deref() == Some("opus") && effort.as_deref() == Some("xhigh")
        ));
        let value: serde_json::Value = serde_json::from_str(&session.to_json().unwrap()).unwrap();
        assert_eq!(value["launch"]["model"], "opus");
        assert_eq!(value["launch"]["effort"], "xhigh");
    }

    #[test]
    fn a_record_without_last_modified_at_loads_with_its_creation() {
        let text = r#"{"version":1,"id":"i","name":"n","parent":null,"tags":[],"cwd":"/","created_at":7.5,"ended_at":null,"lifecycle":"running","launch":{"kind":"agent","engine":"e","profile":[]}}"#;
        assert_eq!(Session::from_json(text).unwrap().last_modified_at, 7.5);
        let stamped = text.replace(
            r#""created_at":7.5,"#,
            r#""created_at":7.5,"last_modified_at":9.0,"#,
        );
        assert_eq!(Session::from_json(&stamped).unwrap().last_modified_at, 9.0);
    }

    /// A record written before agent state existed carries no `state`: running reads as idle,
    /// ended as ended.
    #[test]
    fn a_record_without_state_loads_as_idle_or_ended() {
        let running = r#"{"version":1,"id":"i","name":"n","parent":null,"tags":[],"cwd":"/","created_at":1.0,"ended_at":null,"lifecycle":"running","launch":{"kind":"agent","engine":"e","profile":[]}}"#;
        assert_eq!(
            Session::from_json(running).unwrap().state,
            LlmEngineState::Idle
        );
        for lifecycle in ["exited", "stopped"] {
            let text = running.replace("\"running\"", &format!("\"{lifecycle}\""));
            assert_eq!(
                Session::from_json(&text).unwrap().state,
                LlmEngineState::Ended
            );
        }
        let stated = running.replace(r#""lifecycle""#, r#""state":"working","lifecycle""#);
        assert_eq!(
            Session::from_json(&stated).unwrap().state,
            LlmEngineState::Working
        );
    }

    /// A `process` record (written before PR 11) is named as such, so the store can skip it
    /// instead of failing on an unknown launch kind.
    #[test]
    fn a_process_record_is_refused_by_name() {
        let text = r#"{"version":1,"id":"i","name":"n","parent":null,"tags":[],"cwd":"/","created_at":1.0,"ended_at":null,"lifecycle":"running","launch":{"kind":"process","program":"sh","args":[]}}"#;
        assert!(matches!(
            Session::from_json(text),
            Err(RecordError::ProcessRecord)
        ));
    }

    #[test]
    fn unknown_keys_are_ignored_for_forward_compatibility() {
        let mut text = ended().to_json().unwrap();
        text.insert_str(text.find("\"id\"").unwrap(), "\"future\": {\"x\": 1},\n  ");
        assert_eq!(Session::from_json(&text).unwrap(), ended());
    }

    #[test]
    fn newer_version_is_refused() {
        let text = ended()
            .to_json()
            .unwrap()
            .replace("\"version\": 1", "\"version\": 2");
        assert!(matches!(
            Session::from_json(&text),
            Err(RecordError::UnsupportedVersion(2))
        ));
    }

    #[test]
    fn missing_version_is_refused() {
        let text = ended().to_json().unwrap().replace("\"version\": 1,\n", "");
        assert!(matches!(
            Session::from_json(&text),
            Err(RecordError::MissingVersion)
        ));
    }

    #[test]
    fn garbage_is_a_json_error() {
        assert!(matches!(Session::from_json("{"), Err(RecordError::Json(_))));
        assert!(matches!(
            Session::from_json("[]"),
            Err(RecordError::Json(_))
        ));
        assert!(matches!(
            Session::from_json(r#"{"version":1}"#),
            Err(RecordError::Json(_))
        ));
    }

    #[test]
    fn to_json_refuses_a_non_utf8_cwd_instead_of_panicking() {
        use std::os::unix::ffi::OsStrExt;
        let mut session = ended();
        session.cwd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe"));
        assert!(matches!(session.to_json(), Err(RecordError::Json(_))));
    }

    #[test]
    fn to_json_refuses_non_finite_timestamps() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut session = ended();
            session.created_at = bad;
            assert!(matches!(
                session.to_json(),
                Err(RecordError::NonFiniteTime("created_at"))
            ));
            let mut session = ended();
            session.ended_at = Some(bad);
            assert!(matches!(
                session.to_json(),
                Err(RecordError::NonFiniteTime("ended_at"))
            ));
        }
        let mut session = ended();
        session.created_at = -0.0;
        session.ended_at = Some(f64::MIN_POSITIVE / 2.0);
        assert_eq!(
            Session::from_json(&session.to_json().unwrap()).unwrap(),
            session
        );
    }

    #[test]
    fn non_integer_version_is_invalid_not_missing() {
        for version in ["-1", "1.0", "\"1\"", "true", "null", "[1]", "1e300"] {
            let text = ended()
                .to_json()
                .unwrap()
                .replace("\"version\": 1", &format!("\"version\": {version}"));
            let error = Session::from_json(&text).unwrap_err();
            assert!(
                matches!(&error, RecordError::InvalidVersion(found) if found.starts_with(&version[..2])),
                "{version}: {error:?}"
            );
        }
    }

    #[test]
    fn unsupported_version_message_fits_older_and_newer() {
        let error = RecordError::UnsupportedVersion(0);
        assert_eq!(
            error.to_string(),
            "record version 0 is not supported (this build reads version 1)"
        );
    }

    #[test]
    fn minted_ids_are_distinct_uuids() {
        let ids: HashSet<String> = (0..64).map(|_| SessionId::mint().to_string()).collect();
        assert_eq!(ids.len(), 64);
        for id in &ids {
            assert_eq!(id.len(), 36);
            assert!(id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'));
            assert!(!id.contains(|c: char| c.is_ascii_uppercase()));
        }
    }

    #[test]
    fn session_id_grammar() {
        for ok in ["abc", "a-b_C9", "-", &"x".repeat(MAX_ID_BYTES)] {
            assert_eq!(ok.parse::<SessionId>().unwrap().as_str(), ok, "{ok:?}");
        }
        for bad in [
            "",
            "a/b",
            "..",
            ".",
            "a b",
            "a\n",
            "ä",
            "a.b",
            "/abs",
            &"x".repeat(MAX_ID_BYTES + 1),
        ] {
            assert!(
                matches!(bad.parse::<SessionId>(), Err(InvalidSessionId(found)) if found == bad),
                "{bad:?}"
            );
        }
        assert_eq!(sid("q").to_string(), "q");
        assert!(SessionId::mint().as_str().parse::<SessionId>().is_ok());
    }

    #[test]
    fn from_json_rejects_ids_outside_the_grammar() {
        for bad in ["\"\"", "\"a/b\"", "\"..\"", "\"a b\""] {
            let text = ended()
                .to_json()
                .unwrap()
                .replace("\"id\": \"x\"", &format!("\"id\": {bad}"));
            assert!(
                matches!(Session::from_json(&text), Err(RecordError::Json(_))),
                "{bad}"
            );
            let text = agent()
                .to_json()
                .unwrap()
                .replace("\"parent\": \"p\"", &format!("\"parent\": {bad}"));
            assert!(
                matches!(Session::from_json(&text), Err(RecordError::Json(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn lifecycle_terminal_states() {
        assert!(!Lifecycle::Running.is_terminal());
        assert!(Lifecycle::Exited.is_terminal());
        assert!(Lifecycle::Stopped.is_terminal());
        assert_eq!(Lifecycle::Running.as_str(), "running");
        assert_eq!("stopped".parse::<Lifecycle>().unwrap(), Lifecycle::Stopped);
        assert!("gone".parse::<Lifecycle>().is_err());
    }

    #[test]
    fn valid_names() {
        for name in [
            "a",
            "worker-1",
            "A_B",
            "-x",
            "3",
            "_",
            &"a".repeat(MAX_NAME_BYTES),
        ] {
            assert!(validate_name(name).is_ok(), "{name:?}");
        }
    }

    #[test]
    fn invalid_names() {
        for name in [
            "",
            " ",
            "a b",
            "ä",
            "a/b",
            "a.b",
            "a:b",
            "a\n",
            ".",
            "..",
            "$1",
            &"a".repeat(MAX_NAME_BYTES + 1),
        ] {
            assert!(
                matches!(validate_name(name), Err(CoreError::InvalidName(n)) if n == name),
                "{name:?}"
            );
        }
    }

    #[derive(Debug, PartialEq, Deserialize)]
    struct Kind {
        kind: String,
    }

    #[test]
    fn ext_absent_is_none() {
        let session = ended();
        assert_eq!(session.ext::<serde_json::Value>("views").unwrap(), None);
    }

    #[test]
    fn ext_present_deserializes() {
        let mut session = ended();
        session
            .ext
            .insert("views".into(), serde_json::json!({"kind": "nvim"}));
        assert_eq!(
            session.ext::<Kind>("views").unwrap(),
            Some(Kind {
                kind: "nvim".into()
            })
        );
    }

    #[test]
    fn ext_wrong_shape_is_err() {
        let mut session = ended();
        session.ext.insert("views".into(), serde_json::json!(5));
        assert!(session.ext::<Kind>("views").is_err());
    }
}
