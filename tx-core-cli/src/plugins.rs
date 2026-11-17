//! The plugin switchboard: the compiled-in manifest. The `plugins:` section of `config.yaml` is
//! read by `tx_plugin::config`.

use tx_core::Plugin;
pub use tx_plugin::config::{Entry, RETIRED, plugin_config};

/// Every plugin this binary links, in listing order.
pub const MANIFEST: &[fn() -> Box<dyn Plugin>] = &[
    || Box::new(tx_engine_claude::ClaudePlugin),
    || Box::new(tx_engine_codex::CodexPlugin),
    || Box::new(tx_chat::ChatPlugin),
    || Box::new(tx_client::IdePlugin),
    || Box::new(tx_labels::LabelsPlugin),
    || Box::new(tx_kill::KillPlugin),
    || Box::new(tx_picker::PickerPlugin),
    || Box::new(tx_edit::EditPlugin),
    || Box::new(tx_spawn::SpawnPlugin),
    || Box::new(tx_assistant::AssistantPlugin),
    || Box::new(tx_mockup::MockupPlugin),
    || Box::new(tx_tutor::TutorPlugin),
    || Box::new(tx_artifacts::ArtifactsPlugin),
    || Box::new(tx_tour::TourPlugin),
    || Box::new(tx_timeline::TimelinePlugin),
    || Box::new(tx_graph::GraphPlugin),
    || Box::new(tx_home::HomePlugin),
    || Box::new(tx_home_themes::HomeThemesPlugin),
    || Box::new(tx_layouts::LayoutsPlugin),
];

/// The built-in verbs; a plugin may not register one of these.
pub const RESERVED: &[&str] = &[
    "help", "engines", "plugins", "create", "fork", "respawn", "ls", "show", "rename", "tag",
    "send", "where", "attach", "stop", "gc", "update",
];

pub fn manifest() -> Vec<Box<dyn Plugin>> {
    MANIFEST.iter().map(|make| make()).collect()
}

/// The manifest's ids, for validating the `plugins:` section.
pub fn known_ids() -> Vec<&'static str> {
    MANIFEST.iter().map(|make| make().id()).collect()
}
