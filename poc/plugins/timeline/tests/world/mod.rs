//! The parity world: `tests/fixtures/world/world.json` (engine-neutral session descriptions)
//! and its transcripts, as tx-core records. `expected/*` was produced by main's timeline
//! (`crates/tx/src/plugins/timeline`) over the same description converted to main's v6 records
//! (`role: llm`, `state`, `cmd: "<engine> --model M --effort E"`, `last_activity`,
//! `chats[].transcript_path`), with `TZ=UTC`, `Builder::build(days, now)` and the
//! `TestBackend` frames of `run::snapshot` taken at the same `now`.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

pub fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/world")
}

/// When this file is included from another crate's tests.
pub fn fixture_dir_from(manifest: &str) -> PathBuf {
    Path::new(manifest).join("../plugins/timeline/tests/fixtures/world")
}

pub fn world(dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir.join("world.json")).unwrap()).unwrap()
}

pub fn expected(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join("expected").join(name)).unwrap()
}

/// Day labels and local midnights are local time; the expected files were made under UTC.
pub fn set_utc() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        // SAFETY: runs once, before any test thread of this binary reads the clock.
        unsafe { std::env::set_var("TZ", "UTC") };
        unsafe extern "C" {
            fn tzset();
        }
        // SAFETY: tzset only re-reads TZ.
        unsafe { tzset() };
    });
}

/// One world session as a tx-core record (`version: 1`): lifecycle and `status` from `state`,
/// the engine data (options and conversation), the edit plugin's group.
pub fn record(session: &Value, transcripts: &Path) -> Value {
    let engine = session["engine"].as_str().unwrap();
    let state = session["state"].as_str().unwrap();
    let launch = json!({
        "kind": "agent", "engine": engine, "profile": [], "resume": null,
    });
    let mut ext = Map::new();
    // Model, effort and the conversation are engine data (PR 12a, Engine 8c), under the
    // engine's own `ext` key.
    let mut data = Map::new();
    for key in ["model", "effort"] {
        if let Some(value) = session.get(key).filter(|value| !value.is_null()) {
            data.insert(key.to_owned(), value.clone());
        }
    }
    if let [chat] = session["transcripts"].as_array().unwrap().as_slice() {
        data.insert("conversation_id".into(), chat["chat"].clone());
        data.insert(
            "transcript_path".into(),
            json!(
                transcripts
                    .join(chat["file"].as_str().unwrap())
                    .to_string_lossy()
            ),
        );
    }
    if !data.is_empty() {
        ext.insert(engine.to_owned(), Value::Object(data));
    }
    if let Some(group) = session.get("group") {
        ext.insert("edit".into(), json!({ "group": group }));
    }
    // The record's last write: whichever of creation, end and the last state change came last.
    let last_modified_at = ["created_at", "ended_at", "activity"]
        .iter()
        .filter_map(|key| session.get(*key).and_then(Value::as_f64))
        .fold(f64::MIN, f64::max);
    json!({
        "version": 1,
        "id": session["id"], "name": session["name"],
        "parent": session.get("parent"), "tags": session["tags"], "cwd": "/tmp",
        "created_at": session["created_at"], "ended_at": session.get("ended_at"),
        "last_modified_at": last_modified_at,
        "lifecycle": if state == "exited" { "exited" } else { "running" },
        "launch": launch, "ext": ext,
        "state": if state == "exited" { "ended" } else { state },
    })
}

pub fn records(dir: &Path) -> Vec<Value> {
    let transcripts = dir.join("transcripts").canonicalize().unwrap();
    world(dir)["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|session| record(session, &transcripts))
        .collect()
}

/// What `TxCore::conversation` answers for these records: each engine's recorded
/// `transcript_path` (both engines name it the same way).
pub fn transcripts(sessions: &[tx_core::Session]) -> tx_timeline::model::Transcripts {
    sessions
        .iter()
        .filter_map(|session| {
            let tx_core::Launch::Agent { engine, .. } = &session.launch;
            let path = session
                .ext
                .get(engine.as_str())?
                .get("transcript_path")?
                .as_str()?;
            Some((session.id.clone(), PathBuf::from(path)))
        })
        .collect()
}
