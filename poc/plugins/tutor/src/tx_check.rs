//! The `tx` check: a predicate over session records.
//!
//! Main's record fields map onto core's as follows. `role` is `llm` (or `agent`): every core
//! record is an agent, so `shell` matches nothing. `state` is any of the names a record
//! answers to: a running record is `alive` and `running`, plus its agent state (`starting` /
//! `idle` / `working`); an ended record is `archived` and `ended`. `chat_origin` needs a
//! chat plugin, which this port does not have yet, so it matches nothing.

use tx_core::{LlmEngineState, Session};

use crate::pack::{SessionFilter, TxCheck};

fn role_matches(_: &Session, role: &str) -> bool {
    matches!(role, "llm" | "agent")
}

fn engine_of(session: &Session) -> &str {
    session.engine.as_str()
}

/// Every state name the record answers to.
pub fn states(session: &Session) -> Vec<String> {
    match session.state {
        LlmEngineState::Ended => vec!["archived".to_owned(), "ended".to_owned()],
        state => vec![
            "alive".to_owned(),
            "running".to_owned(),
            state.as_str().to_owned(),
        ],
    }
}

pub fn matches(filter: &SessionFilter, session: &Session) -> bool {
    filter
        .tag
        .as_ref()
        .is_none_or(|tag| session.tags.iter().any(|have| have.trim() == tag))
        && filter
            .name
            .as_ref()
            .is_none_or(|name| &session.name == name)
        && filter
            .role
            .as_ref()
            .is_none_or(|role| role_matches(session, role))
        && filter
            .engine
            .as_ref()
            .is_none_or(|engine| engine_of(session) == engine)
        && (filter.state.is_empty() || {
            let have = states(session);
            filter.state.iter().any(|state| have.contains(state))
        })
        && filter
            .has_parent
            .is_none_or(|want| session.parent_id.is_some() == want)
        && filter.chat_origin.is_none()
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
            Err(format!(
                "expected no matching sessions, found: {}",
                hits.join(", ")
            ))
        };
    }
    if hits.len() >= check.count {
        return Ok(());
    }
    let mut reason = format!(
        "expected at least {} matching sessions, found {}",
        check.count,
        hits.len()
    );
    if check.session.chat_origin.is_some() {
        reason.push_str("\nchat origins are not tracked by txc yet (no chat plugin); skip this lesson with `txc tutor next`");
    }
    Err(reason)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A record whose `state` is `state` (`"running"` stands for a running record at `idle`;
    /// `"stopped"` and `"exited"` for an ended one).
    fn agent(name: &str, state: &str, tags: &[&str], parent: Option<&str>) -> Session {
        let state = match state {
            "running" => "idle",
            "stopped" | "exited" => "ended",
            other => other,
        };
        Session::from_json(
            &json!({"version": 2, "id": format!("id-{name}"), "name": name, "engine": "claude",
                "working_directory": "/r", "roles": [], "state": state, "created_at": 1.0,
                "last_modified_at": 1.0, "archived_at": null, "tags": tags, "parent_id": parent})
            .to_string(),
        )
        .unwrap()
    }

    fn check(session: SessionFilter) -> TxCheck {
        TxCheck {
            label: "l".into(),
            session,
            count: 1,
            absent: false,
        }
    }

    #[test]
    fn every_given_field_must_match() {
        let worker = agent("w", "running", &["tutor"], Some("p"));
        let server = agent("server", "running", &["tutor", "http-tutor"], None);
        let filter = |f: SessionFilter| matches(&f, &worker);
        assert!(filter(SessionFilter::default()));
        assert!(filter(SessionFilter {
            tag: Some("tutor".into()),
            role: Some("llm".into()),
            engine: Some("claude".into()),
            ..Default::default()
        }));
        assert!(!filter(SessionFilter {
            tag: Some("http-tutor".into()),
            ..Default::default()
        }));
        assert!(filter(SessionFilter {
            state: vec!["working".into(), "alive".into()],
            ..Default::default()
        }));
        assert!(!filter(SessionFilter {
            state: vec!["working".into()],
            ..Default::default()
        }));
        assert!(!filter(SessionFilter {
            state: vec!["archived".into()],
            ..Default::default()
        }));
        assert!(filter(SessionFilter {
            has_parent: Some(true),
            ..Default::default()
        }));
        // Every core record is an agent: the `shell` role of main's tutorials matches nothing.
        assert!(!filter(SessionFilter {
            role: Some("shell".into()),
            ..Default::default()
        }));
        assert!(!matches(
            &SessionFilter {
                engine: Some("codex".into()),
                ..Default::default()
            },
            &server
        ));
        assert!(matches(
            &SessionFilter {
                name: Some("server".into()),
                tag: Some("http-tutor".into()),
                role: Some("agent".into()),
                ..Default::default()
            },
            &server
        ));
    }

    #[test]
    fn the_agent_state_adds_a_state_name() {
        let mut worker = agent("w", "running", &[], None);
        assert_eq!(states(&worker), ["alive", "running", "idle"]);
        worker.state = tx_core::LlmEngineState::Working;
        assert_eq!(states(&worker), ["alive", "running", "working"]);
        assert_eq!(
            states(&agent("x", "stopped", &[], None)),
            ["archived", "ended"]
        );
    }

    #[test]
    fn chat_origin_matches_nothing_and_says_why() {
        let sessions = [agent("w", "running", &["tutor"], None)];
        let fork = check(SessionFilter {
            tag: Some("tutor".into()),
            chat_origin: Some("fork".into()),
            ..Default::default()
        });
        let error = evaluate(&fork, &sessions).unwrap_err();
        assert!(error.contains("no chat plugin"), "{error}");
    }

    #[test]
    fn count_and_absent() {
        let sessions = [
            agent("a", "running", &["tutor"], None),
            agent("b", "exited", &["tutor"], None),
        ];
        let tutor = SessionFilter {
            tag: Some("tutor".into()),
            ..Default::default()
        };
        assert_eq!(evaluate(&check(tutor.clone()), &sessions), Ok(()));
        let three = TxCheck {
            count: 3,
            ..check(tutor)
        };
        assert_eq!(
            evaluate(&three, &sessions),
            Err("expected at least 3 matching sessions, found 2".into())
        );
        let live = SessionFilter {
            tag: Some("tutor".into()),
            state: vec!["alive".into(), "working".into()],
            ..Default::default()
        };
        let absent = TxCheck {
            absent: true,
            ..check(live)
        };
        assert_eq!(
            evaluate(&absent, &sessions),
            Err("expected no matching sessions, found: a".into())
        );
        assert_eq!(evaluate(&absent, &sessions[1..]), Ok(()));
    }
}
