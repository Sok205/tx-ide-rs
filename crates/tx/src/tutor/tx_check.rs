//! The `tx` check: a predicate over session records.

use crate::session::Session;
use crate::tutor::pack::{SessionFilter, TxCheck};

pub fn matches(filter: &SessionFilter, session: &Session) -> bool {
    filter
        .tag
        .as_ref()
        .is_none_or(|tag| session.tags.iter().any(|have| have.trim() == tag))
        && filter.name.as_ref().is_none_or(|name| &session.name == name)
        && filter.role.as_ref().is_none_or(|role| session.role().as_str() == role)
        && filter.engine.as_ref().is_none_or(|engine| {
            session.llm().is_some_and(|llm| llm.engine.as_str() == engine)
        })
        && (filter.state.is_empty()
            || filter.state.iter().any(|state| state == session.state.as_str()))
        && filter
            .has_parent
            .is_none_or(|want| session.parent.is_some() == want)
        && filter.chat_origin.as_ref().is_none_or(|how| {
            session.chats().iter().any(|chat| &chat.origin.how == how)
        })
}

pub fn evaluate(check: &TxCheck, sessions: &[Session]) -> Result<(), String> {
    let hits: Vec<&str> = sessions
        .iter()
        .filter(|session| matches(&check.session, session))
        .map(|session| session.name.as_str())
        .collect();
    if check.absent {
        return if hits.is_empty() {
            Ok(())
        } else {
            Err(format!("expected no matching sessions, found: {}", hits.join(", ")))
        };
    }
    if hits.len() >= check.count {
        Ok(())
    } else {
        Err(format!(
            "expected at least {} matching sessions, found {}",
            check.count,
            hits.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::tutor::pack::SessionFilter;

    fn llm(name: &str, state: &str, tags: &[&str], parent: Option<&str>, how: Option<&str>) -> Session {
        let chats: Vec<Value> = how
            .map(|how| json!({"id": null, "role": how, "cwd": "/r", "transcript_path": "/t",
                "origin": {"how": how, "session_id": "s0", "chat_id": "c0"}, "bundle_path": null,
                "started_at": 1.0, "ended_at": null, "summary": "", "engine": "claude"}))
            .into_iter()
            .collect();
        Session::from_value(&json!({"schema_version": 6, "id": format!("id-{name}"), "name": name,
            "role": "llm", "state": state, "cwd": "/r", "cmd": "claude", "tags": tags, "group": null,
            "env": {}, "parent": parent, "pid": null, "attached_to": [], "created_at": 1.0,
            "ended_at": null, "engine": "claude", "last_activity": null, "chats": chats,
            "turn_started_at": null}))
        .unwrap()
    }

    fn shell(name: &str, tags: &[&str]) -> Session {
        Session::from_value(&json!({"schema_version": 6, "id": format!("id-{name}"), "name": name,
            "role": "shell", "state": "alive", "cwd": "/r", "cmd": "", "tags": tags, "group": null,
            "env": {}, "parent": null, "pid": null, "attached_to": [], "created_at": 1.0,
            "ended_at": null, "artifact_id": null}))
        .unwrap()
    }

    fn check(session: SessionFilter) -> TxCheck {
        TxCheck { label: "l".into(), session, count: 1, absent: false }
    }

    #[test]
    fn every_given_field_must_match() {
        let worker = llm("w", "idle", &["tutor"], Some("p"), Some("fork"));
        let server = shell("server", &["tutor", "http-tutor"]);
        let filter = |f: SessionFilter| matches(&f, &worker);
        assert!(filter(SessionFilter::default()));
        assert!(filter(SessionFilter { tag: Some("tutor".into()), role: Some("llm".into()), engine: Some("claude".into()), ..Default::default() }));
        assert!(!filter(SessionFilter { tag: Some("http-tutor".into()), ..Default::default() }));
        assert!(filter(SessionFilter { state: vec!["working".into(), "idle".into()], ..Default::default() }));
        assert!(!filter(SessionFilter { state: vec!["archived".into()], ..Default::default() }));
        assert!(filter(SessionFilter { has_parent: Some(true), chat_origin: Some("fork".into()), ..Default::default() }));
        assert!(!filter(SessionFilter { chat_origin: Some("handover".into()), ..Default::default() }));
        assert!(!matches(&SessionFilter { engine: Some("claude".into()), ..Default::default() }, &server));
        assert!(matches(&SessionFilter { name: Some("server".into()), tag: Some("http-tutor".into()), ..Default::default() }, &server));
    }

    #[test]
    fn count_and_absent() {
        let sessions = [llm("a", "idle", &["tutor"], None, None), llm("b", "archived", &["tutor"], None, None)];
        let tutor = SessionFilter { tag: Some("tutor".into()), ..Default::default() };
        assert_eq!(evaluate(&check(tutor.clone()), &sessions), Ok(()));
        let two = TxCheck { count: 3, ..check(tutor) };
        assert_eq!(evaluate(&two, &sessions), Err("expected at least 3 matching sessions, found 2".into()));
        let live = SessionFilter { tag: Some("tutor".into()), state: vec!["idle".into(), "working".into()], ..Default::default() };
        let absent = TxCheck { absent: true, ..check(live) };
        assert_eq!(evaluate(&absent, &sessions), Err("expected no matching sessions, found: a".into()));
        assert_eq!(evaluate(&absent, &sessions[1..]), Ok(()));
    }
}
