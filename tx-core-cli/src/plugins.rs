//! The plugin switchboard: the compiled-in manifest and the `plugins:` section of `config.yaml`.
//!
//! ```yaml
//! plugins:
//!   engine-codex: { enabled: false }
//!   labels:
//!     settings: { ... }
//! ```
//! No entry enables a plugin; an unknown id is a usage error. A retired id is skipped, so a file
//! written for an older tx keeps loading.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use tx_core::{Plugin, PluginConfig};

use crate::args::UsageError;

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
    || Box::new(tx_home::HomePlugin),
    || Box::new(tx_home_themes::HomeThemesPlugin),
];

/// Ids of plugins this binary no longer links; `config.yaml` entries for them are ignored.
/// `hooks`: agent state moved into core and the engine adapters (`txi hook`).
/// `history`: the transcript mirror; core archives conversations through the engines.
/// `menu`: renamed `home`; an old `menu` entry no longer configures anything.
pub const RETIRED: &[&str] = &["hooks", "history", "menu"];

/// The built-in verbs; a plugin may not register one of these.
pub const RESERVED: &[&str] = &[
    "help", "engines", "plugins", "create", "fork", "respawn", "ls", "show", "rename", "tag",
    "send", "where", "attach", "stop",
];

/// One `plugins.<id>` entry of `config.yaml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    settings: Option<serde_json::Value>,
}

fn yes() -> bool {
    true
}

/// `known` are the manifest's ids: any other key is refused, even an empty entry. `path` only
/// names the file in errors.
pub fn plugin_config(
    entries: BTreeMap<String, Entry>,
    known: &[&str],
    reserved: &[&str],
    path: &Path,
) -> Result<PluginConfig, UsageError> {
    let mut cfg = PluginConfig {
        reserved: reserved.iter().map(|s| (*s).to_owned()).collect(),
        ..Default::default()
    };
    for (id, entry) in entries {
        if RETIRED.contains(&id.as_str()) {
            continue;
        }
        if !known.contains(&id.as_str()) {
            return Err(UsageError(format!(
                "{}: no plugin named {id:?} (known: {})",
                path.display(),
                known.join(", ")
            )));
        }
        if !entry.enabled {
            cfg.disabled.insert(id.clone());
        }
        if let Some(settings) = entry.settings {
            cfg.settings.insert(id, settings);
        }
    }
    Ok(cfg)
}

pub fn manifest() -> Vec<Box<dyn Plugin>> {
    MANIFEST.iter().map(|make| make()).collect()
}

/// The manifest's ids, for validating the `plugins:` section.
pub fn known_ids() -> Vec<&'static str> {
    MANIFEST.iter().map(|make| make().id()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn parse_text(text: &str) -> Result<PluginConfig, UsageError> {
        let path = Path::new("/c/config.yaml");
        let config = crate::config::parse(text, path)?;
        plugin_config(config.plugins, &["engine-codex", "labels"], &["ls"], path)
    }

    #[test]
    fn an_id_outside_the_manifest_is_refused_even_with_an_empty_entry() {
        let error = parse_text("plugins:\n  engine-cloud: {}\n").unwrap_err();
        assert!(error.0.contains("engine-cloud"), "{error}");
        assert!(
            error.0.contains("engine-codex"),
            "names the known ids: {error}"
        );
    }

    #[test]
    fn a_retired_id_is_skipped_with_its_settings() {
        let cfg = parse_text(
            "plugins:\n  hooks:\n    enabled: false\n    settings: { stuck_after_seconds: 600 }\n  menu: { enabled: false }\n",
        )
        .unwrap();
        assert!(cfg.disabled.is_empty());
        assert!(cfg.settings.is_empty());
    }

    #[test]
    fn empty_yaml_enables_everything() {
        for text in ["", "# nothing\n", "plugins:\n", "plugins: {}\n"] {
            let cfg = parse_text(text).unwrap();
            assert!(cfg.disabled.is_empty(), "{text:?}");
            assert!(cfg.settings.is_empty(), "{text:?}");
            assert_eq!(cfg.reserved, BTreeSet::from(["ls".to_owned()]));
        }
    }

    #[test]
    fn enabled_false_disables_and_settings_pass_through() {
        let cfg = parse_text(
            "plugins:\n  engine-codex: { enabled: false }\n  labels:\n    enabled: true\n    settings: { stuck_after_seconds: 5 }\n",
        )
        .unwrap();
        assert_eq!(cfg.disabled, BTreeSet::from(["engine-codex".to_owned()]));
        assert_eq!(
            cfg.settings,
            BTreeMap::from([(
                "labels".to_owned(),
                serde_json::json!({"stuck_after_seconds": 5})
            )])
        );
    }

    #[test]
    fn enabled_defaults_to_true() {
        let cfg = parse_text("plugins:\n  labels: {}\n").unwrap();
        assert!(cfg.disabled.is_empty());
    }

    #[test]
    fn malformed_yaml_is_a_usage_error() {
        let error = parse_text("plugins: [1, 2\n").unwrap_err();
        assert!(error.0.starts_with("/c/config.yaml: "), "{error}");
    }

    #[test]
    fn unknown_keys_on_an_entry_are_refused() {
        let error = parse_text("plugins:\n  labels: { enabledd: true }\n").unwrap_err();
        assert!(error.0.contains("enabledd"), "{error}");
    }
}
