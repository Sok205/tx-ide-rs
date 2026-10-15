//! rename / tags / refresh / stop / forget / location / send, against a private tmux server.

mod common;

use std::time::Duration;

use common::{Harness, agent, process, typed, wait_for};
use tx_core::{CoreError, Lifecycle, ListFilter, RefreshResult, SendOptions, SessionId, TmuxError};

#[test]
fn rename_updates_tmux_and_record() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id = core.create_session(process("old", &h.workdir("a"))).unwrap();
    let other = core.create_session(process("taken", &h.workdir("b"))).unwrap();
    let session = core.rename_session(&id, "new").unwrap();
    assert_eq!(session.name, "new");
    assert_eq!(core.get_session(&id).unwrap().name, "new");
    let mut names = h.live_names();
    names.sort();
    assert_eq!(names, ["new", "taken"]);
    assert!(matches!(core.rename_session(&id, "taken").unwrap_err(), CoreError::NameTaken(n) if n == "taken"));
    assert!(matches!(core.rename_session(&id, "bad name").unwrap_err(), CoreError::InvalidName(_)));
    assert!(matches!(core.rename_session(&"zz".parse::<SessionId>().unwrap(), "x").unwrap_err(), CoreError::NotFound(_)));
    assert_eq!(core.rename_session(&id, "new").unwrap().name, "new", "no-op rename");
    assert_eq!(core.get_execution_location(&id).unwrap().session_target, core.get_execution_location(&id).unwrap().session_target);
    // Renaming to a name a stopped record holds is fine.
    core.stop_session(&other).unwrap();
    assert_eq!(core.rename_session(&id, "taken").unwrap().name, "taken");
    // Renaming a stopped record is a record-only change.
    assert_eq!(core.rename_session(&other, "was-taken").unwrap().name, "was-taken");
    assert_eq!(core.get_session(&other).unwrap().lifecycle, Lifecycle::Stopped);
    let types: Vec<String> = h.log_lines().iter().map(|l| l["type"].as_str().unwrap().to_owned()).collect();
    assert_eq!(types, ["created", "created", "renamed", "stopped", "renamed", "renamed"]);
}

#[test]
fn tags_are_replaced_wholesale() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let mut opts = process("t", &h.workdir("a"));
    opts.tags = vec!["a".into()];
    let id = core.create_session(opts).unwrap();
    let session = core.set_session_tags(&id, vec!["b".into(), "c".into()]).unwrap();
    assert_eq!(session.tags, ["b", "c"]);
    assert_eq!(core.get_session(&id).unwrap().tags, ["b", "c"]);
    assert!(core.set_session_tags(&id, vec![]).unwrap().tags.is_empty());
    assert!(matches!(core.set_session_tags(&"zz".parse::<SessionId>().unwrap(), vec![]).unwrap_err(), CoreError::NotFound(_)));
    assert_eq!(h.log_lines().last().unwrap()["type"], "tagged");
}

#[test]
fn refresh_marks_a_vanished_session_exited_once() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id = core.create_session(process("v", &h.workdir("a"))).unwrap();
    let keep = core.create_session(process("k", &h.workdir("b"))).unwrap();
    assert!(matches!(core.refresh_session(&id).unwrap(), RefreshResult::Unchanged(_)));
    h.tmux_ok(&["kill-session", "-t", "=v"]);
    let before = h.record_bytes(&keep);
    let result = core.refresh_session(&id).unwrap();
    let RefreshResult::Exited(session) = result else {
        panic!("{result:?}")
    };
    assert_eq!(session.lifecycle, Lifecycle::Exited);
    assert!(session.ended_at.is_some());
    let bytes = h.record_bytes(&id);
    assert!(matches!(core.refresh_session(&id).unwrap(), RefreshResult::Unchanged(s) if s.lifecycle == Lifecycle::Exited));
    assert_eq!(h.record_bytes(&id), bytes, "a terminal record is never rewritten");
    assert_eq!(h.record_bytes(&keep), before);
    assert!(matches!(core.get_execution_location(&id).unwrap_err(), CoreError::NotRunning(i) if i == id));
    assert_eq!(h.log_lines().last().unwrap()["type"], "exited");
}

