//! The public surface plugin authors build against: builders instead of struct literals,
//! `Send + Sync` cores and adapters, non-exhaustive enums.

use std::path::{Path, PathBuf};

use tx_core::{
    CoreConfig, CreateOptions, Directories, EngineKind, EventType, Input, Key, Launch,
    LaunchOptions, LaunchPlan, LaunchRequest, Lifecycle, ListFilter, LlmEngine, LlmEngineState,
    Profile, ResolvedProfile, SendOptions, Sender, Session, SessionId, TxCore, WorkspaceAction,
};

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn core_and_adapters_are_send_and_sync() {
    assert_send_sync::<TxCore>();
    assert_send_sync::<Box<dyn LlmEngine>>();
}

#[test]
fn launch_plan_builder() {
    let plan = LaunchPlan::new("prog")
        .arg("-a")
        .args(["b", "c"])
        .env("K", "v")
        .workspace(WorkspaceAction::Write {
            path: "f".into(),
            content: "x".into(),
        });
    assert_eq!(plan.program, "prog");
    assert_eq!(plan.args, ["-a", "b", "c"]);
    assert_eq!(plan.env, [("K".to_owned(), "v".to_owned())]);
    assert_eq!(plan.workspace.len(), 1);
    assert_eq!(LaunchPlan::new("p"), LaunchPlan::default().program("p"));
}

#[test]
fn launch_request_builder() {
    let dir = tempfile::tempdir().unwrap();
    let profile = Profile::write(
        ResolvedProfile {
            text: "t".into(),
            skill_dirs: vec![],
        },
        &dir.path().join("p.md"),
    )
    .unwrap();
    let session = record(dir.path());
    let hooks = ["txi".to_owned(), "hook".to_owned()];
    let request = LaunchRequest::new(&session, &profile).hook_command(&hooks);
    assert_eq!(request.session.id, session.id);
    assert_eq!(request.session.cwd, dir.path());
    assert_eq!(request.profile.text(), "t");
    assert_eq!(request.hook_command, &hooks);
    let bare = LaunchRequest::new(&session, &profile);
    assert!(bare.hook_command.is_empty());
}

#[test]
fn input_is_a_line_or_a_key() {
    assert_eq!(
        Input::line("hi"),
        Input::Text {
            text: "hi".into(),
            submit: true
        }
    );
    assert_ne!(Input::line("hi"), Input::Key(Key::Enter));
    assert_eq!(Key::ControlC.tmux_name(), "C-c");
}

/// A minimal agent record in `cwd`.
fn record(cwd: &Path) -> Session {
    Session {
        id: SessionId::mint(),
        name: "s".into(),
        parent: None,
        tags: Vec::new(),
        cwd: cwd.to_owned(),
        created_at: 0.0,
        ended_at: None,
        archived_at: None,
        last_modified_at: 0.0,
        lifecycle: Lifecycle::Running,
        launch: Launch::Agent {
            engine: EngineKind::Claude,
            profile: Vec::new(),
            model: None,
            effort: None,
            resume: None,
        },
        state: LlmEngineState::Idle,
        ext: Default::default(),
    }
}

#[test]
fn create_options_and_launch_options_builders() {
    let parent: SessionId = "p".parse().unwrap();
    let opts = CreateOptions::new(
        "w",
        "/w",
        LaunchOptions::agent(EngineKind::Claude)
            .profile(["dev", "ops"])
            .option("model", "opus")
            .option("effort", "max")
            .option("model", "sonnet")
            .initial_prompt("start"),
    )
    .parent(parent.clone())
    .tags(["a", "b"])
    .env("K", "v");
    assert_eq!(opts.name, "w");
    assert_eq!(opts.cwd, Path::new("/w"));
    assert_eq!(opts.parent, Some(parent));
    assert_eq!(opts.tags, ["a", "b"]);
    assert_eq!(opts.env, [("K".to_owned(), "v".to_owned())]);
    let LaunchOptions::Agent {
        engine,
        profile,
        options,
        initial_prompt,
        ..
    } = &opts.launch
    else {
        panic!("{:?}", opts.launch)
    };
    assert_eq!(*engine, EngineKind::Claude);
    assert_eq!(profile, &["dev", "ops"]);
    // A repeated key replaces; `options(..)` replaces the whole object.
    assert_eq!(
        *options,
        serde_json::json!({"model": "sonnet", "effort": "max"})
    );
    let replaced = LaunchOptions::agent(EngineKind::Claude)
        .option("model", "opus")
        .options(serde_json::json!({"effort": "low"}));
    assert!(matches!(
        &replaced,
        LaunchOptions::Agent { options, .. } if *options == serde_json::json!({"effort": "low"})
    ));
    let bare = LaunchOptions::agent(EngineKind::Claude);
    assert!(matches!(
        &bare,
        LaunchOptions::Agent { options, .. } if *options == serde_json::json!({})
    ));
    assert_eq!(initial_prompt, "start");
}

#[test]
fn config_filter_and_send_builders() {
    let config = CoreConfig::new(Directories::under("/s"))
        .tmux_socket("/k.sock")
        .env("A", "1")
        .profile_dirs(["/p1", "/p2"])
        .skill_dirs(["/sk"]);
    assert_eq!(config.socket_path(), PathBuf::from("/k.sock"));
    assert_eq!(config.env, [("A".to_owned(), "1".to_owned())]);
    assert_eq!(
        config.profile_dirs,
        [PathBuf::from("/p1"), PathBuf::from("/p2")]
    );
    assert_eq!(config.skill_dirs, [PathBuf::from("/sk")]);
    let id: SessionId = "x".parse().unwrap();
    let filter = ListFilter::new()
        .parent(id.clone())
        .tag("t")
        .lifecycle(Lifecycle::Exited);
    assert_eq!(filter.parent, Some(id.clone()));
    assert_eq!(filter.tag.as_deref(), Some("t"));
    assert_eq!(filter.lifecycle, Some(Lifecycle::Exited));
    assert_eq!(ListFilter::new(), ListFilter::default());
    assert_eq!(
        SendOptions::from_agent(id.clone()).sender,
        Sender::Agent { id }
    );
    assert_eq!(SendOptions::default().sender, Sender::User { via: None });
    assert_eq!(
        SendOptions::from(Sender::User {
            via: Some("notes".into())
        })
        .sender,
        Sender::User {
            via: Some("notes".into())
        }
    );
}

#[test]
fn event_types_are_a_slice() {
    let all: &[EventType] = EventType::ALL;
    assert_eq!(all.len(), 12);
}
