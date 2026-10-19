//! The timeline plugin's resources and verbs, listed through the core.

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_timeline::TimelinePlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
    core.plug(vec![Box::new(TimelinePlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_key_fragment_declares_prefix_shift_t() {
    let (_dir, core) = core(&PluginConfig::default());
    let keys = core.resources("keys/");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].plugin, "timeline");
    assert_eq!(keys[0].name, "keys/timeline.tmux");
    let text = std::str::from_utf8(keys[0].bytes).unwrap();
    assert!(text.starts_with("#!"), "{text}");
    assert!(text.lines().any(|line| line == "# keys: T"), "{text}");
    assert!(text.contains("bind T display-popup"), "{text}");
    assert!(text.contains("_timeline-attach"), "{text}");
}

#[test]
fn the_verbs_are_registered() {
    let (_dir, core) = core(&PluginConfig::default());
    for verb in [
        "timeline",
        "_timeline-resident",
        "_timeline-attach",
        "_timeline-json",
    ] {
        assert!(core.verb(verb).is_some(), "{verb}");
    }
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("timeline".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
    assert!(core.verb("timeline").is_none());
}
