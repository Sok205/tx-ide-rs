//! `TxCore::open`, `create_session` and the reads, against a private tmux server.

mod common;

use std::time::Duration;

use common::{
    Harness, STUB_ID, STUB_KIND, agent, cwd_of, exiting, git, git_ok, git_worktrees, wait_for,
};
use tx_core::worktree::repository_key;
use tx_core::{
    CoreError, CreateOptions, EngineKind, LaunchOptions, ListFilter, LlmEngineState, Session,
    SessionId, WorktreeError,
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
fn an_agent_runs_detached_with_the_tx_environment() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("p");
    let id = core.create_session(agent("agent-1", &repository)).unwrap();
    let session = core.get_session(&id).unwrap();
    assert_ne!(session.state, LlmEngineState::Ended);
    assert_eq!(session.name, "agent-1");
    assert_ne!(
        session.working_directory,
        repository.canonicalize().unwrap(),
        "runs in a worktree of the checkout"
    );
    assert_eq!(session.engine, STUB_KIND);
    assert_eq!(h.live_names(), ["agent-1"]);
    assert_eq!(
        h.tmux_ok(&["show-options", "-v", "-t", "=agent-1:", "@tx-id"])
            .trim(),
        id.as_str()
    );
    let env = env_of(&cwd_of(&core, &id));
    assert_eq!(get(&env, "TX_SESSION_ID"), Some(id.as_str()));
    assert_eq!(
        get(&env, "TX_TMUX_SOCKET"),
        Some(h.socket().to_str().unwrap())
    );
    let directories = h.directories();
    for (key, directory) in [
        ("TX_CONFIG_DIR", &directories.config),
        ("TX_DATA_DIR", &directories.data),
        ("TX_STATE_DIR", &directories.state),
        ("TX_CACHE_DIR", &directories.cache),
        ("TX_RUN_DIR", &directories.run),
    ] {
        assert_eq!(get(&env, key), Some(directory.to_str().unwrap()), "{key}");
    }
    assert_eq!(get(&env, "TX_CORE_HOME"), None);
    let location = core.get_execution_location(&id).unwrap();
    assert_eq!(location.socket, h.socket());
    assert!(location.session_target.starts_with('$'));
    assert!(location.pane_target.starts_with('%'));
    let log = h.log_lines();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0]["type"], "created");
    assert_eq!(log[0]["actor"], "");
    assert_eq!(log[0]["name"], "agent-1");
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
    let mut opts = agent("agent-1", &h.repository("a"));
    opts.env = vec![
        ("B".into(), "opts".into()),
        ("TX_SESSION_ID".into(), "opts".into()),
    ];
    let id = core.create_session(opts).unwrap();
    let env = env_of(&cwd_of(&core, &id));
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
    let repository = h.repository("a");
    let id = core
        .create_session(
            CreateOptions::new(
                "agent-1",
                repository.clone(),
                LaunchOptions::agent(STUB_KIND)
                    .profile(["dev"])
                    .option("model", "m")
                    .option("effort", "max")
                    .initial_prompt("start"),
            )
            .tags(["t1"]),
        )
        .unwrap();
    // The engine's workspace, the profile and the process all land in the agent's worktree.
    let cwd = cwd_of(&core, &id);
    assert_ne!(cwd, repository.canonicalize().unwrap());
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
    assert!(!repository.join("stub-marker.txt").exists());
    assert!(!repository.join("env.txt").exists());
    assert_eq!(git_ok(&repository, &["status", "--porcelain"]), "");
    let session = core.get_session(&id).unwrap();
    // The options are engine data, not record fields.
    assert_eq!(session.engine, STUB_KIND);
    assert_eq!(session.roles, ["dev"]);
    assert_eq!(
        session.ext.get(STUB_ID),
        Some(&serde_json::json!({"model": "m", "effort": "max"}))
    );
    assert_eq!(session.tags, ["t1"]);
}

