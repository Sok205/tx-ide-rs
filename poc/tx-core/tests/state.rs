//! `TxCore::hook`: the adapter decides, core writes and logs; core alone ends it.

mod common;

use common::{Harness, agent, process};
use serde_json::json;
use tx_core::{EventType, LlmEngineHook, LlmEngineState};

fn hook(name: &str, payload: serde_json::Value, at: f64) -> LlmEngineHook {
    LlmEngineHook {
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
    let state = |core: &tx_core::TxCore| core.get_session(&id).unwrap().state;
    assert_eq!(state(&core), Some(LlmEngineState::Starting));

    let moved = core.hook(&id, hook("idle", json!({}), 10.0)).unwrap();
    assert_eq!(moved, Some(LlmEngineState::Idle));
    // Unknown to the adapter: nothing changes, nothing is logged.
    let unchanged = core.hook(&id, hook("bogus", json!({}), 11.0)).unwrap();
    assert_eq!(unchanged, Some(LlmEngineState::Idle));
    // The same state again: nothing is written or logged.
    core.hook(&id, hook("idle", json!({}), 12.0)).unwrap();
    core.hook(&id, hook("working", json!({}), 13.0)).unwrap();
    core.hook(&id, hook("idle", json!({}), 14.0)).unwrap();
    assert_eq!(state(&core), Some(LlmEngineState::Idle));

    assert_eq!(
        state_lines(&h),
        ["starting → idle", "idle → working", "working → idle"]
    );

    let stopped = core.stop_session(&id).unwrap();
    assert_eq!(stopped.state, Some(LlmEngineState::Ended));
    // A stale hook after the end is ignored.
    assert_eq!(core.hook(&id, hook("idle", json!({}), 20.0)).unwrap(), None);
    assert_eq!(state(&core), Some(LlmEngineState::Ended));
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
    assert_eq!(session.state, Some(LlmEngineState::Ended));
}

#[test]
fn process_sessions_have_no_state_and_ignore_hooks() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(process("p", &h.workdir("p"))).unwrap();
    assert_eq!(core.get_session(&id).unwrap().state, None);
    assert_eq!(core.hook(&id, hook("idle", json!({}), 1.0)).unwrap(), None);
    assert_eq!(core.get_session(&id).unwrap().state, None);
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
        bare.get_session(&id).unwrap().state,
        Some(LlmEngineState::Starting)
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

#[test]
fn every_write_stamps_last_modified_at() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(agent("a", &h.workdir("a"))).unwrap();
    let created = core.get_session(&id).unwrap();
    assert!(created.last_modified_at >= created.created_at);
    let pause = || std::thread::sleep(std::time::Duration::from_millis(5));

    pause();
    let renamed = core.rename_session(&id, "b").unwrap();
    assert!(
        renamed.last_modified_at > created.last_modified_at,
        "rename"
    );
    pause();
    let tagged = core.set_session_tags(&id, vec!["t".into()]).unwrap();
    assert!(tagged.last_modified_at > renamed.last_modified_at, "tag");
    pause();
    core.hook(&id, hook("idle", json!({}), 1.0)).unwrap();
    let changed = core.get_session(&id).unwrap();
    assert!(
        changed.last_modified_at > tagged.last_modified_at,
        "state change"
    );
    pause();
    // A hook that changes nothing writes nothing.
    core.hook(&id, hook("idle", json!({}), 2.0)).unwrap();
    let unchanged = core.get_session(&id).unwrap();
    assert_eq!(unchanged.last_modified_at, changed.last_modified_at);
    pause();
    let stopped = core.stop_session(&id).unwrap();
    assert!(stopped.last_modified_at > changed.last_modified_at, "stop");
    assert!(stopped.last_modified_at >= stopped.ended_at.unwrap());
}
