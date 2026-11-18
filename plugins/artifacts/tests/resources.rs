//! The artifacts plugin's settings, through the core.

use tx_artifacts::ArtifactsPlugin;
use tx_core::{CoreConfig, PluginConfig, TxCore};

#[test]
fn a_non_string_editor_setting_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
        dir.path().join("state"),
    )))
    .unwrap();
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
