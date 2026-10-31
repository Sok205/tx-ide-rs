//! The `ext["chat"]` and `ext["history"]` shapes. Unknown fields survive a read-modify-write
//! through each struct's `rest` map.

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tx_core::{Effort, Launch, ListFilter, Session, SessionId, TxCore};

use crate::{CHAT_ID, ChatError, HISTORY_ID};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Primary,
    Fork,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub session: String,
    pub chat: String,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatRef {
    #[serde(default)]
    pub id: Option<String>,
    pub role: Role,
    pub how: String,
    pub since: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentLaunch {
    pub engine: String,
    #[serde(default)]
    pub profile: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatExt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<AgentLaunch>,
    #[serde(default)]
    pub chats: Vec<ChatRef>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryChat {
    pub transcript: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<String>,
    pub at: f64,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HistoryExt {
    #[serde(default)]
    pub chats: BTreeMap<String, HistoryChat>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

fn read_ext<T: DeserializeOwned + Default>(s: &Session, plugin: &str) -> Result<T, ChatError> {
    match s.ext.get(plugin) {
        None | Some(Value::Null) => Ok(T::default()),
        Some(value) => {
            serde_json::from_value(value.clone()).map_err(|error| ChatError::Malformed {
                plugin: plugin.to_owned(),
                name: s.name.clone(),
                reason: error.to_string(),
            })
        }
    }
}

/// Absent is the default; a value of the wrong shape is `Malformed`.
pub fn chat_ext(s: &Session) -> Result<ChatExt, ChatError> {
    read_ext(s, CHAT_ID)
}

pub fn history_ext(s: &Session) -> Result<HistoryExt, ChatError> {
    read_ext(s, HISTORY_ID)
}

/// The record's own agent launch, else the one `ext.chat` remembers.
pub fn agent_launch(s: &Session, ext: &ChatExt) -> Option<AgentLaunch> {
    match &s.launch {
        Launch::Agent {
            engine,
            profile,
            model,
            effort,
            ..
        } => Some(AgentLaunch {
            engine: engine.clone(),
            profile: profile.clone(),
            model: model.clone(),
            effort: *effort,
            rest: Map::new(),
        }),
        Launch::Process { .. } => ext.launch.clone(),
    }
}

/// The chat an op targets: the last ref with an id, preferring one still open.
pub fn active_chat(ext: &ChatExt) -> Option<&ChatRef> {
    let with_id = || ext.chats.iter().filter(|chat| chat.id.is_some());
    with_id()
        .rev()
        .find(|chat| chat.ended_at.is_none())
        .or_else(|| with_id().next_back())
}

/// Whether another record already lists `chat_id` (in `ext.chat.chats[].id` or
/// `ext.history.chats`), unless `me` lists it itself (a resume). A malformed ext on another
/// record counts as "not listed".
pub fn owned_elsewhere(core: &TxCore, me: &SessionId, chat_id: &str) -> Result<bool, ChatError> {
    let in_chat = |s: &Session| {
        chat_ext(s).is_ok_and(|ext| ext.chats.iter().any(|c| c.id.as_deref() == Some(chat_id)))
    };
    let in_history = |s: &Session| history_ext(s).is_ok_and(|ext| ext.chats.contains_key(chat_id));
    let mut elsewhere = false;
    for id in core.list_sessions(&ListFilter::new())? {
        let session = match core.get_session(&id) {
            Ok(session) => session,
            Err(tx_core::CoreError::NotFound(_)) => continue,
            Err(error) => return Err(error.into()),
        };
        if &id == me {
            if in_chat(&session) {
                return Ok(false);
            }
        } else if in_chat(&session) || in_history(&session) {
            elsewhere = true;
        }
    }
    Ok(elsewhere)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tx_core::{CoreConfig, SessionStore};

    use super::*;

    fn session(id: &str, name: &str, ext: Value) -> Session {
        serde_json::from_value(json!({
            "id": id, "name": name, "parent": null, "tags": [], "cwd": "/tmp",
            "created_at": 1.0, "ended_at": null, "last_modified_at": 1.0, "lifecycle": "running",
            "launch": {"kind": "process", "program": "sh", "args": []},
            "ext": ext,
        }))
        .unwrap()
    }

    fn chat_ref(id: Option<&str>, ended_at: Option<f64>) -> ChatRef {
        ChatRef {
            id: id.map(str::to_owned),
            role: Role::Primary,
            how: "spawn".into(),
            since: 1.0,
            ended_at,
            origin: None,
            transcript: None,
            rest: Map::new(),
        }
    }

    #[test]
    fn chat_ext_absent_is_default() {
        let s = session("a", "a", json!({}));
        assert_eq!(chat_ext(&s).unwrap(), ChatExt::default());
        assert_eq!(history_ext(&s).unwrap(), HistoryExt::default());
    }

    #[test]
    fn chat_ext_malformed_is_an_error() {
        let s = session("a", "alice", json!({"chat": 5}));
        let error = chat_ext(&s).unwrap_err();
        assert!(matches!(error, ChatError::Malformed { .. }), "{error}");
        assert!(
            error
                .to_string()
                .starts_with("ext.chat of 'alice' is malformed: ")
        );
        assert!(history_ext(&s).is_ok());
    }

    #[test]
    fn unknown_fields_round_trip() {
        let s = session(
            "a",
            "a",
            json!({"chat": {"future": 1, "chats": [
                {"id": "c", "role": "fork", "how": "fork", "since": 2.0, "extra": [1]}]}}),
        );
        let ext = chat_ext(&s).unwrap();
        assert_eq!(ext.chats[0].role, Role::Fork);
        let back = serde_json::to_value(&ext).unwrap();
        assert_eq!(back["future"], 1);
        assert_eq!(back["chats"][0]["extra"], json!([1]));
    }

    #[test]
    fn active_chat_prefers_open_then_last() {
        let mut ext = ChatExt::default();
        assert!(active_chat(&ext).is_none());
        ext.chats = vec![
            chat_ref(Some("a"), None),
            chat_ref(Some("b"), Some(5.0)),
            chat_ref(None, None),
        ];
        assert_eq!(active_chat(&ext).unwrap().id.as_deref(), Some("a"));
        ext.chats[0].ended_at = Some(3.0);
        assert_eq!(active_chat(&ext).unwrap().id.as_deref(), Some("b"));
        ext.chats.push(chat_ref(Some("c"), None));
        assert_eq!(active_chat(&ext).unwrap().id.as_deref(), Some("c"));
    }

    #[test]
    fn agent_launch_prefers_the_record() {
        let ext = ChatExt {
            launch: Some(AgentLaunch {
                engine: "codex".into(),
                profile: vec![],
                model: None,
                effort: None,
                rest: Map::new(),
            }),
            ..ChatExt::default()
        };
        let process = session("a", "a", json!({}));
        assert_eq!(agent_launch(&process, &ext).unwrap().engine, "codex");
        assert!(agent_launch(&process, &ChatExt::default()).is_none());
        let mut agent = process;
        agent.launch = Launch::Agent {
            engine: "claude".into(),
            profile: vec!["r".into()],
            model: Some("opus".into()),
            effort: Some(Effort::Low),
            resume: None,
        };
        let launch = agent_launch(&agent, &ext).unwrap();
        assert_eq!(launch.engine, "claude");
        assert_eq!(launch.effort, Some(Effort::Low));
    }

    #[test]
    fn owned_elsewhere_sees_chat_and_history_refs_but_not_own_resume() {
        let dir = tempfile::tempdir().unwrap();
        let core = TxCore::open(CoreConfig::new(dir.path())).unwrap();
        let store = SessionStore::new(dir.path().join("sessions"));
        let chat = json!({"chat": {"chats": [
            {"id": "c1", "role": "primary", "how": "spawn", "since": 1.0}]}});
        let history = json!({"history": {"chats": {
            "c2": {"transcript": "/t", "at": 1.0}}}});
        let records = [
            session("s-chat", "chatty", chat.clone()),
            session("s-hist", "histy", history),
            session("s-bad", "bad", json!({"chat": 5, "history": "x"})),
        ];
        for record in &records {
            store.save(record).unwrap();
        }
        let id = |s: &str| s.parse::<SessionId>().unwrap();
        let stranger = id("s-new");
        assert!(owned_elsewhere(&core, &stranger, "c1").unwrap());
        assert!(owned_elsewhere(&core, &stranger, "c2").unwrap());
        assert!(!owned_elsewhere(&core, &stranger, "c3").unwrap());
        assert!(
            !owned_elsewhere(&core, &id("s-chat"), "c1").unwrap(),
            "own resume"
        );
        assert!(!owned_elsewhere(&core, &id("s-hist"), "c2").unwrap());
        assert!(owned_elsewhere(&core, &id("s-hist"), "c1").unwrap());
    }
}
