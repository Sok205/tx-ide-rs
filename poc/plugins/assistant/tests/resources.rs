//! The assistant plugin's resources and create options, through the core.

use tx_assistant::{AssistantPlugin, create_options};
use tx_core::{CoreConfig, LaunchOptions, PluginConfig, TxCore};

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
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
    assert!(core.resources("keys/").is_empty());
}

#[test]
fn the_assistant_is_claude_with_common_and_its_role_in_the_state_dir() {
    let (_dir, core) = core(&PluginConfig::default());
    let options = create_options(&core);
    assert_eq!(options.name, "tx-assistant");
    assert_eq!(options.cwd, core.config().state_dir);
    assert_eq!(options.tags, ["tx-system"]);
    let LaunchOptions::Agent {
        engine,
        profile,
        model,
        effort,
        initial_prompt,
        ..
    } = options.launch
    else {
        panic!("an agent launch");
    };
    assert_eq!(engine, "claude");
    assert_eq!(profile, ["COMMON", "TX-ASSISTANT"]);
    assert_eq!(model.as_deref(), Some("opus"));
    assert_eq!(effort, Some(tx_core::Effort::Medium));
    assert_eq!(initial_prompt, None);
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("assistant".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
    assert!(core.verb("assistant").is_none());
}
