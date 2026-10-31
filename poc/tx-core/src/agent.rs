//! The session record: what core stores on disk for every managed agent.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::engine::EngineKind;
use crate::error::CoreError;
use crate::state::LlmEngineState;

/// The record schema this build reads and writes. Version 1 (one `sessions/<id>.json` per
/// record, with `lifecycle` and `launch`) is upgraded by `txc migrate`, never read in place.
pub const RECORD_VERSION: u64 = 2;
/// Agent names are tmux session names and file-name fragments; keep them short.
pub const MAX_NAME_BYTES: usize = 128;
/// Ids are uuids (36 bytes); the slack lets other minting schemes in without a format change.
pub const MAX_ID_BYTES: usize = 64;

/// A record id: a uuid v4 minted by `create_agent`. Every way to build one validates the
/// grammar `[A-Za-z0-9_-]{1,64}` (parsing, deserializing), so an id is always a safe file-name
/// fragment and tmux option value: `agents/<id>/`, `<id>.lock`, `@tx-id`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

impl TryFrom<String> for AgentId {
    type Error = InvalidAgentId;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        id.parse()
    }
}

impl AgentId {
    pub fn mint() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid session id {0:?} (allowed: [A-Za-z0-9_-]+, at most {MAX_ID_BYTES} bytes)")]
pub struct InvalidAgentId(pub String);

impl FromStr for AgentId {
    type Err = InvalidAgentId;

    fn from_str(id: &str) -> Result<Self, Self::Err> {
        if is_safe_fragment(id, MAX_ID_BYTES) {
            Ok(Self(id.to_owned()))
        } else {
            Err(InvalidAgentId(id.to_owned()))
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

/// The snapshot every read returns: `<data>/agents/<id>/agent.json`, written atomically under
/// the agent's lock. Disk is the record; tmux holds only the `@tx-id` link.
///
/// An agent is running while `state` is not `Ended`; `Ended` is the terminal state (the
/// process exited or was stopped; the event log says which). There is no separate lifecycle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: AgentId,
    pub name: String,
    /// The engine that runs it, one of the closed [`EngineKind`]s.
    pub engine: EngineKind,
    /// The agent's worktree.
    pub working_directory: PathBuf,
    /// The role names its profile was built from.
    pub roles: Vec<String>,
    /// The agent's state as its engine adapter reported it. Core-owned.
    pub state: LlmEngineState,
    pub created_at: f64,
    /// When the record was last written; core stamps it on every save.
    pub last_modified_at: f64,
    /// When the agent was archived: hidden from listings and its name freed, with nothing
    /// deleted. `None` for an unarchived record; a respawn clears it.
    #[serde(default)]
    pub archived_at: Option<f64>,
    pub tags: Vec<String>,
    /// Ancestry only; core never schedules or stops children.
    #[serde(default)]
    pub parent_id: Option<AgentId>,
    /// Plugin- and engine-owned data keyed by plugin id or `ext_key`; written through
    /// `TxCore::update_ext` and the engine hooks. Core never reads it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ext: BTreeMap<String, Value>,
    /// Keys this build does not know; kept so a newer build's fields survive a rewrite.
    #[serde(flatten)]
    pub other: Map<String, Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("record version {0} is not supported (this build reads version {RECORD_VERSION})")]
    UnsupportedVersion(u64),
    /// An older record this build knows how to upgrade, but only through `txc migrate`: the
    /// mapping needs the engine plugins and moves files.
    #[error("record version {0}: run `txc migrate`")]
    NeedsMigration(u64),
    #[error("record version {0} is not an unsigned integer")]
    InvalidVersion(String),
    #[error("record has no version field")]
    MissingVersion,
    #[error("{0} is not a finite number")]
    NonFiniteTime(&'static str),
    /// The record names an engine that is not an [`EngineKind`]: written by hand or by a build
    /// with another engine set. The store skips it and names it.
    #[error("names engine {0:?}, which this build does not know (known: {known})", known = EngineKind::names())]
    UnknownEngine(String),
}

#[derive(Serialize)]
struct RecordOut<'a> {
    version: u64,
    #[serde(flatten)]
    session: &'a Agent,
}

impl Agent {
    /// Pretty JSON, newline-terminated, `version` first. Fails for what JSON cannot carry: a
    /// non-UTF-8 `working_directory` or a non-finite timestamp (serde_json would write `null`,
    /// which `from_json` then refuses — better to refuse before the record exists).
    pub fn to_json(&self) -> Result<String, RecordError> {
        if !self.created_at.is_finite() {
            return Err(RecordError::NonFiniteTime("created_at"));
        }
        if !self.last_modified_at.is_finite() {
            return Err(RecordError::NonFiniteTime("last_modified_at"));
        }
        if self.archived_at.is_some_and(|at| !at.is_finite()) {
            return Err(RecordError::NonFiniteTime("archived_at"));
        }
        let record = RecordOut {
            version: RECORD_VERSION,
            session: self,
        };
        let mut text = serde_json::to_string_pretty(&record)?;
        text.push('\n');
        Ok(text)
    }

