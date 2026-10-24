//! The mockup plugin's resources: the skill and the COMMON overlay, listed through the core.

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_mockup::MockupPlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
    core.plug(vec![Box::new(MockupPlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn ships_skill_and_overlay() {
    let (_dir, core) = core(&PluginConfig::default());
    let resources = core.resources("agents/");
    let names: Vec<&str> = resources
        .iter()
        .map(|resource| resource.name.as_str())
        .collect();
    assert_eq!(
        names,
        ["agents/COMMON.local.md", "agents/skills/tx-mockup/SKILL.md",]
    );

    let overlay = std::str::from_utf8(resources[0].bytes).unwrap();
    assert!(overlay.starts_with("---\nskills: [tx-mockup]"), "{overlay}");

    let skill = std::str::from_utf8(resources[1].bytes).unwrap();
    assert!(skill.contains("name: tx-mockup"), "{skill}");
    assert!(skill.contains("txc mockup show"), "{skill}");
    assert!(skill.contains("\"frames\""), "{skill}");
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("mockup".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
}