/// The engine is handed the record being created (its id), and the engine data its plan carries
/// is in the record from the first save.
#[test]
fn the_engine_sees_the_record_and_its_data_lands_in_ext() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let mut opts = agent("named", &h.repository("d"));
    opts.launch = LaunchOptions::agent(STUB_KIND)
        .option("model", "data")
        .initial_prompt("start");
    let id = core.create_session(opts).unwrap();
    let env = env_of(&cwd_of(&core, &id));
    assert_eq!(get(&env, "STUB_MODE"), Some("start"));
    assert_eq!(get(&env, "STUB_SESSION_ID"), Some(id.as_str()));
    let session = core.get_session(&id).unwrap();
    assert_eq!(
        session.ext.get(STUB_ID),
        Some(&serde_json::json!({"conversation_id": id.as_str(), "model": "data"}))
    );
    // No options and no data in the plan: the engine's key holds the empty object `configure`
    // returned.
    let plain = core
        .create_session(agent("plain", &h.repository("p")))
        .unwrap();
    assert_eq!(
        core.get_session(&plain).unwrap().ext.get(STUB_ID),
        Some(&serde_json::json!({}))
    );
}

#[test]
fn failures_before_launch_leave_nothing_behind() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let cwd = h.repository("a");

    let mut opts = agent("x", &cwd);
    opts.launch = LaunchOptions::agent(EngineKind::Codex);
    assert!(matches!(
        core.create_session(opts).unwrap_err(),
        CoreError::EngineDisabled(EngineKind::Codex)
    ));

    let mut opts = agent("x", &cwd);
    opts.launch = LaunchOptions::agent(STUB_KIND)
        .initial_prompt("start")
        .profile(["missing"]);
    assert!(matches!(
        core.create_session(opts).unwrap_err(),
        CoreError::Profile(_)
    ));

    for launch in [
        LaunchOptions::agent(STUB_KIND),
        LaunchOptions::agent(STUB_KIND).initial_prompt(""),
        LaunchOptions::agent(STUB_KIND).initial_prompt(" \n "),
    ] {
        let mut opts = agent("x", &cwd);
        opts.launch = launch;
        assert!(matches!(
            core.create_session(opts).unwrap_err(),
            CoreError::PromptRequired
        ));
    }

    // The engine refuses after the worktree was made: the worktree goes with the failure.
    let mut opts = agent("x", &cwd);
    opts.launch = LaunchOptions::agent(STUB_KIND)
        .initial_prompt("start")
        .option("model", "refuse");
    assert!(matches!(
        core.create_session(opts).unwrap_err(),
        CoreError::Engine(tx_core::EngineError::Refused(ref text)) if text == "model refused"
    ));
    assert!(core.list_sessions(&Default::default()).unwrap().is_empty());

    // Not a checkout: git's own words, and nothing made.
    let plain = h.workdir("plain");
    let error = core.create_session(agent("x", &plain)).unwrap_err();
    assert!(
        matches!(&error, CoreError::Worktree(WorktreeError::Git(message)) if message.starts_with("fatal: not a git repository")),
        "{error:?}"
    );

    assert!(matches!(
        core.create_session(agent("bad name", &cwd)).unwrap_err(),
        CoreError::InvalidName(_)
    ));
    let mut opts = agent("x", &cwd);
    opts.env = vec![("A=B".into(), "1".into())];
    assert!(
        matches!(core.create_session(opts).unwrap_err(), CoreError::InvalidEnvKey(k) if k == "A=B")
    );
    let mut opts = agent("x", &cwd);
    opts.env = vec![(String::new(), "1".into())];
    assert!(matches!(
        core.create_session(opts).unwrap_err(),
        CoreError::InvalidEnvKey(_)
    ));
    let mut opts = agent("x", &cwd);
    opts.env = vec![("A".into(), "a\0b".into())];
    assert!(matches!(
        core.create_session(opts).unwrap_err(),
        CoreError::InvalidEnvKey(_)
    ));
    assert!(matches!(
        core.create_session(agent("x", &h.dir.path().join("nope"))).unwrap_err(),
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
    assert_eq!(git_worktrees(&cwd), [cwd.canonicalize().unwrap()]);
    assert_eq!(h.worktree_count(), 0);
}

#[test]
fn every_agent_gets_its_own_detached_worktree_under_a_free_name() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("repo");
    let head = git_ok(&repository, &["rev-parse", "HEAD"]);
    let key = repository_key(
        &repository.canonicalize().unwrap(),
        &repository.canonicalize().unwrap().join(".git"),
    );
    let expected = |name: &str| {
        h.worktrees_dir()
            .join(&key)
            .join(format!("repo--{name}"))
            .canonicalize()
            .unwrap()
    };

    let first = core.create_session(agent("w", &repository)).unwrap();
    let session = core.get_session(&first).unwrap();
    assert_eq!(session.name, "w");
    assert_eq!(session.working_directory, expected("w"));
    assert_eq!(
        git_ok(&session.working_directory, &["rev-parse", "HEAD"]),
        head
    );
    assert!(
        !git(&session.working_directory, &["symbolic-ref", "-q", "HEAD"])
            .status
            .success(),
        "detached"
    );
    assert!(session.working_directory.join("README.md").is_file());
    assert_eq!(h.live_names(), ["w"]);

    // The same name while the first runs: the next free one, for the record and the tree.
    let second = core.create_session(agent("w", &repository)).unwrap();
    let session = core.get_session(&second).unwrap();
    assert_eq!(session.name, "w-2");
    assert_eq!(session.working_directory, expected("w-2"));
    assert_eq!(
        git_ok(&session.working_directory, &["rev-parse", "HEAD"]),
        head
    );
    assert_eq!(
        git_worktrees(&repository),
        [
            repository.canonicalize().unwrap(),
            expected("w"),
            expected("w-2")
        ]
    );
    let mut names = h.live_names();
    names.sort();
    assert_eq!(names, ["w", "w-2"]);
    assert_eq!(core.resolve("w").unwrap(), first);
    assert_eq!(core.resolve("w-2").unwrap(), second);

    // Stopped: the name is free again, but its tree is still on disk, so the next is `w-3`.
    core.stop_session(&first).unwrap();
    let third = core.create_session(agent("w", &repository)).unwrap();
    assert_eq!(core.get_session(&third).unwrap().name, "w-3");
    assert!(expected("w").is_dir(), "stop never removes a worktree");
}

