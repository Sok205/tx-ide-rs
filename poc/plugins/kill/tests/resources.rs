//! The kill plugin's resources, listed through the core.

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_kill::KillPlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
    core.plug(vec![Box::new(KillPlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_key_fragment_declares_its_key() {
    let (_dir, core) = core(&PluginConfig::default());
    let keys = core.resources("keys/");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].plugin, "kill");
    assert_eq!(keys[0].name, "keys/kill.tmux");
    let text = std::str::from_utf8(keys[0].bytes).unwrap();
    assert!(text.lines().any(|line| line == "# keys: X"), "{text}");
    assert!(text.contains("$STATE/bin/tmux-kill-tx-session"), "{text}");
}

#[test]
fn the_helper_script_is_a_script() {
    let (_dir, core) = core(&PluginConfig::default());
    let bin = core.resources("bin/");
    assert_eq!(bin.len(), 1);
    assert_eq!(bin[0].name, "bin/tmux-kill-tx-session");
    assert!(bin[0].bytes.starts_with(b"#!"));
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("kill".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
}
