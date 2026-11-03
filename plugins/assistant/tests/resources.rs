//! The assistant plugin's resources and create options, through the core.

use tx_assistant::{AssistantPlugin, Settings, create_options, latest_ended};
use tx_core::{CoreConfig, LaunchOptions, PluginConfig, TxCore};

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("tx")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
        dir.path().join("state"),
    )))
    .unwrap();
    core.plug(vec![Box::new(AssistantPlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_key_fragment_is_a_resource_and_the_role_comes_from_the_core() {
    let (_dir, core) = core(&PluginConfig::default());
    assert!(core.plugin_resources("agents/").is_empty());
    let role = core
        .resources("agents/")
        .into_iter()
        .find(|r| r.name == "agents/TX-ASSISTANT.md")
        .expect("shipped by tx-core");
    assert_eq!(role.plugin, "tx-core");
    assert!(role.bytes.starts_with(b"# TX-ASSISTANT role"));
    let keys = core.resources("keys/");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].name, "keys/assistant.tmux");
    let fragment = String::from_utf8_lossy(keys[0].bytes);
    assert!(fragment.contains("# keys: /\n"), "declares prefix+/");
    assert!(fragment.contains("set-buffer -b tx-assistant-input \"%%\""));
    assert!(fragment.contains("assistant ask --from-buffer --pane '#{pane_id}'"));
}

#[test]
fn the_assistant_is_claude_with_common_and_its_role_in_its_own_repository() {
    let (dir, core) = core(&PluginConfig::default());
    // Trust seeding reads Claude's config file from the environment; keep it off the real HOME.
    unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", dir.path().join("claude-config")) };
    let options = create_options(&core, &Settings::default()).unwrap();
    assert_eq!(options.name, "tx-assistant");
    let home = core.config().plugin_data_dir("assistant").join("home");
    assert_eq!(options.cwd, home);
    assert!(
        home.join(".git").is_dir(),
        "a repository, made on first use"
    );
    assert!(home.join("README.md").is_file());
    let head = std::process::Command::new("git")
        .args([
            "-C",
            home.to_str().unwrap(),
            "rev-parse",
            "--verify",
            "HEAD",
        ])
        .output()
        .unwrap();
    assert!(head.status.success(), "one commit to branch worktrees from");
    // Second time: the same repository, untouched.
    assert_eq!(
        create_options(&core, &Settings::default()).unwrap().cwd,
        home
    );
    assert_eq!(options.tags, ["tx-system"]);
    let LaunchOptions::Agent {
        engine,
        profile,
        options,
        initial_prompt,
        ..
    } = options.launch
    else {
        panic!("an agent launch");
    };
    assert_eq!(engine, tx_core::EngineKind::Claude);
    assert_eq!(profile, ["COMMON", "TX-ASSISTANT"]);
    assert_eq!(options, serde_json::json!({"model": "opus", "effort": "2"}));
    assert_eq!(initial_prompt, tx_assistant::GREETING);
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("assistant".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.plugin_resources("").is_empty());
    assert!(core.verb("assistant").is_none());
}

fn plant_ended(
    core: &TxCore,
    id: &str,
    roles: &[&str],
    last_modified_at: f64,
    archived_at: Option<f64>,
) {
    let path = core
        .config()
        .agent_dir(&id.parse().unwrap())
        .join("agent.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        serde_json::json!({
            "version": 2, "id": id, "name": id, "engine": "claude", "working_directory": "/",
            "roles": roles, "state": "ended", "created_at": 1.0,
            "last_modified_at": last_modified_at, "archived_at": archived_at,
            "tags": ["tx-system"], "parent_id": null,
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
fn latest_ended_is_the_newest_unarchived_assistant() {
    let (_dir, core) = core(&PluginConfig::default());
    assert_eq!(latest_ended(&core).unwrap(), None);
    let older = "00000000-0000-4000-8000-000000000001";
    let newer = "00000000-0000-4000-8000-000000000002";
    plant_ended(&core, older, &["COMMON", "TX-ASSISTANT"], 10.0, None);
    plant_ended(&core, newer, &["COMMON", "TX-ASSISTANT"], 20.0, None);
    plant_ended(
        &core,
        "00000000-0000-4000-8000-000000000003",
        &["COMMON", "TX-ASSISTANT"],
        30.0,
        Some(31.0),
    );
    plant_ended(
        &core,
        "00000000-0000-4000-8000-000000000004",
        &["COMMON"],
        40.0,
        None,
    );
    assert_eq!(
        latest_ended(&core).unwrap().map(|id| id.to_string()),
        Some(newer.to_owned())
    );
}
