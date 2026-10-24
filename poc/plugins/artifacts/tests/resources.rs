//! The artifacts plugin's resources: the prefix+a fragment and its helper, listed through the
//! core.

use tx_artifacts::ArtifactsPlugin;
use tx_core::{CoreConfig, PluginConfig, TxCore};

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
    core.plug(vec![Box::new(ArtifactsPlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_key_fragment_declares_prefix_a() {
    let (_dir, core) = core(&PluginConfig::default());
    let keys = core.resources("keys/");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].plugin, "artifacts");
    assert_eq!(keys[0].name, "keys/artifacts.tmux");
    let text = std::str::from_utf8(keys[0].bytes).unwrap();
    assert!(text.lines().any(|line| line == "# keys: a"), "{text}");
    assert!(text.contains("$STATE/bin/tmux-artifact-menu"), "{text}");
    assert!(text.contains("send-keys $menu_prefix a"), "{text}");
}

#[test]
fn the_helper_script_runs_the_menu_verb() {
    let (_dir, core) = core(&PluginConfig::default());
    let bin = core.resources("bin/");
    assert_eq!(bin.len(), 1);
    assert_eq!(bin[0].name, "bin/tmux-artifact-menu");
    let text = std::str::from_utf8(bin[0].bytes).unwrap();
    assert!(text.starts_with("#!"));
    assert!(text.contains("_artifact-menu \"$1\" \"$2\""), "{text}");
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("artifacts".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
}

#[test]
fn a_non_string_editor_setting_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
    let mut config = PluginConfig::default();
    config
        .settings
        .insert("artifacts".to_owned(), serde_json::json!({"editor": 3}));
    let error = core
        .plug(vec![Box::new(ArtifactsPlugin)], &config)
        .err()
        .unwrap();
    assert!(error.to_string().contains("settings.editor"), "{error}");
}