#[test]
fn refresh_all_returns_only_changed_records() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let a = core.create_session(process("a", &h.workdir("a"))).unwrap();
    let b = core.create_session(process("b", &h.workdir("b"))).unwrap();
    let c = core.create_session(process("c", &h.workdir("c"))).unwrap();
    core.stop_session(&c).unwrap();
    assert!(core.refresh_all().unwrap().is_empty());
    h.tmux_ok(&["kill-session", "-t", "=a"]);
    let changed = core.refresh_all().unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].id, a);
    assert_eq!(changed[0].lifecycle, Lifecycle::Exited);
    assert_eq!(core.get_session(&b).unwrap().lifecycle, Lifecycle::Running);
    assert_eq!(core.get_session(&c).unwrap().lifecycle, Lifecycle::Stopped);
    assert!(core.refresh_all().unwrap().is_empty());
    h.tmux(&["kill-server"]);
    let changed = core.refresh_all().unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].id, b);
    assert!(h.live_names().is_empty(), "refresh did not restart the server");
}

#[test]
fn stop_kills_and_keeps_the_record() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id = core.create_session(process("s", &h.workdir("a"))).unwrap();
    let other = core.create_session(process("o", &h.workdir("b"))).unwrap();
    let session = core.stop_session(&id).unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Stopped);
    assert!(session.ended_at.is_some());
    assert_eq!(h.live_names(), ["o"]);
    assert_eq!(core.get_session(&id).unwrap().lifecycle, Lifecycle::Stopped);
    assert!(matches!(core.stop_session(&id).unwrap_err(), CoreError::NotRunning(_)));
    assert!(matches!(core.stop_session(&"zz".parse::<SessionId>().unwrap()).unwrap_err(), CoreError::NotFound(_)));
    // A session that vanished before stop is reported as what happened.
    h.tmux_ok(&["kill-session", "-t", "=o"]);
    assert_eq!(core.stop_session(&other).unwrap().lifecycle, Lifecycle::Exited);
    let types: Vec<String> = h.log_lines().iter().map(|l| l["type"].as_str().unwrap().to_owned()).collect();
    assert_eq!(types, ["created", "created", "stopped", "exited"]);
}

#[test]
fn forget_drops_the_record_and_leaves_tmux_alone() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(agent("f", &h.workdir("a"))).unwrap();
    let profile = h.state_dir().join(format!("profiles/{id}.md"));
    assert!(profile.exists());
    assert!(core.forget_session(&id).unwrap());
    assert!(!h.state_dir().join(format!("sessions/{id}.json")).exists());
    assert!(!profile.exists());
    assert_eq!(h.live_names(), ["f"], "the tmux session keeps running");
    assert!(!core.forget_session(&id).unwrap());
    assert!(matches!(core.get_session(&id).unwrap_err(), CoreError::NotFound(_)));
    assert!(core.list_sessions(&ListFilter::default()).unwrap().is_empty());
    let types: Vec<String> = h.log_lines().iter().map(|l| l["type"].as_str().unwrap().to_owned()).collect();
    assert_eq!(types, ["created", "forgotten"]);
}