#[test]
fn a_launch_that_fails_after_the_worktree_removes_it() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("repo");
    let before = git_worktrees(&repository);
    // The name is free among records, so the worktree is made; then tmux refuses the duplicate.
    h.tmux_ok(&["new-session", "-d", "-s", "w", "sleep", "30"]);
    let error = core.create_session(agent("w", &repository)).unwrap_err();
    assert!(
        matches!(&error, CoreError::NameTaken(name) if name == "w"),
        "{error:?}"
    );
    assert_eq!(git_worktrees(&repository), before);
    assert_eq!(h.worktree_count(), 0);
    assert!(
        core.list_sessions(&ListFilter::default())
            .unwrap()
            .is_empty()
    );
    assert_eq!(h.live_names(), ["w"], "the foreign session is left alone");
}

#[test]
fn a_program_that_exits_at_once_yields_an_exited_record() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    // Not every run loses the race against tmux; whichever way it goes, the record is truthful.
    let id = core
        .create_session(exiting("quick", &h.repository("q")))
        .unwrap();
    let session = core.refresh_session(&id).unwrap().session().clone();
    assert!(session.state == LlmEngineState::Ended, "{session:?}");
    let repository = h.repository("q2");
    let held = core.create_session(exiting("quick", &repository)).unwrap();
    assert_eq!(
        core.get_session(&held).unwrap().name,
        "quick-2",
        "an exited record keeps its name"
    );
    core.archive_session(&id).unwrap();
    let freed = core.create_session(exiting("quick", &repository)).unwrap();
    assert_eq!(
        core.get_session(&freed).unwrap().name,
        "quick",
        "the name is free once archived"
    );
}

#[test]
fn names_are_unique_among_unarchived_sessions() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let first = core
        .create_session(agent("dup", &h.repository("a")))
        .unwrap();
    // The same name asked from another checkout: the next free one, never a clash.
    let other_repository = h.repository("b");
    let other = core
        .create_session(agent("dup", &other_repository))
        .unwrap();
    assert_eq!(core.get_session(&other).unwrap().name, "dup-2");
    let mut names = h.live_names();
    names.sort();
    assert_eq!(names, ["dup", "dup-2"]);
    core.stop_session(&first).unwrap();
    let third = core
        .create_session(agent("dup", &other_repository))
        .unwrap();
    assert_eq!(
        core.get_session(&third).unwrap().name,
        "dup-3",
        "a stopped record keeps its name"
    );
    core.archive_session(&first).unwrap();
    let second = core
        .create_session(agent("dup", &other_repository))
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        core.get_session(&second).unwrap().name,
        "dup",
        "archived, the name is free again"
    );
    assert_eq!(core.resolve("dup").unwrap(), second);
}

