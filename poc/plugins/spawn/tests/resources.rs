//! The spawn plugin's resources, listed through the core.

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_spawn::SpawnPlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
        dir.path().join("state"),
    )))
    .unwrap();
    core.plug(vec![Box::new(SpawnPlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_key_fragment_declares_its_key() {
    let (_dir, core) = core(&PluginConfig::default());
    let keys = core.resources("keys/");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].plugin, "spawn");
    assert_eq!(keys[0].name, "keys/spawn.tmux");
    let text = std::str::from_utf8(keys[0].bytes).unwrap();
    assert!(text.lines().any(|line| line == "# keys: n"), "{text}");
    assert!(text.contains("$DATA/bin/tmux-new-agent"), "{text}");
}

#[test]
fn the_helper_script_is_a_script() {
    let (_dir, core) = core(&PluginConfig::default());
    let bin = core.resources("bin/");
    assert_eq!(bin.len(), 1);
    assert_eq!(bin[0].name, "bin/tmux-new-agent");
    assert!(bin[0].bytes.starts_with(b"#!"));
    assert!(
        std::str::from_utf8(bin[0].bytes)
            .unwrap()
            .contains("_new-agent")
    );
}

#[test]
fn the_shipped_roles_and_their_skills_are_resources() {
    let (_dir, core) = core(&PluginConfig::default());
    let names: Vec<String> = core
        .resources("agents/")
        .into_iter()
        .map(|resource| resource.name)
        .collect();
    assert_eq!(
        names,
        [
            "agents/COMMON.md",
            "agents/DEVELOPER.md",
            "agents/skills/tx-artifacts/SKILL.md",
            "agents/skills/tx-code-tours/SKILL.md",
            "agents/skills/tx-code-tours/scripts/apply_tour.py",
            "agents/skills/tx-diff-review/SKILL.md",
            "agents/skills/tx-sessions/SKILL.md",
        ]
    );
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("spawn".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
}
