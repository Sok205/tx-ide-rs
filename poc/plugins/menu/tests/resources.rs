//! The menu plugin's resources and verbs, listed through the core.

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_menu::MenuPlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
    core.plug(vec![Box::new(MenuPlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_key_fragment_declares_prefix_m_and_the_attach_hook() {
    let (_dir, core) = core(&PluginConfig::default());
    let keys = core.resources("keys/");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].plugin, "menu");
    assert_eq!(keys[0].name, "keys/menu.tmux");
    let text = std::str::from_utf8(keys[0].bytes).unwrap();
    assert!(text.starts_with("#!"), "{text}");
    assert!(text.lines().any(|line| line == "# keys: m"), "{text}");
    assert!(
        text.lines()
            .any(|line| line == "# unhook: client-attached[90]"),
        "{text}"
    );
    assert!(text.contains("bind m run-shell -b"), "{text}");
    assert!(
        text.contains("_menu-open '#{pane_id}' '#{client_name}'"),
        "{text}"
    );
    assert!(text.contains("set-hook -g client-attached[90] {"), "{text}");
    assert!(text.contains("#{@tx-menu-shown}"), "{text}");
    assert!(
        text.contains("_menu-first '#{pane_id}' '#{client_name}'"),
        "{text}"
    );
}

#[test]
fn the_verbs_are_registered() {
    let (_dir, core) = core(&PluginConfig::default());
    for verb in ["menu", "_menu-open", "_menu-first", "_menu-tui"] {
        assert!(core.verb(verb).is_some(), "{verb}");
    }
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("menu".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
    assert!(core.verb("menu").is_none());
}
