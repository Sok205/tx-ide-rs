//! rename / tags / refresh / stop / respawn / archive / location / send, against a private tmux
//! server.

mod common;

use std::time::Duration;

use common::{Harness, STUB_ID, STUB_KIND, agent, cwd_of, typed, wait_for};
use tx_core::{
    AgentId, CoreError, LaunchOptions, ListFilter, LlmEngineState, RefreshResult, SendOptions,
    Sender, TmuxError,
};

/// `KEY=value` lines of the `env.txt` the stub engine writes in its working directory.
fn stub_env(cwd: &std::path::Path, key: &str) -> Option<String> {
    std::fs::read_to_string(cwd.join("env.txt"))
        .unwrap_or_default()
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
}

fn process_exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn rename_updates_tmux_and_record() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_agent(agent("old", &h.repository("a"))).unwrap();
    let other = core
        .create_agent(agent("taken", &h.repository("b")))
        .unwrap();
    let session = core.rename_agent(&id, "new").unwrap();
    assert_eq!(session.name, "new");
    assert_eq!(core.get_agent(&id).unwrap().name, "new");
    let mut names = h.live_names();
    names.sort();
    assert_eq!(names, ["new", "taken"]);
    assert!(
        matches!(core.rename_agent(&id, "taken").unwrap_err(), CoreError::NameTaken(n) if n == "taken")
    );
    assert!(matches!(
        core.rename_agent(&id, "bad name").unwrap_err(),
        CoreError::InvalidName(_)
    ));
    assert!(matches!(
        core.rename_agent(&"zz".parse::<AgentId>().unwrap(), "x")
            .unwrap_err(),
        CoreError::NotFound(_)
    ));
    assert_eq!(
        core.rename_agent(&id, "new").unwrap().name,
        "new",
        "no-op rename"
    );
    assert_eq!(
        core.get_execution_location(&id).unwrap().session_target,
        core.get_execution_location(&id).unwrap().session_target
    );
    // A stopped record keeps its name.
    core.stop_agent(&other).unwrap();
    assert!(matches!(
        core.rename_agent(&id, "taken").unwrap_err(),
        CoreError::NameTaken(_)
    ));
    // Renaming a stopped record is a record-only change, and frees the old name.
    assert_eq!(
        core.rename_agent(&other, "was-taken").unwrap().name,
        "was-taken"
    );
    assert_eq!(core.rename_agent(&id, "taken").unwrap().name, "taken");
    assert_eq!(core.get_agent(&other).unwrap().state, LlmEngineState::Ended);
    let types: Vec<String> = h
        .log_lines()
        .iter()
        .map(|l| l["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        types,
        [
            "created", "created", "renamed", "stopped", "renamed", "renamed"
        ]
    );
}

#[test]
fn tags_are_replaced_wholesale() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let mut opts = agent("t", &h.repository("a"));
    opts.tags = vec!["a".into()];
    let id = core.create_agent(opts).unwrap();
    let session = core
        .set_agent_tags(&id, vec!["b".into(), "c".into()])
        .unwrap();
    assert_eq!(session.tags, ["b", "c"]);
    assert_eq!(core.get_agent(&id).unwrap().tags, ["b", "c"]);
    assert!(core.set_agent_tags(&id, vec![]).unwrap().tags.is_empty());
    assert!(matches!(
        core.set_agent_tags(&"zz".parse::<AgentId>().unwrap(), vec![])
            .unwrap_err(),
        CoreError::NotFound(_)
    ));
    assert_eq!(h.log_lines().last().unwrap()["type"], "tagged");
}

