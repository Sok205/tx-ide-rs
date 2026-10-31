//! `LlmEngine::archive` after every hook, a stop and an exit; `restore` before a respawn.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{Harness, STUB_KIND, agent, wait_for};
use serde_json::json;
use tx_core::{EventType, LaunchOptions, LlmEngineHook, LlmEngineState, SessionId};

fn hook(name: &str, payload: serde_json::Value, at: f64) -> LlmEngineHook {
    LlmEngineHook {
        name: name.into(),
        payload,
        at,
    }
}

fn archive_lines(h: &Harness) -> Vec<String> {
    h.log_lines()
        .into_iter()
        .filter(|line| line["type"] == EventType::ArchiveFailed.as_str())
        .map(|line| line["msg"].as_str().unwrap().to_owned())
        .collect()
}

fn archived(h: &Harness, id: &SessionId, conversation: &str) -> PathBuf {
    h.config()
        .archive_directory(id)
        .join(format!("{conversation}.jsonl"))
}

#[test]
fn every_hook_archives_the_conversation_and_a_failure_is_an_event() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(agent("a", &h.repository("a"))).unwrap();
    let transcript = h.dir.path().join("live/c-1.jsonl");
    let data = json!({"conversation_id": "c-1", "transcript_path": transcript});
    core.hook(&id, hook("data", data, 10.0)).unwrap();
    let copy = archived(&h, &id, "c-1");
    assert!(!copy.exists(), "no live transcript yet: nothing to archive");
    assert!(
        h.config()
            .archive_directory(&id)
            .join(".archive.lock")
            .is_file(),
        "the per-session lock lives in the archive"
    );

    // The first turn: a whole copy on the idle hook.
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "one\n").unwrap();
    assert_eq!(
        core.hook(&id, hook("idle", json!({}), 11.0)).unwrap(),
        Some(LlmEngineState::Idle)
    );
    assert_eq!(std::fs::read(&copy).unwrap(), b"one\n");

    // Lines written after the idle hook reach the archive on the next hook, whatever it is.
    std::fs::write(&transcript, "one\ntwo\n").unwrap();
    core.hook(&id, hook("working", json!({}), 12.0)).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), b"one\ntwo\n");
    std::fs::write(&transcript, "one\ntwo\nthree\n").unwrap();
    core.hook(&id, hook("bogus", json!({}), 13.0)).unwrap();
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        b"one\ntwo\nthree\n",
        "a hook that changes nothing still archives"
    );
    assert!(archive_lines(&h).is_empty());

    // A failing archive: the hook still succeeds and moves the state; the failure is logged.
    let refused = json!({"conversation_id": "c-1", "transcript_path": "refuse"});
    core.hook(&id, hook("data", refused, 15.0)).unwrap();
    assert_eq!(
        core.hook(&id, hook("idle", json!({}), 17.0)).unwrap(),
        Some(LlmEngineState::Idle)
    );
    assert_eq!(core.get_session(&id).unwrap().state, LlmEngineState::Idle);
    assert_eq!(archive_lines(&h), ["archive refused", "archive refused"]);
    assert_eq!(std::fs::read(&copy).unwrap(), b"one\ntwo\nthree\n");
}

#[test]
fn a_held_archive_lock_makes_the_hook_skip_the_archive() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(agent("a", &h.repository("a"))).unwrap();
    let transcript = h.dir.path().join("live/c-1.jsonl");
    let data = json!({"conversation_id": "c-1", "transcript_path": transcript});
    core.hook(&id, hook("data", data, 10.0)).unwrap();
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "one\n").unwrap();

    let archive_directory = h.config().archive_directory(&id);
    let holder = std::fs::File::create(archive_directory.join(".archive.lock")).unwrap();
    holder.lock().unwrap();
    assert_eq!(
        core.hook(&id, hook("idle", json!({}), 11.0)).unwrap(),
        Some(LlmEngineState::Idle)
    );
    let copy = archived(&h, &id, "c-1");
    assert!(
        !copy.exists(),
        "a held lock means another archive is running"
    );
    assert!(archive_lines(&h).is_empty(), "skipping is not a failure");

    drop(holder);
    core.hook(&id, hook("idle", json!({}), 12.0)).unwrap();
    assert_eq!(std::fs::read(&copy).unwrap(), b"one\n");
}

