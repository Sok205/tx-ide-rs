//! The plugin switchboard: the compiled-in manifest and `plugins.yaml`.
//!
//! ```yaml
//! plugins:
//!   engine-codex: { enabled: false }
//!   labels:
//!     settings: { ... }
//! ```
//! A missing file enables everything; a malformed file or an unknown id is a usage error. A
//! retired id is skipped, so a file written for an older txc keeps loading.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use tx_core::{Plugin, PluginConfig};

use crate::args::UsageError;

pub const FILE_NAME: &str = "plugins.yaml";

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
    || Box::new(tx_menu::MenuPlugin),
];

/// Ids of plugins this binary no longer links; `plugins.yaml` entries for them are ignored.
/// `hooks`: agent state moved into core and the engine adapters (`txi hook`).
/// `history`: the transcript mirror; core archives conversations through the engines.
pub const RETIRED: &[&str] = &["hooks", "history"];

/// The built-in verbs; a plugin may not register one of these.
pub const RESERVED: &[&str] = &[
    "help", "engines", "plugins", "create", "fork", "respawn", "ls", "show", "rename", "tag",
    "send", "refresh", "where", "attach", "stop", "archive",
];

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    plugins: Option<BTreeMap<String, Entry>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    settings: Option<serde_json::Value>,
}

fn yes() -> bool {
    true
}

pub fn load(path: &Path, known: &[&str], reserved: &[&str]) -> Result<PluginConfig, UsageError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(UsageError(format!("{}: {error}", path.display()))),
    };
    parse(&text, known, reserved)
}

/// `known` are the manifest's ids: any other key is refused, even an empty entry.
pub fn parse(text: &str, known: &[&str], reserved: &[&str]) -> Result<PluginConfig, UsageError> {
    let file: File = if text.trim().is_empty() {
        File::default()
    } else {
        serde_norway::from_str(text).map_err(|error| UsageError(format!("{FILE_NAME}: {error}")))?
    };
    let mut cfg = PluginConfig {
        reserved: reserved.iter().map(|s| (*s).to_owned()).collect(),
        ..Default::default()
    };
    for (id, entry) in file.plugins.unwrap_or_default() {
        if RETIRED.contains(&id.as_str()) {
            continue;
        }
        if !known.contains(&id.as_str()) {
            return Err(UsageError(format!(
                "{FILE_NAME}: no plugin named {id:?} (known: {})",
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

/// The manifest's ids, for validating `plugins.yaml`.
pub fn known_ids() -> Vec<&'static str> {
    MANIFEST.iter().map(|make| make().id()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn parse_text(text: &str) -> Result<PluginConfig, UsageError> {
        parse(text, &["engine-codex", "labels"], &["ls"])
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
            "plugins:\n  hooks:\n    enabled: false\n    settings: { stuck_after_seconds: 600 }\n",
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
        assert!(error.0.starts_with("plugins.yaml: "), "{error}");
    }

    #[test]
    fn unknown_keys_on_an_entry_are_refused() {
        let error = parse_text("plugins:\n  labels: { enabledd: true }\n").unwrap_err();
        assert!(error.0.contains("enabledd"), "{error}");
    }
}