#[test]
fn refresh_marks_a_vanished_session_exited_once() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_agent(agent("v", &h.repository("a"))).unwrap();
    let keep = core.create_agent(agent("k", &h.repository("b"))).unwrap();
    assert!(matches!(
        core.refresh_agent(&id).unwrap(),
        RefreshResult::Unchanged(_)
    ));
    h.tmux_ok(&["kill-session", "-t", "=v"]);
    let before = h.record_bytes(&keep);
    let result = core.refresh_agent(&id).unwrap();
    let RefreshResult::Exited(session) = result else {
        panic!("{result:?}")
    };
    assert_eq!(session.state, LlmEngineState::Ended);
    let bytes = h.record_bytes(&id);
    assert!(
        matches!(core.refresh_agent(&id).unwrap(), RefreshResult::Unchanged(s) if s.state == LlmEngineState::Ended)
    );
    assert_eq!(
        h.record_bytes(&id),
        bytes,
        "a terminal record is never rewritten"
    );
    assert_eq!(h.record_bytes(&keep), before);
    assert!(
        matches!(core.get_execution_location(&id).unwrap_err(), CoreError::NotRunning(i) if i == id)
    );
    assert_eq!(h.log_lines().last().unwrap()["type"], "exited");
}

#[test]
fn refresh_all_returns_only_changed_records() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let a = core.create_agent(agent("a", &h.repository("a"))).unwrap();
    let b = core.create_agent(agent("b", &h.repository("b"))).unwrap();
    let c = core.create_agent(agent("c", &h.repository("c"))).unwrap();
    core.stop_agent(&c).unwrap();
    assert!(core.refresh_all().unwrap().is_empty());
    h.tmux_ok(&["kill-session", "-t", "=a"]);
    let changed = core.refresh_all().unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].id, a);
    assert_eq!(changed[0].state, LlmEngineState::Ended);
    assert_ne!(core.get_agent(&b).unwrap().state, LlmEngineState::Ended);
    assert_eq!(core.get_agent(&c).unwrap().state, LlmEngineState::Ended);
    assert!(core.refresh_all().unwrap().is_empty());
    h.tmux(&["kill-server"]);
    let changed = core.refresh_all().unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].id, b);
    assert!(
        h.live_names().is_empty(),
        "refresh did not restart the server"
    );
}

#[test]
fn stop_asks_the_engine_and_keeps_the_record() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_agent(agent("s", &h.repository("a"))).unwrap();
    let other = core.create_agent(agent("o", &h.repository("b"))).unwrap();
    let cwd = core.get_agent(&id).unwrap().working_directory;
    assert!(wait_for(Duration::from_secs(5), || cwd
        .join("env.txt")
        .exists()));
    let started = std::time::Instant::now();
    let session = core.stop_agent(&id).unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the stub left on the engine's stop input, no grace period: {:?}",
        started.elapsed()
    );
    assert_eq!(session.state, LlmEngineState::Ended);
    assert_eq!(h.live_names(), ["o"]);
    let stopped = h
        .log_lines()
        .into_iter()
        .find(|line| line["type"] == "stopped")
        .unwrap();
    assert_eq!(stopped["msg"], "", "left by itself, not killed");
    assert_eq!(core.get_agent(&id).unwrap().state, LlmEngineState::Ended);
    assert!(matches!(
        core.stop_agent(&id).unwrap_err(),
        CoreError::NotRunning(_)
    ));
    assert!(matches!(
        core.stop_agent(&"zz".parse::<AgentId>().unwrap())
            .unwrap_err(),
        CoreError::NotFound(_)
    ));
    // A session that vanished before stop is reported as what happened.
    h.tmux_ok(&["kill-session", "-t", "=o"]);
    assert_eq!(
        core.stop_agent(&other).unwrap().state,
        LlmEngineState::Ended
    );
    let types: Vec<String> = h
        .log_lines()
        .iter()
        .map(|l| l["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(types, ["created", "created", "stopped", "exited"]);
}

#[test]
fn stop_waits_for_an_agent_that_survives_the_pane_and_then_kills_it() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let mut opts = agent("tough", &h.repository("t"));
    // Ignores the hang-up the closed pane sends, as a CLI busy with its exit hooks might.
    opts.launch = LaunchOptions::agent(STUB_KIND)
        .initial_prompt("start")
        .option("model", "tough");
    let id = core.create_agent(opts).unwrap();
    let pid: u32 = h
        .tmux_ok(&["list-panes", "-t", "tough", "-F", "#{pane_pid}"])
        .trim()
        .parse()
        .unwrap();
    assert!(process_exists(pid));
    let started = std::time::Instant::now();
    let session = core.stop_agent(&id).unwrap();
    let waited = started.elapsed();
    assert_eq!(session.state, LlmEngineState::Ended);
    assert!(
        !process_exists(pid),
        "the process is gone once stop returns"
    );
    assert!(
        waited >= Duration::from_secs(20) && waited < Duration::from_secs(26),
        "stop waited {waited:?}: the grace period after the engine's stop input, kill-session, \
         the grace period again, then the kill"
    );
    let stopped = h
        .log_lines()
        .into_iter()
        .find(|line| line["type"] == "stopped")
        .unwrap();
    assert_eq!(stopped["msg"], "killed");
}

