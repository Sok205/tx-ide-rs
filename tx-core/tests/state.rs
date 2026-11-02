//! `TxCore::hook`: the adapter decides, core writes and logs; core alone ends it.

mod common;

use common::{Harness, STUB_ID, agent};
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
    let id = core.create_agent(agent("a", &h.repository("a"))).unwrap();
    let state = |core: &tx_core::TxCore| core.get_agent(&id).unwrap().state;
    assert_eq!(state(&core), LlmEngineState::Starting);

    let moved = core.hook(&id, hook("idle", json!({}), 10.0)).unwrap();
    assert_eq!(moved, Some(LlmEngineState::Idle));
    // Unknown to the adapter: nothing changes, nothing is logged.
    let unchanged = core.hook(&id, hook("bogus", json!({}), 11.0)).unwrap();
    assert_eq!(unchanged, Some(LlmEngineState::Idle));
    // The same state again: nothing is written or logged.
    core.hook(&id, hook("idle", json!({}), 12.0)).unwrap();
    core.hook(&id, hook("working", json!({}), 13.0)).unwrap();
    core.hook(&id, hook("idle", json!({}), 14.0)).unwrap();
    assert_eq!(state(&core), LlmEngineState::Idle);

    assert_eq!(
        state_lines(&h),
        ["starting → idle", "idle → working", "working → idle"]
    );

    let stopped = core.stop_agent(&id).unwrap();
    assert_eq!(stopped.state, LlmEngineState::Ended);
    // A stale hook after the end is ignored.
    assert_eq!(core.hook(&id, hook("idle", json!({}), 20.0)).unwrap(), None);
    assert_eq!(state(&core), LlmEngineState::Ended);
    assert_eq!(
        state_lines(&h).len(),
        3,
        "stop logs `stopped`, not a state change"
    );
}

fn data_lines(h: &Harness) -> Vec<String> {
    h.log_lines()
        .into_iter()
        .filter(|line| line["type"] == EventType::EngineDataChanged.as_str())
        .map(|line| line["msg"].as_str().unwrap().to_owned())
        .collect()
}

/// The engine's data from a hook lands under `ext[<ext key>]`, logged as `engine-data-changed`
/// with the key; an unchanged value writes and logs nothing; a plugin's own `ext` entry and the
/// engine's coexist.
#[test]
fn hooks_write_engine_data_under_the_engines_key() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_agent(agent("a", &h.repository("a"))).unwrap();
    // No options: the engine's key holds the empty object `configure` returned.
    assert_eq!(
        core.get_agent(&id).unwrap().ext.get(STUB_ID),
        Some(&json!({}))
    );

    let first = json!({"conversation_id": "c-1", "transcript_path": "/t/c-1.jsonl"});
    core.hook(&id, hook("data", first.clone(), 10.0)).unwrap();
    let session = core.get_agent(&id).unwrap();
    assert_eq!(session.ext.get(STUB_ID), Some(&first));
    assert_eq!(
        session.state,
        LlmEngineState::Starting,
        "data alone moves no state"
    );

    // The same value again: nothing is written or logged.
    let before = session.last_modified_at;
    std::thread::sleep(std::time::Duration::from_millis(5));
    core.hook(&id, hook("data", first.clone(), 11.0)).unwrap();
    assert_eq!(core.get_agent(&id).unwrap().last_modified_at, before);

    // Another plugin's entry survives the engine's rewrite, and the reverse.
    core.update_ext(&id, "other", |value| *value = json!({"x": 1}))
        .unwrap();
    let second = json!({"conversation_id": "c-2", "transcript_path": "/t/c-2.jsonl"});
    core.hook(&id, hook("data", second.clone(), 12.0)).unwrap();
    core.hook(&id, hook("idle", json!({}), 13.0)).unwrap();
    let session = core.get_agent(&id).unwrap();
    assert_eq!(session.ext.get(STUB_ID), Some(&second));
    assert_eq!(session.ext.get("other"), Some(&json!({"x": 1})));
    assert_eq!(session.state, LlmEngineState::Idle);

    assert_eq!(data_lines(&h), [STUB_ID, STUB_ID]);
    assert_eq!(state_lines(&h), ["starting → idle"]);
}

#[test]
fn an_exited_agent_reads_ended() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_agent(agent("a", &h.repository("a"))).unwrap();
    core.hook(&id, hook("working", json!({}), 10.0)).unwrap();
    let live = core.get_execution_location(&id).unwrap();
    h.tmux_ok(&["kill-session", "-t", &live.session_target]);
    core.refresh_all().unwrap();
    let session = core.get_agent(&id).unwrap();
    assert_eq!(session.state, LlmEngineState::Ended);
}

#[test]
fn a_hook_for_an_unregistered_engine_is_ignored() {
    require_tmux!();
    let h = Harness::new();
    let id = h
        .open_with_engine()
        .create_agent(agent("a", &h.repository("a")))
        .unwrap();
    let bare = h.open();
    assert_eq!(bare.hook(&id, hook("idle", json!({}), 1.0)).unwrap(), None);
    assert_eq!(bare.get_agent(&id).unwrap().state, LlmEngineState::Starting);
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
    let id = core.create_agent(agent("a", &h.repository("a"))).unwrap();
    let created = core.get_agent(&id).unwrap();
    assert!(created.last_modified_at >= created.created_at);
    let pause = || std::thread::sleep(std::time::Duration::from_millis(5));

    pause();
    let renamed = core.rename_agent(&id, "b").unwrap();
    assert!(
        renamed.last_modified_at > created.last_modified_at,
        "rename"
    );
    pause();
    let tagged = core.set_agent_tags(&id, vec!["t".into()]).unwrap();
    assert!(tagged.last_modified_at > renamed.last_modified_at, "tag");
    pause();
    core.hook(&id, hook("idle", json!({}), 1.0)).unwrap();
    let changed = core.get_agent(&id).unwrap();
    assert!(
        changed.last_modified_at > tagged.last_modified_at,
        "state change"
    );
    pause();
    // A hook that changes nothing writes nothing.
    core.hook(&id, hook("idle", json!({}), 2.0)).unwrap();
    let unchanged = core.get_agent(&id).unwrap();
    assert_eq!(unchanged.last_modified_at, changed.last_modified_at);
    pause();
    let stopped = core.stop_agent(&id).unwrap();
    assert!(stopped.last_modified_at > changed.last_modified_at, "stop");
}
