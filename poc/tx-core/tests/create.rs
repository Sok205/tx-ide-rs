//! `TxCore::open`, `create_session` and the reads, against a private tmux server.

mod common;

use std::time::Duration;

use common::{Harness, STUB_ID, agent, process, wait_for};
use tx_core::{
    CoreError, CreateOptions, Launch, LaunchOptions, Lifecycle, ListFilter, Session, SessionId,
};

fn env_of(cwd: &std::path::Path) -> Vec<(String, String)> {
    assert!(wait_for(Duration::from_secs(5), || cwd
        .join("env.txt")
        .exists()));
    std::fs::read_to_string(cwd.join("env.txt"))
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

fn get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

#[test]
fn process_session_runs_detached_with_the_tx_environment() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("p");
    let id = core.create_session(process("proc-1", &cwd)).unwrap();
    let session = core.get_session(&id).unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Running);
    assert_eq!(session.name, "proc-1");
    assert_eq!(session.cwd, cwd.canonicalize().unwrap());
    assert!(session.ended_at.is_none());
    assert!(matches!(session.launch, Launch::Process { ref program, .. } if program == "sh"));
    assert_eq!(h.live_names(), ["proc-1"]);
    assert_eq!(
        h.tmux_ok(&["show-options", "-v", "-t", "=proc-1:", "@tx-id"])
            .trim(),
        id.as_str()
    );
    let env = env_of(&cwd);
    assert_eq!(get(&env, "TX_SESSION_ID"), Some(id.as_str()));
    assert_eq!(
        get(&env, "TX_TMUX_SOCKET"),
        Some(h.socket().to_str().unwrap())
    );
    assert_eq!(
        get(&env, "TX_CORE_HOME"),
        Some(h.state_dir().to_str().unwrap())
    );
    let location = core.get_execution_location(&id).unwrap();
    assert_eq!(location.socket, h.socket());
    assert!(location.session_target.starts_with('$'));
    assert!(location.pane_target.starts_with('%'));
    let log = h.log_lines();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["type"], "created");
    assert_eq!(log[0]["actor"], "");
    assert_eq!(log[0]["name"], "proc-1");
    assert_eq!(log[0]["id"], id.as_str());
}

#[test]
fn env_layers_later_wins() {
    require_tmux!();
    let h = Harness::new();
    let mut config = h.config();
    config.env = vec![
        ("A".into(), "config".into()),
        ("FROM_PLAN".into(), "config".into()),
        ("B".into(), "config".into()),
    ];
    let mut core = tx_core::TxCore::open(config).unwrap();
    core.register_engine(Box::new(common::ShellStub)).unwrap();
    let cwd = h.workdir("a");
    let mut opts = agent("agent-1", &cwd);
    opts.env = vec![
        ("B".into(), "opts".into()),
        ("TX_SESSION_ID".into(), "opts".into()),
    ];
    let id = core.create_session(opts).unwrap();
    let env = env_of(&cwd);
    assert_eq!(get(&env, "A"), Some("config"));
    assert_eq!(get(&env, "B"), Some("opts"));
    assert_eq!(get(&env, "FROM_PLAN"), Some("plan"));
    assert_eq!(
        get(&env, "TX_SESSION_ID"),
        Some("opts"),
        "option env may override even TX_SESSION_ID"
    );
    assert_eq!(get(&env, "STUB_EFFORT"), Some("none"));
    let _ = id;
}

#[test]
fn agent_session_gets_profile_plan_and_workspace() {
    require_tmux!();
    let h = Harness::new();
    let roles = h.dir.path().join("roles");
    std::fs::create_dir_all(&roles).unwrap();
    std::fs::write(roles.join("dev.md"), "# Dev\nbe careful\n").unwrap();
    let mut config = h.config();
    config.profile_dirs = vec![roles];
    let mut core = tx_core::TxCore::open(config).unwrap();
    core.register_engine(Box::new(common::ShellStub)).unwrap();
    let cwd = h.workdir("a");
    let id = core
        .create_session(
            CreateOptions::new(
                "agent-1",
                cwd.clone(),
                LaunchOptions::agent(STUB_ID)
                    .profile(["dev"])
                    .model("m")
                    .effort(tx_core::Effort::Max)
                    .initial_prompt("start"),
            )
            .tags(["t1"]),
        )
        .unwrap();
    let env = env_of(&cwd);
    let profile_file = h.state_dir().join(format!("profiles/{id}.md"));
    assert_eq!(
        get(&env, "STUB_PROFILE"),
        Some(profile_file.to_str().unwrap())
    );
    assert_eq!(
        std::fs::read_to_string(&profile_file).unwrap(),
        "# Dev\nbe careful"
    );
    assert_eq!(get(&env, "STUB_EFFORT"), Some("max"));
    assert_eq!(
        std::fs::read_to_string(cwd.join("stub-marker.txt")).unwrap(),
        "# Dev\nbe careful"
    );
    assert!(wait_for(Duration::from_secs(5), || cwd
        .join("prompt.txt")
        .exists()));
    assert_eq!(
        std::fs::read_to_string(cwd.join("prompt.txt")).unwrap(),
        "start\n"
    );
    let session = core.get_session(&id).unwrap();
    assert!(matches!(
        session.launch,
        Launch::Agent { ref engine, ref profile, ref model, effort: Some(tx_core::Effort::Max), .. }
            if engine == STUB_ID && *profile == ["dev"] && model.as_deref() == Some("m")
    ));
    assert_eq!(session.tags, ["t1"]);
}