#[test]
fn interrupt_types_the_engines_input_into_a_running_agent() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_agent(agent("i", &h.repository("a"))).unwrap();
    let cwd = core.get_agent(&id).unwrap().working_directory;
    assert!(wait_for(Duration::from_secs(5), || cwd
        .join("env.txt")
        .exists()));
    // The input is typed, and the state the engine says follows, as for a hook.
    core.interrupt_agent(&id).unwrap();
    assert!(wait_for(Duration::from_secs(5), || !typed(&cwd).is_empty()));
    assert_eq!(typed(&cwd), "INTERRUPT\n");
    assert_eq!(core.get_agent(&id).unwrap().state, LlmEngineState::Idle);
    // Mid-turn: idle again, one state-changed each time the state moved.
    core.hook(
        &id,
        tx_core::LlmEngineHook {
            name: "working".into(),
            payload: serde_json::json!({}),
            at: 1.0,
        },
    )
    .unwrap();
    core.interrupt_agent(&id).unwrap();
    assert_eq!(core.get_agent(&id).unwrap().state, LlmEngineState::Idle);
    let states: Vec<String> = h
        .log_lines()
        .iter()
        .filter(|line| line["type"] == "state-changed")
        .map(|line| line["msg"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        states,
        ["starting → idle", "idle → working", "working → idle"]
    );
    assert!(matches!(
        core.interrupt_agent(&"zz".parse::<AgentId>().unwrap())
            .unwrap_err(),
        CoreError::NotFound(_)
    ));
    core.stop_agent(&id).unwrap();
    assert!(matches!(
        core.interrupt_agent(&id).unwrap_err(),
        CoreError::NotRunning(_)
    ));
}

#[test]
fn respawn_brings_an_ended_agent_back_in_place() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let mut opts = agent("r", &h.repository("repo"));
    // Model `data`: the stub names its conversation in the engine data, as Claude does.
    opts.launch = LaunchOptions::agent(STUB_KIND)
        .option("model", "data")
        .initial_prompt("remember OTTER-7");
    let id = core.create_agent(opts).unwrap();
    let before = core.get_agent(&id).unwrap();
    assert!(matches!(
        core.respawn_agent(&id).unwrap_err(),
        CoreError::StillRunning(ref running) if *running == id
    ));

    let stopped = core.stop_agent(&id).unwrap();
    assert_eq!(stopped.state, LlmEngineState::Ended);
    let session = core.respawn_agent(&id).unwrap();
    assert_eq!(session.id, id, "the same record");
    assert_eq!(session.name, "r");
    assert_eq!(
        session.working_directory, before.working_directory,
        "the same worktree"
    );
    assert_eq!(session.created_at, before.created_at);
    assert_ne!(session.state, LlmEngineState::Ended);
    assert_eq!(session.state, LlmEngineState::Starting);
    assert_eq!(session.ext.get(STUB_ID), before.ext.get(STUB_ID));
    assert_eq!(core.get_agent(&id).unwrap(), session);
    assert_eq!(h.live_names(), ["r"]);
    assert!(wait_for(Duration::from_secs(5), || {
        stub_env(&session.working_directory, "STUB_MODE").as_deref() == Some("resume")
    }));
    assert_eq!(
        stub_env(&session.working_directory, "STUB_RESUME").as_deref(),
        Some(id.as_str()),
        "the engine continues the conversation named in its data"
    );
    assert_eq!(
        stub_env(&session.working_directory, "STUB_SESSION_ID").as_deref(),
        Some(id.as_str())
    );
    assert!(
        h.state_dir()
            .join("profiles")
            .join(format!("{id}.md"))
            .is_file()
    );
    assert_eq!(
        core.list_agents(&ListFilter::new()).unwrap(),
        std::slice::from_ref(&id)
    );
    assert!(matches!(
        core.respawn_agent(&id).unwrap_err(),
        CoreError::StillRunning(_)
    ));
    // An exited agent (its process gone behind core's back) respawns the same way.
    h.tmux_ok(&["kill-session", "-t", "=r"]);
    assert!(matches!(
        core.refresh_agent(&id).unwrap(),
        RefreshResult::Exited(_)
    ));
    assert_ne!(
        core.respawn_agent(&id).unwrap().state,
        LlmEngineState::Ended
    );
    core.stop_agent(&id).unwrap();

    let types: Vec<String> = h
        .log_lines()
        .iter()
        .map(|line| line["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        types,
        [
            "created",
            "stopped",
            "respawned",
            "exited",
            "respawned",
            "stopped"
        ]
    );
    let respawned = &h.log_lines()[2];
    assert_eq!(respawned["id"], id.as_str());
    assert_eq!(respawned["msg"], "");
}

