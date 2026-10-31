//! The assistant plugin's resources and create options, through the core.

use tx_assistant::{AssistantPlugin, create_options};
use tx_core::{CoreConfig, LaunchOptions, PluginConfig, TxCore};

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
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
fn the_role_is_a_resource() {
    let (_dir, core) = core(&PluginConfig::default());
    let agents = core.resources("agents/");
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].plugin, "assistant");
    assert_eq!(agents[0].name, "agents/TX-ASSISTANT.md");
    assert!(agents[0].bytes.starts_with(b"# TX-ASSISTANT role"));
    let keys = core.resources("keys/");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].name, "keys/assistant.tmux");
    let fragment = String::from_utf8_lossy(keys[0].bytes);
    assert!(fragment.contains("# keys: /\n"), "declares prefix+/");
    assert!(fragment.contains("set-buffer -b tx-assistant-input \"%%\""));
    assert!(fragment.contains("assistant ask --from-buffer"));
}

#[test]
fn the_assistant_is_claude_with_common_and_its_role_in_its_own_repository() {
    let (_dir, core) = core(&PluginConfig::default());
    let options = create_options(&core).unwrap();
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
    assert_eq!(create_options(&core).unwrap().cwd, home);
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
    assert_eq!(
        options,
        serde_json::json!({"model": "opus", "effort": "medium"})
    );
    assert_eq!(initial_prompt, tx_assistant::GREETING);
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("assistant".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
    assert!(core.verb("assistant").is_none());
}
