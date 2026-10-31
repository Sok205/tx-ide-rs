//! `TxCore::hook`: the adapter decides, core applies, stamps and logs; core alone ends it.

mod common;

use common::{Harness, agent, process};
use serde_json::json;
use tx_core::{EventType, Hook, State};

fn hook(name: &str, payload: serde_json::Value, at: f64) -> Hook {
    Hook {
        name: name.into(),
        payload,
        at,
    }
}

fn state_lines(h: &Harness) -> Vec<String> {
    h.log_lines()
        .into_iter()
        .filter(|line| line["type"] == EventType::StateChanged.as_str())
        .map(|line| line["msg"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn hooks_move_an_agent_and_core_ends_it() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(agent("a", &h.workdir("a"))).unwrap();
    let status = core.get_session(&id).unwrap().status.unwrap();
    assert_eq!(status.state, State::Starting);

    let status = core
        .hook(&id, hook("idle", json!({}), 10.0))
        .unwrap()
        .unwrap();
    assert_eq!((status.state, status.since), (State::Idle, 10.0));

    // Unknown to the adapter: nothing changes, nothing is logged.
    let status = core
        .hook(&id, hook("bogus", json!({}), 11.0))
        .unwrap()
        .unwrap();
    assert_eq!((status.state, status.since), (State::Idle, 10.0));

    let status = core
        .hook(
            &id,
            hook("waiting", json!({"note": "approval: Bash"}), 12.0),
        )
        .unwrap()
        .unwrap();
    assert_eq!(status.note.as_deref(), Some("approval: Bash"));
    // Same state: the note is replaced, `since` kept, no event.
    let status = core
        .hook(
            &id,
            hook("waiting", json!({"note": "approval: Edit"}), 13.0),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        (status.since, status.note.as_deref()),
        (12.0, Some("approval: Edit"))
    );
    let status = core
        .hook(&id, hook("working", json!({}), 14.0))
        .unwrap()
        .unwrap();
    assert_eq!(
        (status.state, status.note.as_deref()),
        (State::Working, None)
    );
    assert_eq!(core.get_session(&id).unwrap().status.unwrap(), status);

    assert_eq!(
        state_lines(&h),
        [
            "starting → idle",
            "idle → waiting: approval: Bash",
            "waiting → working"
        ]
    );

    let stopped = core.stop_session(&id).unwrap();
    let ended = stopped.status.unwrap();
    assert_eq!(ended.state, State::Ended);
    assert_eq!(Some(ended.since), stopped.ended_at);
    // A stale hook after the end is ignored.
    assert_eq!(core.hook(&id, hook("idle", json!({}), 20.0)).unwrap(), None);
    assert_eq!(
        core.get_session(&id).unwrap().status.unwrap().state,
        State::Ended
    );
    assert_eq!(
        state_lines(&h).len(),
        3,
        "stop logs `stopped`, not a state change"
    );
}

#[test]
fn an_exited_agent_reads_ended() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(agent("a", &h.workdir("a"))).unwrap();
    core.hook(&id, hook("working", json!({}), 10.0)).unwrap();
    let live = core.get_execution_location(&id).unwrap();
    h.tmux_ok(&["kill-session", "-t", &live.session_target]);
    core.refresh_all().unwrap();
    let session = core.get_session(&id).unwrap();
    assert_eq!(session.status.unwrap().state, State::Ended);
}

#[test]
fn process_sessions_have_no_state_and_ignore_hooks() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(process("p", &h.workdir("p"))).unwrap();
    assert_eq!(core.get_session(&id).unwrap().status, None);
    assert_eq!(core.hook(&id, hook("idle", json!({}), 1.0)).unwrap(), None);
    assert_eq!(core.get_session(&id).unwrap().status, None);
}

#[test]
fn a_hook_for_an_unregistered_engine_is_ignored() {
    require_tmux!();
    let h = Harness::new();
    let id = h
        .open_with_engine()
        .create_session(agent("a", &h.workdir("a")))
        .unwrap();
    let bare = h.open();
    assert_eq!(bare.hook(&id, hook("idle", json!({}), 1.0)).unwrap(), None);
    assert_eq!(
        bare.get_session(&id).unwrap().status.unwrap().state,
        State::Starting
    );
}

#[test]
fn a_hook_for_an_unknown_session_is_not_found() {
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = "nope".parse().unwrap();
    assert!(matches!(
        core.hook(&id, hook("idle", json!({}), 1.0)),
        Err(tx_core::CoreError::NotFound(_))
    ));
}