#[test]
fn respawn_refusals_change_nothing() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    assert!(matches!(
        core.respawn_agent(&"zz".parse::<AgentId>().unwrap())
            .unwrap_err(),
        CoreError::NotFound(_)
    ));

    let repository = h.repository("repo");
    let mut opts = agent("a", &repository);
    opts.launch = LaunchOptions::agent(STUB_KIND)
        .option("model", "data")
        .initial_prompt("start");
    let a = core.create_agent(opts).unwrap();
    core.stop_agent(&a).unwrap();
    // A stopped agent keeps its name: a rename onto it is refused.
    let b = core.create_agent(agent("b", &repository)).unwrap();
    assert!(matches!(
        core.rename_agent(&b, "a").unwrap_err(),
        CoreError::NameTaken(ref name) if name == "a"
    ));
    core.stop_agent(&b).unwrap();
    core.archive_agent(&b).unwrap();
    // Archived, `a` frees its name; a new agent takes it, in a worktree of its own (the
    // archived one keeps its directory). `a` cannot come back under the name, and is never
    // renamed for it.
    core.archive_agent(&a).unwrap();
    let c = core.create_agent(agent("a", &repository)).unwrap();
    assert_eq!(core.get_agent(&c).unwrap().name, "a");
    assert!(cwd_of(&core, &a).is_dir());
    assert_ne!(cwd_of(&core, &c), cwd_of(&core, &a));
    let error = core.respawn_agent(&a).unwrap_err();
    assert!(
        error.to_string().contains("rename one of them first"),
        "{error}"
    );
    assert!(matches!(
        error,
        CoreError::NameHeld { ref name, ref holder } if name == "a" && *holder == c
    ));
    let held = core.get_agent(&a).unwrap();
    assert_eq!(held.name, "a");
    assert!(
        held.archived_at.is_some(),
        "a refused respawn leaves it archived"
    );
    core.rename_agent(&c, "b").unwrap();
    // A worktree that is gone is never recreated.
    let worktree = cwd_of(&core, &a);
    std::fs::remove_dir_all(&worktree).unwrap();
    assert!(matches!(
        core.respawn_agent(&a).unwrap_err(),
        CoreError::WorktreeMissing(ref path) if *path == worktree
    ));
    let still = core.get_agent(&a).unwrap();
    assert_eq!(still.state, LlmEngineState::Ended);
    assert_eq!(h.live_names(), ["b"]);
    let types: Vec<String> = h
        .log_lines()
        .iter()
        .map(|line| line["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        types,
        [
            "created", "stopped", "created", "stopped", "archived", "archived", "created",
            "renamed"
        ]
    );
}