#[test]
fn resume_reaches_the_engine_and_the_record() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let cwd = h.workdir("r");
    let mut opts = agent("resumed", &cwd);
    opts.launch = LaunchOptions::agent(STUB_ID).resume("chat-99");
    let id = core.create_session(opts).unwrap();
    let env = env_of(&cwd);
    assert_eq!(get(&env, "STUB_RESUME"), Some("chat-99"));
    let session = core.get_session(&id).unwrap();
    assert!(
        matches!(session.launch, Launch::Agent { ref resume, .. } if resume.as_deref() == Some("chat-99"))
    );
    let fresh = core
        .create_session(agent("fresh", &h.workdir("f")))
        .unwrap();
    assert!(
        matches!(core.get_session(&fresh).unwrap().launch, Launch::Agent { ref resume, .. } if resume.is_none())
    );
}

#[test]
fn failures_before_launch_leave_nothing_behind() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let cwd = h.workdir("a");

    let mut opts = agent("x", &cwd);
    opts.launch = LaunchOptions::agent("ghost");
    assert!(
        matches!(core.create_session(opts).unwrap_err(), CoreError::UnknownEngine(e) if e == "ghost")
    );

    let mut opts = agent("x", &cwd);
    opts.launch = LaunchOptions::agent(STUB_ID).profile(["missing"]);
    assert!(matches!(
        core.create_session(opts).unwrap_err(),
        CoreError::Profile(_)
    ));

    let mut opts = agent("x", &cwd);
    opts.launch = LaunchOptions::agent(STUB_ID).model("refuse");
    assert!(matches!(
        core.create_session(opts).unwrap_err(),
        CoreError::Engine(_)
    ));

    assert!(matches!(
        core.create_session(process("bad name", &cwd)).unwrap_err(),
        CoreError::InvalidName(_)
    ));
    let mut opts = process("x", &cwd);
    opts.env = vec![("A=B".into(), "1".into())];
    assert!(
        matches!(core.create_session(opts).unwrap_err(), CoreError::InvalidEnvKey(k) if k == "A=B")
    );
    let mut opts = process("x", &cwd);
    opts.env = vec![(String::new(), "1".into())];
    assert!(matches!(
        core.create_session(opts).unwrap_err(),
        CoreError::InvalidEnvKey(_)
    ));
    let mut opts = process("x", &cwd);
    opts.env = vec![("A".into(), "a\0b".into())];
    assert!(matches!(
        core.create_session(opts).unwrap_err(),
        CoreError::InvalidEnvKey(_)
    ));
    assert!(matches!(
        core.create_session(process("x", &h.dir.path().join("nope"))).unwrap_err(),
        CoreError::NotADirectory(p) if p == h.dir.path().join("nope")
    ));

    assert!(h.live_names().is_empty());
    assert!(
        core.list_sessions(&ListFilter::default())
            .unwrap()
            .is_empty()
    );
    assert!(
        !h.state_dir()
            .join("profiles")
            .read_dir()
            .unwrap()
            .any(|_| true)
    );
    assert!(h.log_lines().is_empty());
    assert!(!cwd.join("stub-marker.txt").exists());
}

#[test]
fn a_program_that_cannot_start_leaves_no_record() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    // new-session itself fails when the command cannot be parsed? No: a missing program exits the
    // pane at once. Use an invalid tmux option instead: a name tmux rejects.
    let mut opts = process("x", &cwd);
    opts.launch = LaunchOptions::process("", Vec::<String>::new());
    let error = core.create_session(opts).unwrap_err();
    assert!(
        matches!(error, CoreError::InvalidName(_)) || matches!(error, CoreError::Tmux(_)),
        "{error:?}"
    );
    assert!(
        core.list_sessions(&ListFilter::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_program_that_exits_at_once_yields_an_exited_record() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let mut opts = process("quick", &h.workdir("q"));
    opts.launch = LaunchOptions::process("true", Vec::<String>::new());
    // Not every run loses the race against tmux; whichever way it goes, the record is truthful.
    let id = core.create_session(opts).unwrap();
    let session = core.refresh_session(&id).unwrap().session().clone();
    assert!(
        matches!(session.lifecycle, Lifecycle::Exited),
        "{session:?}"
    );
    assert!(session.ended_at.is_some());
    assert!(
        core.create_session(process("quick", &h.workdir("q2")))
            .is_ok(),
        "the name is free again"
    );
}

#[test]
fn names_are_unique_among_running_sessions_only() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    let first = core.create_session(process("dup", &cwd)).unwrap();
    let error = core
        .create_session(process("dup", &h.workdir("b")))
        .unwrap_err();
    assert!(
        matches!(&error, CoreError::NameTaken(n) if n == "dup"),
        "{error:?}"
    );
    assert_eq!(h.live_names(), ["dup"]);
    core.stop_session(&first).unwrap();
    let second = core
        .create_session(process("dup", &h.workdir("b")))
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(core.resolve("dup").unwrap(), second);
}