#[test]
fn respawn_restores_a_deleted_transcript_from_the_archive() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let mut opts = agent("r", &h.repository("repo"));
    opts.launch = LaunchOptions::agent(STUB_KIND)
        .option("model", "data")
        .initial_prompt("remember OTTER-7");
    let id = core.create_session(opts).unwrap();
    let transcript = h.dir.path().join("live").join(format!("{id}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, "OTTER-7\n").unwrap();
    let data = json!({
        "conversation_id": id.as_str(),
        "transcript_path": transcript,
        "model": "data",
    });
    core.hook(&id, hook("data", data, 10.0)).unwrap();
    core.hook(&id, hook("idle", json!({}), 11.0)).unwrap();
    let copy = archived(&h, &id, id.as_str());
    assert_eq!(std::fs::read(&copy).unwrap(), b"OTTER-7\n");

    core.stop_session(&id).unwrap();
    std::fs::remove_file(&transcript).unwrap();
    let session = core.respawn_session(&id).unwrap();
    assert_eq!(
        std::fs::read(&transcript).unwrap(),
        b"OTTER-7\n",
        "the live transcript is back before the resume"
    );
    assert!(wait_for(Duration::from_secs(5), || {
        std::fs::read_to_string(session.working_directory.join("env.txt"))
            .unwrap_or_default()
            .contains("STUB_MODE=resume")
    }));

    // A respawn with the live transcript in place leaves it alone.
    core.stop_session(&id).unwrap();
    std::fs::write(&transcript, "OTTER-7\nmore\n").unwrap();
    core.respawn_session(&id).unwrap();
    assert_eq!(std::fs::read(&transcript).unwrap(), b"OTTER-7\nmore\n");
    assert_eq!(std::fs::read(&copy).unwrap(), b"OTTER-7\n");
    assert!(archive_lines(&h).is_empty());
}

/// The process is gone, so the transcript is final: a stop and an exit both archive it, and
/// compared whole, so a rewrite further back than the offset mirror's trailing window (which a
/// hook leaves stale) is picked up too.
#[test]
fn a_stop_and_an_exit_archive_the_final_transcript_exactly() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let transcript_of = |name: &str| h.dir.path().join("live").join(format!("{name}.jsonl"));
    std::fs::create_dir_all(h.dir.path().join("live")).unwrap();
    let original: Vec<u8> = (0..=255u8).cycle().take(204_800).collect();
    // Rewritten 100 KiB before the end, same length, then appended to after the last hook.
    let mut final_bytes = original.clone();
    final_bytes[original.len() - 102_400] ^= 0xFF;
    final_bytes.extend_from_slice(b"written after the last hook\n");

    let stopped = core.create_session(agent("s", &h.repository("s"))).unwrap();
    let transcript = transcript_of("s");
    std::fs::write(&transcript, &original).unwrap();
    let data = json!({"conversation_id": "s", "transcript_path": transcript});
    core.hook(&stopped, hook("idle", json!({}), 10.0)).unwrap();
    core.hook(&stopped, hook("data", data, 11.0)).unwrap();
    std::fs::write(&transcript, &final_bytes).unwrap();
    core.hook(&stopped, hook("working", json!({}), 12.0))
        .unwrap();
    let copy = archived(&h, &stopped, "s");
    assert_eq!(
        std::fs::read(&copy).unwrap()[..original.len()],
        original[..],
        "a hook appends by offset and misses the early rewrite"
    );
    core.stop_session(&stopped).unwrap();
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        final_bytes,
        "exact after the stop"
    );

    let exited = core.create_session(agent("e", &h.repository("e"))).unwrap();
    let transcript = transcript_of("e");
    std::fs::write(&transcript, &original).unwrap();
    let data = json!({"conversation_id": "e", "transcript_path": transcript});
    core.hook(&exited, hook("data", data, 13.0)).unwrap();
    std::fs::write(&transcript, &final_bytes).unwrap();
    let live = core.get_execution_location(&exited).unwrap();
    h.tmux_ok(&["kill-session", "-t", &live.session_target]);
    core.refresh_all().unwrap();
    assert_eq!(
        core.get_session(&exited).unwrap().state,
        LlmEngineState::Ended
    );
    assert_eq!(
        std::fs::read(archived(&h, &exited, "e")).unwrap(),
        final_bytes,
        "exact after the exit"
    );
    assert!(archive_lines(&h).is_empty());
}

#[test]
fn archive_directory_is_under_the_agents_data_directory() {
    let h = Harness::new();
    let id = SessionId::mint();
    assert_eq!(
        h.config().archive_directory(&id),
        h.data_dir()
            .join("agents")
            .join(id.as_str())
            .join("archive")
    );
    assert!(
        h.config()
            .archive_directory(&id)
            .starts_with(h.config().agent_dir(&id))
    );
}
