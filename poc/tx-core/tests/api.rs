//! The public surface plugin authors build against: builders instead of struct literals,
//! `Send + Sync` cores and adapters, non-exhaustive enums.

use std::path::{Path, PathBuf};

use tx_core::{
    CoreConfig, CreateOptions, Directories, Effort, EventType, LaunchOptions, LaunchPlan,
    LaunchRequest, Lifecycle, ListFilter, LlmEngine, Profile, ResolvedProfile, SendOptions,
    SessionId, TxCore, WorkspaceAction,
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
    let request = LaunchRequest::new(dir.path(), &profile)
        .model("m")
        .effort(Effort::Low)
        .initial_prompt("go")
        .resume("chat-1");
    assert_eq!(request.cwd, dir.path());
    assert_eq!(request.profile.text(), "t");
    assert_eq!(request.model, Some("m"));
    assert_eq!(request.effort, Some(Effort::Low));
    assert_eq!(request.initial_prompt, Some("go"));
    assert_eq!(request.resume, Some("chat-1"));
    let bare = LaunchRequest::new(dir.path(), &profile);
    assert!(
        bare.model.is_none()
            && bare.effort.is_none()
            && bare.initial_prompt.is_none()
            && bare.resume.is_none()
    );
}

#[test]
fn create_options_and_launch_options_builders() {
    let parent: SessionId = "p".parse().unwrap();
    let opts = CreateOptions::new(
        "w",
        "/w",
        LaunchOptions::agent("claude")
            .profile(["dev", "ops"])
            .model("opus")
            .effort(Effort::Max)
            .initial_prompt("start")
            .resume("chat-9"),
    )
    .parent(parent.clone())
    .tags(["a", "b"])
    .env("K", "v");
    assert_eq!(opts.name, "w");
    assert_eq!(opts.cwd, Path::new("/w"));
    assert_eq!(opts.parent, Some(parent));
    assert_eq!(opts.tags, ["a", "b"]);
    assert_eq!(opts.env, [("K".to_owned(), "v".to_owned())]);
    match &opts.launch {
        LaunchOptions::Agent {
            engine,
            profile,
            model,
            effort,
            initial_prompt,
            resume,
            ..
        } => {
            assert_eq!(engine, "claude");
            assert_eq!(profile, &["dev", "ops"]);
            assert_eq!(model.as_deref(), Some("opus"));
            assert_eq!(*effort, Some(Effort::Max));
            assert_eq!(initial_prompt.as_deref(), Some("start"));
            assert_eq!(resume.as_deref(), Some("chat-9"));
        }
        other => panic!("{other:?}"),
    }
    let process = LaunchOptions::process("sh", ["-c", "true"]);
    assert!(
        matches!(&process, LaunchOptions::Process { program, args, .. } if program == "sh" && args == &["-c", "true"])
    );
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
    assert_eq!(SendOptions::from_agent(id.clone()).sender, Some(id));
    assert_eq!(SendOptions::default().sender, None);
}

#[test]
fn event_types_are_a_slice() {
    let all: &[EventType] = EventType::ALL;
    assert_eq!(all.len(), 8);
}