#[test]
fn messages_arrive_whole_with_the_right_envelope() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let cwd_a = h.workdir("a");
    let a = core.create_session(agent("alice", &cwd_a)).unwrap();
    let b = core.create_session(process("bob", &h.workdir("b"))).unwrap();
    assert!(wait_for(Duration::from_secs(5), || cwd_a.join("env.txt").exists()));
    let receipt = core.send_message(&a, "hello; -n <x>", SendOptions::default()).unwrap();
    assert_eq!(receipt.recipient, a);
    assert!(receipt.pane.starts_with('%'));
    assert!(receipt.submitted_at > 1.7e9);
    core.send_message(&a, "from bob", SendOptions::from_agent(b.clone())).unwrap();
    assert!(wait_for(Duration::from_secs(5), || typed(&cwd_a).lines().count() == 2));
    assert_eq!(
        typed(&cwd_a),
        "<from-user>hello; -n <x></from-user>\n<from-agent session=\"bob\">from bob</from-agent>\n"
    );
    let log = h.log_lines();
    let sent: Vec<&serde_json::Value> = log.iter().filter(|l| l["type"] == "message-sent").collect();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["actor"], "");
    assert_eq!(sent[1]["actor"], b.as_str());
    assert_eq!(sent[1]["name"], "alice");
    assert_eq!(sent[1]["id"], a.as_str());
    assert_eq!(sent[1]["msg"], "from bob");

    assert!(matches!(
        core.send_message(&a, "x", SendOptions::from_agent("zz".parse().unwrap())).unwrap_err(),
        CoreError::NotFound(_)
    ));
    core.stop_session(&b).unwrap();
    assert!(matches!(core.send_message(&b, "x", SendOptions::default()).unwrap_err(), CoreError::NotRunning(_)));
    // A stopped sender may still send.
    core.send_message(&a, "late", SendOptions::from_agent(b.clone())).unwrap();
    h.tmux_ok(&["kill-session", "-t", "=alice"]);
    assert!(matches!(core.send_message(&a, "x", SendOptions::default()).unwrap_err(), CoreError::NotRunning(_)));
    assert_eq!(core.get_session(&a).unwrap().lifecycle, Lifecycle::Running, "send never reconciles");
}

#[test]
fn a_session_renamed_behind_cores_back_is_still_reachable() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    let id = core.create_session(process("orig", &cwd)).unwrap();
    assert!(wait_for(Duration::from_secs(5), || cwd.join("env.txt").exists()));
    h.tmux_ok(&["rename-session", "-t", "=orig", "sneaky"]);
    assert!(core.get_execution_location(&id).is_ok());
    core.send_message(&id, "still here", SendOptions::default()).unwrap();
    assert!(wait_for(Duration::from_secs(5), || typed(&cwd).contains("still here")));
    assert!(matches!(core.refresh_session(&id).unwrap(), RefreshResult::Unchanged(_)));
    assert_eq!(core.rename_session(&id, "fixed").unwrap().name, "fixed");
    assert_eq!(h.live_names(), ["fixed"]);
    assert_eq!(core.stop_session(&id).unwrap().lifecycle, Lifecycle::Stopped);
    assert!(h.live_names().is_empty());
}

#[test]
fn inspection_failure_writes_nothing() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id = core.create_session(process("i", &h.workdir("a"))).unwrap();
    let before = h.record_bytes(&id);
    // Make the socket unusable: replace it with a directory.
    h.tmux(&["kill-server"]);
    std::fs::remove_file(h.socket()).ok();
    std::fs::create_dir(h.socket()).unwrap();
    let is_inspection = |error: CoreError| matches!(error, CoreError::Tmux(TmuxError::InspectionFailed { .. }));
    assert!(is_inspection(core.refresh_session(&id).unwrap_err()));
    assert!(is_inspection(core.refresh_all().unwrap_err()));
    assert!(is_inspection(core.get_execution_location(&id).unwrap_err()));
    assert!(is_inspection(core.stop_session(&id).unwrap_err()));
    assert!(is_inspection(core.send_message(&id, "x", SendOptions::default()).unwrap_err()));
    assert!(is_inspection(core.rename_session(&id, "y").unwrap_err()));
    assert_eq!(h.record_bytes(&id), before);
    assert_eq!(core.get_session(&id).unwrap().lifecycle, Lifecycle::Running);
    assert_eq!(h.log_lines().len(), 1);
    std::fs::remove_dir(h.socket()).unwrap();
}

#[test]
fn location_of_a_terminal_record_is_not_running_even_if_tmux_has_its_id() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id = core.create_session(process("t", &h.workdir("a"))).unwrap();
    core.stop_session(&id).unwrap();
    h.tmux_ok(&["new-session", "-d", "-s", "ghost", "sleep", "30"]);
    h.tmux_ok(&["set-option", "-t", "=ghost:", "@tx-id", id.as_str()]);
    assert!(matches!(core.get_execution_location(&id).unwrap_err(), CoreError::NotRunning(_)));
    assert_eq!(core.get_session(&id).unwrap().lifecycle, Lifecycle::Stopped);
}
