//! The menu plugin's resources and verbs, listed through the core.

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_home::HomePlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("tx")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
        dir.path().join("state"),
    )))
    .unwrap();
    core.plug(vec![Box::new(HomePlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_key_fragment_declares_prefix_m_and_the_attach_hook() {
    let (_dir, core) = core(&PluginConfig::default());
    let keys = core.resources("keys/");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].plugin, "home");
    assert_eq!(keys[0].name, "keys/home.tmux");
    let text = std::str::from_utf8(keys[0].bytes).unwrap();
    assert!(text.starts_with("#!"), "{text}");
    assert!(text.lines().any(|line| line == "# keys: m"), "{text}");
    assert!(
        !text.lines().any(|line| line.starts_with("# unhook:")),
        "{text}"
    );
    assert!(text.contains("bind m run-shell -b"), "{text}");
    assert!(
        text.contains("_home-open '#{pane_id}' '#{client_name}'"),
        "{text}"
    );
    assert!(!text.contains("client-attached"), "{text}");
}

#[test]
fn the_verbs_are_registered() {
    let (_dir, core) = core(&PluginConfig::default());
    for verb in [
        "home",
        "_home-open",
        "_home-tui",
        "_home-install",
        "_home-perform",
    ] {
        assert!(core.verb(verb).is_some(), "{verb}");
    }
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("home".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.plugin_resources("").is_empty());
    assert!(core.verb("home").is_none());
}