#[test]
fn a_foreign_tmux_session_holding_the_name_is_name_taken() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("a");
    core.create_session(agent("seed", &repository)).unwrap();
    h.tmux_ok(&["new-session", "-d", "-s", "foreign", "sleep", "30"]);
    let error = core
        .create_session(agent("foreign", &repository))
        .unwrap_err();
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
    let core = h.open_with_engine();
    let repository = h.repository("r");
    for name in ["-x", "3", "--", "_"] {
        let id = core.create_session(agent(name, &repository)).unwrap();
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
    let core = h.open_with_engine();
    let repository = h.repository("a");
    let root = core.create_session(agent("root", &repository)).unwrap();
    let mut child = agent("child", &repository);
    child.parent = Some(root.clone());
    child.tags = vec!["t".into(), "u".into()];
    let child = core.create_session(child).unwrap();
    let other = core.create_session(agent("other", &repository)).unwrap();
    core.stop_session(&other).unwrap();

    let all = core.list_sessions(&ListFilter::default()).unwrap();
    assert_eq!(all, [root.clone(), child.clone(), other.clone()]);
    let by_parent = core
        .list_sessions(&ListFilter::new().parent(root.clone()))
        .unwrap();
    assert_eq!(by_parent, std::slice::from_ref(&child));
    let by_tag = core.list_sessions(&ListFilter::new().tag("u")).unwrap();
    assert_eq!(by_tag, std::slice::from_ref(&child));
    let running = core.list_sessions(&ListFilter::new().running()).unwrap();
    assert_eq!(running, [root.clone(), child.clone()]);
    let stopped = core.list_sessions(&ListFilter::new().ended()).unwrap();
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
fn resolve_prefers_unarchived_then_most_recent_archived() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("a");
    let first = core.create_session(agent("n", &repository)).unwrap();
    core.archive_session(&first).unwrap();
    let second = core.create_session(agent("n", &repository)).unwrap();
    core.archive_session(&second).unwrap();
    assert_eq!(
        core.resolve("n").unwrap(),
        second,
        "the most recent archived one when none is unarchived"
    );
    let third = core.create_session(agent("n", &repository)).unwrap();
    core.stop_session(&third).unwrap();
    assert_eq!(
        core.resolve("n").unwrap(),
        third,
        "the unarchived one wins, running or not"
    );
}

#[test]
fn reads_never_touch_tmux() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(agent("r", &h.repository("a"))).unwrap();
    h.tmux(&["kill-server"]);
    assert_ne!(core.get_session(&id).unwrap().state, LlmEngineState::Ended);
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
        let core = h.open_with_engine();
        core.create_session(agent("keep", &h.repository("k")))
            .unwrap()
    };
    let core = h.open();
    assert_ne!(core.get_session(&id).unwrap().state, LlmEngineState::Ended);
    assert!(core.get_execution_location(&id).is_ok());
    assert_eq!(h.live_names(), ["keep"]);
}