    /// Version 2 only. A version-1 record is `NeedsMigration`; any other version is
    /// `UnsupportedVersion`. Keys this build does not know land in `other`.
    pub fn from_json(text: &str) -> Result<Self, RecordError> {
        let mut value: Map<String, Value> = serde_json::from_str(text)?;
        match value.remove("version") {
            None => return Err(RecordError::MissingVersion),
            Some(Value::Number(version)) if version.as_u64() == Some(RECORD_VERSION) => {}
            Some(Value::Number(version)) => {
                return Err(match version.as_u64() {
                    Some(1) => RecordError::NeedsMigration(1),
                    Some(other) => RecordError::UnsupportedVersion(other),
                    None => RecordError::InvalidVersion(version.to_string()),
                });
            }
            Some(other) => return Err(RecordError::InvalidVersion(other.to_string())),
        }
        if let Some(Value::String(engine)) = value.get("engine")
            && engine.parse::<EngineKind>().is_err()
        {
            return Err(RecordError::UnknownEngine(engine.clone()));
        }
        Ok(serde_json::from_value(Value::Object(value))?)
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

    fn sid(id: &str) -> AgentId {
        id.parse().unwrap()
    }

    fn agent() -> Agent {
        Agent {
            id: sid("b0000000-0000-4000-8000-000000000001"),
            name: "worker-1".into(),
            engine: EngineKind::Claude,
            working_directory: PathBuf::from("/tmp/w"),
            roles: vec!["COMMON".into(), "dev".into()],
            state: LlmEngineState::Working,
            created_at: 1727000000.5,
            last_modified_at: 1727000000.5,
            archived_at: None,
            tags: vec!["a".into(), "b".into()],
            parent_id: Some(sid("p")),
            ext: BTreeMap::new(),
            other: Map::new(),
        }
    }

    /// An agent that was stopped.
    fn ended() -> Agent {
        Agent {
            id: sid("x"),
            name: "done".into(),
            engine: EngineKind::Codex,
            working_directory: PathBuf::from("/"),
            roles: vec![],
            state: LlmEngineState::Ended,
            created_at: 1.0,
            last_modified_at: 2.0,
            archived_at: Some(3.0),
            tags: vec![],
            parent_id: None,
            ext: BTreeMap::new(),
            other: Map::new(),
        }
    }

    const VERSION_ONE: &str = r#"{"version":1,"id":"i","name":"n","parent":null,"tags":[],"cwd":"/","created_at":1.0,"ended_at":null,"lifecycle":"running","launch":{"kind":"agent","engine":"claude","profile":[]}}"#;

    #[test]
    fn agent_round_trips_through_json() {
        let text = agent().to_json().unwrap();
        assert_eq!(Agent::from_json(&text).unwrap(), agent());
    }

    #[test]
    fn an_ended_agent_round_trips_through_json() {
        let text = ended().to_json().unwrap();
        assert_eq!(Agent::from_json(&text).unwrap(), ended());
    }

    #[test]
    fn json_shape_is_the_documented_one() {
        let text = agent().to_json().unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["version"], RECORD_VERSION);
        assert_eq!(value["engine"], "claude");
        assert_eq!(value["working_directory"], "/tmp/w");
        assert_eq!(value["roles"], serde_json::json!(["COMMON", "dev"]));
        assert_eq!(value["state"], "working");
        assert_eq!(value["parent_id"], "p");
        assert_eq!(value["archived_at"], Value::Null);
        for gone in ["lifecycle", "launch", "ended_at", "cwd", "parent", "ext"] {
            assert!(value.get(gone).is_none(), "{gone}: {text}");
        }
        let keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "version",
                "id",
                "name",
                "engine",
                "working_directory",
                "roles",
                "state",
                "created_at",
                "last_modified_at",
                "archived_at",
                "tags",
                "parent_id",
            ]
        );
        let value: Value = serde_json::from_str(&ended().to_json().unwrap()).unwrap();
        assert_eq!(value["engine"], "codex");
        assert_eq!(value["state"], "ended");
        assert_eq!(value["archived_at"], 3.0);
        assert_eq!(value["parent_id"], Value::Null);
    }

