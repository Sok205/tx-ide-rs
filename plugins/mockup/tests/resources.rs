//! The mockup plugin registers no resources: its skill and the COMMON overlay are shipped by
//! the core (`tx_core::roles`), so they are there whether the plugin is enabled or not.

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_mockup::MockupPlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("tx")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
        dir.path().join("state"),
    )))
    .unwrap();
    core.plug(vec![Box::new(MockupPlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_skill_and_overlay_come_from_the_core() {
    let (_dir, core) = core(&PluginConfig::default());
    assert!(core.plugin_resources("agents/").is_empty());
    let resources = core.resources("");
    let find = |name: &str| {
        resources
            .iter()
            .find(|resource| resource.name == name)
            .unwrap_or_else(|| panic!("{name} shipped by tx-core"))
    };
    assert_eq!(find("agents/COMMON.local.md").plugin, "tx-core");

    let overlay = std::str::from_utf8(find("agents/COMMON.local.md").bytes).unwrap();
    assert!(overlay.starts_with("---\nskills: [tx-mockup]"), "{overlay}");

    let skill = std::str::from_utf8(find("definitions/skills/tx-mockup/SKILL.md").bytes).unwrap();
    assert!(skill.contains("name: tx-mockup"), "{skill}");
    assert!(skill.contains("tx mockup show"), "{skill}");
    assert!(skill.contains("\"frames\""), "{skill}");
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("mockup".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.plugin_resources("").is_empty());
}
