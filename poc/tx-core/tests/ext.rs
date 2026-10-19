//! Plugin-owned record data: `Session.ext`, written under the session lock.

mod common;

use std::sync::Arc;
use std::thread;

use common::{Harness, process};
use serde_json::{Value, json};
use tx_core::{Session, SessionId};

const RECORD: &str = r#"{"version":1,"id":"0f6b3b8a-1111-4222-8333-444444444444","name":"a","parent":null,
    "tags":[],"cwd":"/tmp","created_at":1.0,"ended_at":null,"lifecycle":"running",
    "launch":{"kind":"process","program":"sh","args":[]}"#;

#[test]
fn record_without_ext_loads_as_empty() {
    let session = Session::from_json(&format!("{RECORD}}}")).unwrap();
    assert!(session.ext.is_empty());
    assert!(
        !session.to_json().unwrap().contains("\"ext\""),
        "empty ext is not written"
    );
}

#[test]
fn ext_round_trips() {
    let session = Session::from_json(&format!(
        r#"{RECORD},"ext":{{"hooks":{{"state":"idle"}}}}}}"#
    ))
    .unwrap();
    assert_eq!(session.ext["hooks"], json!({"state": "idle"}));
    let again = Session::from_json(&session.to_json().unwrap()).unwrap();
    assert_eq!(again, session);
}

#[test]
fn update_ext_writes_only_on_change() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id = core.create_session(process("a", &h.workdir("a"))).unwrap();
    assert!(
        core.update_ext(&id, "hooks", |v| *v = json!({"state": "working"}))
            .unwrap()
    );
    assert_eq!(
        core.get_session(&id).unwrap().ext["hooks"]["state"],
        "working"
    );
    assert!(
        !core
            .update_ext(&id, "hooks", |v| {
                let _ = v;
            })
            .unwrap(),
        "untouched → false"
    );
    assert!(core.update_ext(&id, "other", |v| *v = json!(1)).unwrap());
    let ext = core.get_session(&id).unwrap().ext;
    assert_eq!(ext.len(), 2, "other plugins' keys survive");
}

#[test]
fn null_removes_the_key() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id = core.create_session(process("a", &h.workdir("a"))).unwrap();
    core.update_ext(&id, "hooks", |v| *v = json!({"state": "idle"}))
        .unwrap();
    assert!(core.update_ext(&id, "hooks", |v| *v = Value::Null).unwrap());
    assert!(core.get_session(&id).unwrap().ext.is_empty());
    assert!(
        !core.update_ext(&id, "hooks", |v| *v = Value::Null).unwrap(),
        "absent → Null is no change"
    );
}

#[test]
fn update_ext_on_unknown_session_is_not_found() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id: SessionId = "0f6b3b8a-1111-4222-8333-444444444444".parse().unwrap();
    assert!(matches!(
        core.update_ext(&id, "hooks", |_| {}),
        Err(tx_core::CoreError::NotFound(_))
    ));
}

#[test]
fn concurrent_updates_both_survive() {
    require_tmux!();
    let h = Harness::new();
    let core = Arc::new(h.open());
    let id = core.create_session(process("a", &h.workdir("a"))).unwrap();
    let handles: Vec<_> = ["x", "y"]
        .into_iter()
        .map(|key| {
            let core = Arc::clone(&core);
            let id = id.clone();
            thread::spawn(move || {
                for i in 0..50 {
                    core.update_ext(&id, key, |v| *v = json!(i)).unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let ext = core.get_session(&id).unwrap().ext;
    assert_eq!(ext["x"], json!(49));
    assert_eq!(ext["y"], json!(49));
}