#[test]
fn a_foreign_tmux_session_holding_the_name_is_name_taken() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    core.create_session(process("seed", &cwd)).unwrap();
    h.tmux_ok(&["new-session", "-d", "-s", "foreign", "sleep", "30"]);
    let error = core.create_session(process("foreign", &cwd)).unwrap_err();
    assert!(
        matches!(&error, CoreError::NameTaken(n) if n == "foreign"),
        "{error:?}"
    );
    assert_eq!(core.list_sessions(&ListFilter::default()).unwrap().len(), 1);
}

#[test]
fn names_special_to_tmux_are_fine() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    for name in ["-x", "3", "--", "_"] {
        let id = core
            .create_session(process(name, &h.workdir(name)))
            .unwrap();
        assert_eq!(core.resolve(name).unwrap(), id);
        assert!(core.get_execution_location(&id).is_ok(), "{name}");
        let renamed = core.rename_session(&id, &format!("{name}-r")).unwrap();
        assert_eq!(renamed.name, format!("{name}-r"));
    }
    let mut names = h.live_names();
    names.sort();
    assert_eq!(names, ["---r", "-x-r", "3-r", "_-r"]);
}

#[test]
fn list_and_resolve() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    let root = core.create_session(process("root", &cwd)).unwrap();
    let mut child = process("child", &cwd);
    child.parent = Some(root.clone());
    child.tags = vec!["t".into(), "u".into()];
    let child = core.create_session(child).unwrap();
    let other = core.create_session(process("other", &cwd)).unwrap();
    core.stop_session(&other).unwrap();

    let all = core.list_sessions(&ListFilter::default()).unwrap();
    assert_eq!(all, [root.clone(), child.clone(), other.clone()]);
    let by_parent = core
        .list_sessions(&ListFilter::new().parent(root.clone()))
        .unwrap();
    assert_eq!(by_parent, std::slice::from_ref(&child));
    let by_tag = core.list_sessions(&ListFilter::new().tag("u")).unwrap();
    assert_eq!(by_tag, std::slice::from_ref(&child));
    let running = core
        .list_sessions(&ListFilter::new().lifecycle(Lifecycle::Running))
        .unwrap();
    assert_eq!(running, [root.clone(), child.clone()]);
    let stopped = core
        .list_sessions(&ListFilter::new().lifecycle(Lifecycle::Stopped))
        .unwrap();
    assert_eq!(stopped, std::slice::from_ref(&other));
    let none = core.list_sessions(&ListFilter::new().tag("zzz")).unwrap();
    assert!(none.is_empty());

    assert_eq!(core.resolve(child.as_str()).unwrap(), child);
    assert_eq!(core.resolve("child").unwrap(), child);
    assert_eq!(core.resolve("other").unwrap(), other);
    assert!(matches!(core.resolve("nope").unwrap_err(), CoreError::NotFound(n) if n == "nope"));
    assert!(matches!(
        core.resolve("").unwrap_err(),
        CoreError::NotFound(_)
    ));
    assert!(matches!(
        core.resolve("a/b").unwrap_err(),
        CoreError::NotFound(_)
    ));
    assert!(matches!(
        core.get_session(&"zzzz".parse::<SessionId>().unwrap())
            .unwrap_err(),
        CoreError::NotFound(_)
    ));
}

#[test]
fn resolve_prefers_running_then_most_recent() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    let first = core.create_session(process("n", &cwd)).unwrap();
    core.stop_session(&first).unwrap();
    let second = core.create_session(process("n", &cwd)).unwrap();
    core.stop_session(&second).unwrap();
    assert_eq!(
        core.resolve("n").unwrap(),
        second,
        "most recent when none runs"
    );
    let third = core.create_session(process("n", &cwd)).unwrap();
    assert_eq!(core.resolve("n").unwrap(), third, "the running one wins");
}

#[test]
fn reads_never_touch_tmux() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    let id = core.create_session(process("r", &cwd)).unwrap();
    h.tmux(&["kill-server"]);
    assert_eq!(core.get_session(&id).unwrap().lifecycle, Lifecycle::Running);
    assert_eq!(core.resolve("r").unwrap(), id);
    assert_eq!(
        core.list_sessions(&ListFilter::default()).unwrap(),
        std::slice::from_ref(&id)
    );
    assert!(
        h.live_names().is_empty(),
        "reads did not restart the server"
    );
    let _: Session = core.get_session(&id).unwrap();
}

#[test]
fn reopen_sees_the_same_sessions() {
    require_tmux!();
    let h = Harness::new();
    let id = {
        let core = h.open();
        core.create_session(process("keep", &h.workdir("k")))
            .unwrap()
    };
    let core = h.open();
    assert_eq!(core.get_session(&id).unwrap().lifecycle, Lifecycle::Running);
    assert!(core.get_execution_location(&id).is_ok());
    assert_eq!(h.live_names(), ["keep"]);
}
