//! The `tx` check: a predicate over session records.
//!
//! Main's record fields map onto core's as follows. `role` is the launch kind: `llm` (or
//! `agent`) for an engine launch, `shell` (or `process`) for a plain program. `state` is any of
//! the names a record answers to: a running record is `alive` and `running`, plus the hooks
//! plugin's `ext.hooks.state` (`working` / `waiting` / `idle`) when it holds a valid one; an
//! ended record is `archived` and its lifecycle (`exited` / `stopped`). `chat_origin` needs a
//! chat plugin, which this port does not have yet, so it matches nothing.

use serde::Deserialize;
use tx_core::{Launch, Lifecycle, Session};

use crate::pack::{SessionFilter, TxCheck};

const HOOKS_PLUGIN: &str = "hooks";

#[derive(Deserialize)]
struct HookState {
    state: String,
}

fn role_matches(session: &Session, role: &str) -> bool {
    match &session.launch {
        Launch::Agent { .. } => matches!(role, "llm" | "agent"),
        Launch::Process { .. } => matches!(role, "shell" | "process"),
    }
}

fn engine_of(session: &Session) -> Option<&str> {
    match &session.launch {
        Launch::Agent { engine, .. } => Some(engine),
        Launch::Process { .. } => None,
    }
}

/// Every state name the record answers to. A malformed `ext.hooks` is ignored, never an error.
pub fn states(session: &Session) -> Vec<String> {
    match session.lifecycle {
        Lifecycle::Running => {
            let mut names = vec!["alive".to_owned(), "running".to_owned()];
            if let Ok(Some(hook)) = session.ext::<HookState>(HOOKS_PLUGIN) {
                names.push(hook.state);
            }
            names
        }
        ended => vec!["archived".to_owned(), ended.as_str().to_owned()],
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
            .is_none_or(|engine| engine_of(session) == Some(engine.as_str()))
        && (filter.state.is_empty() || {
            let have = states(session);
            filter.state.iter().any(|state| have.contains(state))
        })
        && filter
            .has_parent
            .is_none_or(|want| session.parent.is_some() == want)
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
    use serde_json::{Value, json};

    use super::*;

    fn record(
        name: &str,
        launch: Value,
        lifecycle: &str,
        tags: &[&str],
        parent: Option<&str>,
    ) -> Session {
        Session::from_json(
            &json!({"version": 1, "id": format!("id-{name}"), "name": name, "parent": parent,
                "tags": tags, "cwd": "/r", "created_at": 1.0, "ended_at": null,
                "lifecycle": lifecycle, "launch": launch})
            .to_string(),
        )
        .unwrap()
    }

    fn agent(name: &str, lifecycle: &str, tags: &[&str], parent: Option<&str>) -> Session {
        let launch = json!({"kind": "agent", "engine": "claude", "profile": [], "model": null, "effort": null});
        record(name, launch, lifecycle, tags, parent)
    }

    fn shell(name: &str, tags: &[&str]) -> Session {
        let launch = json!({"kind": "process", "program": "sh", "args": []});
        record(name, launch, "running", tags, None)
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
        let server = shell("server", &["tutor", "http-tutor"]);
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
            state: vec!["archived".into()],
            ..Default::default()
        }));
        assert!(filter(SessionFilter {
            has_parent: Some(true),
            ..Default::default()
        }));
        assert!(!filter(SessionFilter {
            role: Some("shell".into()),
            ..Default::default()
        }));
        assert!(!matches(
            &SessionFilter {
                engine: Some("claude".into()),
                ..Default::default()
            },
            &server
        ));
        assert!(matches(
            &SessionFilter {
                name: Some("server".into()),
                tag: Some("http-tutor".into()),
                role: Some("shell".into()),
                ..Default::default()
            },
            &server
        ));
    }

    #[test]
    fn hook_state_adds_a_state_name_and_a_malformed_one_is_ignored() {
        let mut worker = agent("w", "running", &[], None);
        worker
            .ext
            .insert("hooks".into(), json!({"state": "working", "since": 1.0}));
        assert_eq!(states(&worker), ["alive", "running", "working"]);
        worker.ext.insert("hooks".into(), json!("garbage"));
        assert_eq!(states(&worker), ["alive", "running"]);
        assert_eq!(
            states(&agent("x", "stopped", &[], None)),
            ["archived", "stopped"]
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