    #[test]
    fn json_is_pretty_and_newline_terminated() {
        let text = ended().to_json().unwrap();
        assert!(text.ends_with("}\n"), "{text:?}");
        assert!(
            text.starts_with("{\n  \"version\": 2,\n  \"id\""),
            "{text:?}"
        );
    }

    #[test]
    fn optional_fields_may_be_null_or_absent() {
        let text = r#"{"version":2,"id":"i","name":"n","engine":"claude","working_directory":"/","roles":[],"state":"idle","created_at":1.0,"last_modified_at":1.0,"archived_at":null,"tags":[],"parent_id":null}"#;
        let session = Agent::from_json(text).unwrap();
        assert_eq!(session.parent_id, None);
        assert_eq!(session.archived_at, None);
        assert!(session.ext.is_empty() && session.other.is_empty());
        let text = text
            .replace(r#","archived_at":null"#, "")
            .replace(r#","parent_id":null"#, "");
        assert_eq!(Agent::from_json(&text).unwrap(), session);
    }

    /// Acceptance 5: a key this build does not know survives a read and a rewrite, in place.
    #[test]
    fn unknown_keys_round_trip_through_other() {
        let mut text = ended().to_json().unwrap();
        text.insert_str(
            text.find("\"id\"").unwrap(),
            "\"future_field\": 1,\n  \"future\": {\"x\": [1, 2]},\n  ",
        );
        let session = Agent::from_json(&text).unwrap();
        assert_eq!(session.other["future_field"], 1);
        assert_eq!(session.other["future"], serde_json::json!({"x": [1, 2]}));
        let mut expected = ended();
        expected.other = session.other.clone();
        assert_eq!(session, expected);
        let rewritten: Value = serde_json::from_str(&session.to_json().unwrap()).unwrap();
        assert_eq!(rewritten["future_field"], 1);
        assert_eq!(rewritten["future"]["x"][1], 2);
        assert_eq!(rewritten["version"], RECORD_VERSION);
        assert_eq!(
            Agent::from_json(&session.to_json().unwrap()).unwrap(),
            session
        );
    }

    /// Acceptance 1: a version-1 record is refused by name, with the pointer to `txc migrate`,
    /// before any of its fields is looked at (a process record, an unknown engine included).
    #[test]
    fn version_one_needs_migration() {
        let error = Agent::from_json(VERSION_ONE).unwrap_err();
        assert!(matches!(error, RecordError::NeedsMigration(1)), "{error:?}");
        assert_eq!(error.to_string(), "record version 1: run `txc migrate`");
        let process = VERSION_ONE.replace(
            r#"{"kind":"agent","engine":"claude","profile":[]}"#,
            r#"{"kind":"process","program":"sh","args":[]}"#,
        );
        assert!(matches!(
            Agent::from_json(&process),
            Err(RecordError::NeedsMigration(1))
        ));
        assert!(matches!(
            Agent::from_json(&VERSION_ONE.replace("\"claude\"", "\"kodex\"")),
            Err(RecordError::NeedsMigration(1))
        ));
        assert!(matches!(
            Agent::from_json(r#"{"version":1}"#),
            Err(RecordError::NeedsMigration(1))
        ));
    }

    #[test]
    fn newer_and_older_versions_are_refused() {
        let text = ended()
            .to_json()
            .unwrap()
            .replace("\"version\": 2", "\"version\": 3");
        assert!(matches!(
            Agent::from_json(&text),
            Err(RecordError::UnsupportedVersion(3))
        ));
        let text = ended()
            .to_json()
            .unwrap()
            .replace("\"version\": 2", "\"version\": 0");
        assert!(matches!(
            Agent::from_json(&text),
            Err(RecordError::UnsupportedVersion(0))
        ));
    }

    #[test]
    fn missing_version_is_refused() {
        let text = ended().to_json().unwrap().replace("\"version\": 2,\n", "");
        assert!(matches!(
            Agent::from_json(&text),
            Err(RecordError::MissingVersion)
        ));
    }

    /// A version-2 record naming an engine this build lacks is refused by name, so the store
    /// can skip it; one whose engine is not even a string is a plain JSON error.
    #[test]
    fn an_unknown_engine_is_refused_by_name() {
        let text = ended().to_json().unwrap().replace("\"codex\"", "\"kodex\"");
        let error = Agent::from_json(&text).unwrap_err();
        assert!(
            matches!(&error, RecordError::UnknownEngine(engine) if engine == "kodex"),
            "{error:?}"
        );
        assert!(error.to_string().contains("claude, codex"), "{error}");
        let text = ended().to_json().unwrap().replace("\"codex\"", "7");
        assert!(matches!(Agent::from_json(&text), Err(RecordError::Json(_))));
    }

    #[test]
    fn garbage_is_a_json_error() {
        assert!(matches!(Agent::from_json("{"), Err(RecordError::Json(_))));
        assert!(matches!(Agent::from_json("[]"), Err(RecordError::Json(_))));
        assert!(matches!(
            Agent::from_json(r#"{"version":2}"#),
            Err(RecordError::Json(_))
        ));
    }

    #[test]
    fn to_json_refuses_a_non_utf8_working_directory_instead_of_panicking() {
        use std::os::unix::ffi::OsStrExt;
        let mut session = ended();
        session.working_directory = PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff\xfe"));
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
            session.last_modified_at = bad;
            assert!(matches!(
                session.to_json(),
                Err(RecordError::NonFiniteTime("last_modified_at"))
            ));
            let mut session = ended();
            session.archived_at = Some(bad);
            assert!(matches!(
                session.to_json(),
                Err(RecordError::NonFiniteTime("archived_at"))
            ));
        }
        let mut session = ended();
        session.created_at = -0.0;
        session.archived_at = Some(f64::MIN_POSITIVE / 2.0);
        assert_eq!(
            Agent::from_json(&session.to_json().unwrap()).unwrap(),
            session
        );
    }

    #[test]
    fn non_integer_version_is_invalid_not_missing() {
        for version in ["-1", "1.0", "\"1\"", "true", "null", "[1]", "1e300"] {
            let text = ended()
                .to_json()
                .unwrap()
                .replace("\"version\": 2", &format!("\"version\": {version}"));
            let error = Agent::from_json(&text).unwrap_err();
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
            "record version 0 is not supported (this build reads version 2)"
        );
    }

    #[test]
    fn minted_ids_are_distinct_uuids() {
        let ids: HashSet<String> = (0..64).map(|_| AgentId::mint().to_string()).collect();
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
            assert_eq!(ok.parse::<AgentId>().unwrap().as_str(), ok, "{ok:?}");
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
                matches!(bad.parse::<AgentId>(), Err(InvalidAgentId(found)) if found == bad),
                "{bad:?}"
            );
        }
        assert_eq!(sid("q").to_string(), "q");
        assert!(AgentId::mint().as_str().parse::<AgentId>().is_ok());
    }

    #[test]
    fn from_json_rejects_ids_outside_the_grammar() {
        for bad in ["\"\"", "\"a/b\"", "\"..\"", "\"a b\""] {
            let text = ended()
                .to_json()
                .unwrap()
                .replace("\"id\": \"x\"", &format!("\"id\": {bad}"));
            assert!(
                matches!(Agent::from_json(&text), Err(RecordError::Json(_))),
                "{bad}"
            );
            let text = agent()
                .to_json()
                .unwrap()
                .replace("\"parent_id\": \"p\"", &format!("\"parent_id\": {bad}"));
            assert!(
                matches!(Agent::from_json(&text), Err(RecordError::Json(_))),
                "{bad}"
            );
        }
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
        assert_eq!(session.ext::<Value>("views").unwrap(), None);
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