#[test]
fn archive_stops_hides_and_keeps_everything_until_a_respawn() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("a");
    let mut opts = agent("f", &repository);
    opts.launch = LaunchOptions::agent(STUB_KIND)
        .option("model", "data")
        .initial_prompt("start");
    let id = core.create_agent(opts).unwrap();
    let worktree = cwd_of(&core, &id);
    let profile = h.state_dir().join(format!("profiles/{id}.md"));
    // Running: stopped first, then archived.
    let archived = core.archive_agent(&id).unwrap();
    assert_eq!(archived.state, LlmEngineState::Ended);
    assert!(archived.archived_at.is_some());
    assert_eq!(core.get_agent(&id).unwrap(), archived);
    assert!(
        h.data_dir()
            .join(format!("agents/{id}/agent.json"))
            .exists()
    );
    assert!(profile.exists());
    assert!(worktree.is_dir());
    assert!(h.live_names().is_empty());
    assert!(core.list_agents(&ListFilter::new()).unwrap().is_empty());
    assert_eq!(
        core.list_agents(&ListFilter::new().archived()).unwrap(),
        std::slice::from_ref(&id)
    );
    assert_eq!(
        core.resolve("f").unwrap(),
        id,
        "an archived agent resolves by name"
    );
    // Again: unchanged, no event.
    assert_eq!(core.archive_agent(&id).unwrap(), archived);
    // Its name is free: a new agent gets it plainly, in another worktree directory.
    let other = core.create_agent(agent("f", &repository)).unwrap();
    assert_eq!(core.get_agent(&other).unwrap().name, "f");
    assert_ne!(cwd_of(&core, &other), worktree);
    assert_eq!(core.resolve("f").unwrap(), other, "the unarchived one wins");
    core.archive_agent(&other).unwrap();
    // A respawn brings the first back in place and unarchives it.
    let respawned = core.respawn_agent(&id).unwrap();
    assert_ne!(respawned.state, LlmEngineState::Ended);
    assert_eq!(respawned.archived_at, None);
    assert_eq!(respawned.working_directory, worktree);
    assert_eq!(
        core.list_agents(&ListFilter::new()).unwrap(),
        std::slice::from_ref(&id)
    );
    assert_eq!(core.resolve("f").unwrap(), id);
    assert_eq!(h.live_names(), ["f"]);
    let types: Vec<String> = h
        .log_lines()
        .iter()
        .map(|l| l["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        types,
        [
            "created",
            "stopped",
            "archived",
            "created",
            "stopped",
            "archived",
            "respawned"
        ]
    );
}

#[test]
fn a_stopped_agent_keeps_its_name_from_a_new_create() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("a");
    let first = core.create_agent(agent("n", &repository)).unwrap();
    core.stop_agent(&first).unwrap();
    let second = core.create_agent(agent("n", &repository)).unwrap();
    assert_eq!(core.get_agent(&second).unwrap().name, "n-2");
}

#[test]
fn messages_arrive_whole_with_the_right_envelope() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let a = core
        .create_agent(agent("alice", &h.repository("a")))
        .unwrap();
    let cwd_a = cwd_of(&core, &a);
    let b = core.create_agent(agent("bob", &h.repository("b"))).unwrap();
    assert!(wait_for(Duration::from_secs(5), || cwd_a
        .join("env.txt")
        .exists()));
    let receipt = core
        .send_message(&a, "hello; -n <x>", SendOptions::default())
        .unwrap();
    assert_eq!(receipt.recipient, a);
    assert!(receipt.pane.starts_with('%'));
    assert!(receipt.submitted_at > 1.7e9);
    core.send_message(&a, "from bob", SendOptions::from_agent(b.clone()))
        .unwrap();
    core.send_message(
        &a,
        "from notes",
        SendOptions::from(Sender::User {
            via: Some("notes".into()),
        }),
    )
    .unwrap();
    assert!(wait_for(Duration::from_secs(5), || typed(&cwd_a)
        .lines()
        .count()
        == 3));
    assert_eq!(
        typed(&cwd_a),
        "<from-user>hello; -n <x></from-user>\n<from-agent session=\"bob\">from bob</from-agent>\n<from-user session=\"notes\">from notes</from-user>\n"
    );
    let log = h.log_lines();
    let sent: Vec<&serde_json::Value> =
        log.iter().filter(|l| l["type"] == "message-sent").collect();
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0]["msg"], "from user");
    assert_eq!(sent[2]["actor"], "");
    assert_eq!(sent[2]["msg"], "from user via notes");
    assert_eq!(sent[0]["actor"], "");
    assert_eq!(sent[1]["actor"], b.as_str());
    assert_eq!(sent[1]["name"], "alice");
    assert_eq!(sent[1]["id"], a.as_str());
    assert_eq!(sent[1]["msg"], "from bob");

    assert!(matches!(
        core.send_message(&a, "x", SendOptions::from_agent("zz".parse().unwrap()))
            .unwrap_err(),
        CoreError::NotFound(_)
    ));
    core.stop_agent(&b).unwrap();
    assert!(matches!(
        core.send_message(&b, "x", SendOptions::default())
            .unwrap_err(),
        CoreError::NotRunning(_)
    ));
    // A stopped sender may still send.
    core.send_message(&a, "late", SendOptions::from_agent(b.clone()))
        .unwrap();
    h.tmux_ok(&["kill-session", "-t", "=alice"]);
    assert!(matches!(
        core.send_message(&a, "x", SendOptions::default())
            .unwrap_err(),
        CoreError::NotRunning(_)
    ));
    assert_ne!(
        core.get_agent(&a).unwrap().state,
        LlmEngineState::Ended,
        "send never reconciles"
    );
}

