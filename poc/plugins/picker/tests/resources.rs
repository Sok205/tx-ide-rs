//! The picker plugin's resources, listed through the core.

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_picker::PickerPlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
    core.plug(vec![Box::new(PickerPlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_key_fragment_declares_its_key() {
    let (_dir, core) = core(&PluginConfig::default());
    let keys = core.resources("keys/");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].plugin, "picker");
    assert_eq!(keys[0].name, "keys/picker.tmux");
    let text = std::str::from_utf8(keys[0].bytes).unwrap();
    assert!(text.lines().any(|line| line == "# keys: t"), "{text}");
    assert!(text.contains("bind t display-popup"), "{text}");
    assert!(text.contains("$TX pick"), "{text}");
}

#[test]
fn it_publishes_no_scripts() {
    let (_dir, core) = core(&PluginConfig::default());
    assert!(core.resources("bin/").is_empty());
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("picker".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
}
