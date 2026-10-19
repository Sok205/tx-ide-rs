//! The labels plugin's resources, listed through the core.

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_labels::LabelsPlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
    core.plug(vec![Box::new(LabelsPlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_key_fragment_declares_its_key_and_its_non_key_state() {
    let (_dir, core) = core(&PluginConfig::default());
    let all = core.resources("");
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].plugin, "labels");
    assert_eq!(all[0].name, "keys/labels.tmux");
    let text = std::str::from_utf8(all[0].bytes).unwrap();
    for line in [
        "# keys: s",
        "# unset: pane-border-format",
        "# unhook: after-new-window",
    ] {
        assert!(text.lines().any(|l| l == line), "{line} missing:\n{text}");
    }
}

#[test]
fn the_fragment_owns_the_format_and_the_hook_but_not_the_border_look() {
    let (_dir, core) = core(&PluginConfig::default());
    let text = String::from_utf8(core.resources("keys/")[0].bytes.to_vec()).unwrap();
    assert!(text.contains("set -g pane-border-format"));
    assert!(text.contains("set-hook -g after-new-window"));
    for look in [
        "pane-border-style",
        "pane-active-border-style",
        "pane-border-lines",
        "pane-border-indicators",
        "pane-border-status off",
    ] {
        assert!(!text.contains(look), "{look} belongs to ide");
    }
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("labels".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
}