#[test]
fn fork_makes_a_new_agent_in_its_own_worktree_from_the_source() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("repo");
    let mut opts = agent("src", &repository);
    opts.launch = LaunchOptions::agent(STUB_KIND)
        .option("effort", "high")
        .initial_prompt("start");
    opts.tags = vec!["t1".into()];
    let source_id = core.create_session(opts).unwrap();
    let source = core.get_session(&source_id).unwrap();
    let head = git_ok(&source.working_directory, &["rev-parse", "HEAD"]);
    // Uncommitted work in the source's worktree stays there.
    std::fs::write(source.working_directory.join("draft.txt"), "wip").unwrap();

    let fork_id = core
        .fork_session(&source_id, tx_core::ForkOptions::new("go on"))
        .unwrap();
    assert_ne!(fork_id, source_id);
    let fork = core.get_session(&fork_id).unwrap();
    assert_eq!(fork.name, "src-fork");
    assert_eq!(fork.parent_id, Some(source_id.clone()));
    assert_eq!(fork.tags, ["t1"]);
    assert_ne!(fork.state, LlmEngineState::Ended);
    assert_eq!(fork.state, tx_core::LlmEngineState::Starting);
    assert_eq!(fork.engine, STUB_KIND);
    assert_ne!(fork.working_directory, source.working_directory);
    assert_eq!(
        git_ok(&fork.working_directory, &["rev-parse", "HEAD"]),
        head
    );
    assert!(!fork.working_directory.join("draft.txt").exists());
    assert!(
        fork.working_directory.join("stub-marker.txt").is_file(),
        "workspace on the fork's worktree"
    );
    // The fork's engine data starts as a copy of the source's (the stub's plan adds nothing
    // without model `data`), so the source's effort reached the fork's launch.
    assert_eq!(
        fork.ext.get(STUB_ID),
        Some(&serde_json::json!({"effort": "high"}))
    );
    let env = env_of(&fork.working_directory);
    assert_eq!(get(&env, "STUB_MODE"), Some("fork"));
    assert_eq!(get(&env, "STUB_RESUME"), Some(source_id.as_str()));
    assert_eq!(get(&env, "STUB_SESSION_ID"), Some(fork_id.as_str()));
    assert_eq!(
        get(&env, "STUB_EFFORT"),
        Some("high"),
        "the source's effort"
    );
    assert!(wait_for(Duration::from_secs(5), || fork
        .working_directory
        .join("prompt.txt")
        .exists()));
    assert_eq!(
        std::fs::read_to_string(fork.working_directory.join("prompt.txt")).unwrap(),
        "go on\n"
    );
    // The source is untouched and both run.
    assert_eq!(core.get_session(&source_id).unwrap(), source);
    let mut names = h.live_names();
    names.sort();
    assert_eq!(names, ["src", "src-fork"]);
    let types: Vec<String> = h
        .log_lines()
        .iter()
        .map(|line| line["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(types, ["created", "forked"]);
    let forked = &h.log_lines()[1];
    assert_eq!(forked["id"], fork_id.as_str());
    assert_eq!(forked["name"], "src-fork");
    assert_eq!(forked["msg"], source_id.as_str());

    // A chosen name, tags and an engine option; a taken name gets the next free one, like
    // create. The option goes through the engine's `configure` over the copied data.
    let second = core
        .fork_session(
            &source_id,
            tx_core::ForkOptions::new("again")
                .name("src-fork")
                .tags(["t2"])
                .option("effort", "low"),
        )
        .unwrap();
    let second = core.get_session(&second).unwrap();
    assert_eq!(second.name, "src-fork-2");
    assert_eq!(second.tags, ["t2"]);
    assert_eq!(second.parent_id, Some(source_id.clone()));
    assert_eq!(
        get(&env_of(&second.working_directory), "STUB_EFFORT"),
        Some("low")
    );
    assert_eq!(h.worktree_count(), 3);
    // An option the engine refuses: its answer, and nothing is left of the fork.
    let error = core
        .fork_session(
            &source_id,
            tx_core::ForkOptions::new("no").option("model", "refuse"),
        )
        .unwrap_err();
    assert!(matches!(error, CoreError::Engine(_)), "{error:?}");
    assert_eq!(h.worktree_count(), 3);
}

#[test]
fn fork_refuses_a_missing_prompt_and_an_unknown_source() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("repo");
    let source_id = core.create_session(agent("src", &repository)).unwrap();
    let before = git_worktrees(&repository);
    for prompt in ["", "  \n"] {
        let error = core
            .fork_session(&source_id, tx_core::ForkOptions::new(prompt))
            .unwrap_err();
        assert!(matches!(error, CoreError::PromptRequired), "{error:?}");
    }
    let ghost: SessionId = "no-such".parse().unwrap();
    assert!(matches!(
        core.fork_session(&ghost, tx_core::ForkOptions::new("go"))
            .unwrap_err(),
        CoreError::NotFound(_)
    ));
    assert_eq!(git_worktrees(&repository), before);
    assert_eq!(h.worktree_count(), 1);
    assert_eq!(core.list_sessions(&ListFilter::default()).unwrap().len(), 1);
    assert_eq!(h.live_names(), ["src"]);
    let types: Vec<String> = h
        .log_lines()
        .iter()
        .map(|line| line["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(types, ["created"]);
}