#[test]
fn a_session_renamed_behind_cores_back_is_still_reachable() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core
        .create_agent(agent("orig", &h.repository("a")))
        .unwrap();
    let cwd = cwd_of(&core, &id);
    assert!(wait_for(Duration::from_secs(5), || cwd
        .join("env.txt")
        .exists()));
    h.tmux_ok(&["rename-session", "-t", "=orig", "sneaky"]);
    assert!(core.get_execution_location(&id).is_ok());
    core.send_message(&id, "still here", SendOptions::default())
        .unwrap();
    assert!(wait_for(Duration::from_secs(5), || typed(&cwd)
        .contains("still here")));
    assert!(matches!(
        core.refresh_agent(&id).unwrap(),
        RefreshResult::Unchanged(_)
    ));
    assert_eq!(core.rename_agent(&id, "fixed").unwrap().name, "fixed");
    assert_eq!(h.live_names(), ["fixed"]);
    assert_eq!(core.stop_agent(&id).unwrap().state, LlmEngineState::Ended);
    assert!(h.live_names().is_empty());
}

#[test]
fn inspection_failure_writes_nothing() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_agent(agent("i", &h.repository("a"))).unwrap();
    let before = h.record_bytes(&id);
    // Make the socket unusable: replace it with a directory.
    h.tmux(&["kill-server"]);
    std::fs::remove_file(h.socket()).ok();
    std::fs::create_dir(h.socket()).unwrap();
    let is_inspection =
        |error: CoreError| matches!(error, CoreError::Tmux(TmuxError::InspectionFailed { .. }));
    assert!(is_inspection(core.refresh_agent(&id).unwrap_err()));
    assert!(is_inspection(core.refresh_all().unwrap_err()));
    assert!(is_inspection(core.get_execution_location(&id).unwrap_err()));
    assert!(is_inspection(core.stop_agent(&id).unwrap_err()));
    assert!(is_inspection(
        core.send_message(&id, "x", SendOptions::default())
            .unwrap_err()
    ));
    assert!(is_inspection(core.rename_agent(&id, "y").unwrap_err()));
    assert_eq!(h.record_bytes(&id), before);
    assert_ne!(core.get_agent(&id).unwrap().state, LlmEngineState::Ended);
    assert_eq!(h.log_lines().len(), 1);
    std::fs::remove_dir(h.socket()).unwrap();
}

#[test]
fn location_of_a_terminal_record_is_not_running_even_if_tmux_has_its_id() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_agent(agent("t", &h.repository("a"))).unwrap();
    core.stop_agent(&id).unwrap();
    h.tmux_ok(&["new-session", "-d", "-s", "ghost", "sleep", "30"]);
    h.tmux_ok(&["set-option", "-t", "=ghost:", "@tx-id", id.as_str()]);
    assert!(matches!(
        core.get_execution_location(&id).unwrap_err(),
        CoreError::NotRunning(_)
    ));
    assert_eq!(core.get_agent(&id).unwrap().state, LlmEngineState::Ended);
}
