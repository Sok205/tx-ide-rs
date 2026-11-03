//! `<config>/config.yaml`: the user's preferences, read once per invocation.
//!
//! ```yaml
//! agent:                         # defaults for `tx create`; flags override
//!   engine: claude
//!   roles: [COMMON, DEVELOPER]   # any --role replaces the whole list
//!   model: claude-opus-5-5       # null: the engine's own default
//!   effort: high                 # null: the engine's own default
//! plugins:
//!   engine-codex: { enabled: false }
//!   assistant:
//!     settings: { model: opus, effort: medium }
//! ```
//! Every key is optional; a missing or empty file is every default. An unknown key, a bad value
//! or malformed YAML is a usage error naming the file.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use tx_core::EngineKind;

use crate::args::UsageError;
use crate::plugins::Entry;

pub const FILE_NAME: &str = "config.yaml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub agent: AgentDefaults,
    #[serde(default)]
    pub plugins: BTreeMap<String, Entry>,
}

/// What `tx create` uses for every flag it was not given.
#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentDefaults {
    pub engine: EngineKind,
    pub roles: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
}

impl Default for AgentDefaults {
    fn default() -> Self {
        Self {
            engine: EngineKind::Claude,
            roles: vec!["COMMON".to_owned(), "DEVELOPER".to_owned()],
            model: Some("claude-opus-5-5".to_owned()),
            effort: Some("high".to_owned()),
        }
    }
}

/// Reads `path`; a missing file is every default.
pub fn load(path: &Path) -> Result<Config, UsageError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(UsageError(format!("{}: {error}", path.display()))),
    };
    parse(&text, path)
}

/// `path` only names the file in errors.
pub fn parse(text: &str, path: &Path) -> Result<Config, UsageError> {
    // An empty document (or only comments) is null to serde, not an empty mapping.
    if text.lines().all(|line| {
        let line = line.trim();
        line.is_empty() || line.starts_with('#')
    }) {
        return Ok(Config::default());
    }
    serde_norway::from_str(text).map_err(|error| UsageError(format!("{}: {error}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_text(text: &str) -> Result<Config, UsageError> {
        parse(text, Path::new("/c/config.yaml"))
    }

    /// The file `install` writes spells out exactly the built-in defaults: every plugin on, each
    /// plugin's own default settings, and `agent` as a missing file has it.
    #[test]
    fn the_installed_file_is_the_built_in_defaults() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/tx-ide/config.yaml");
        let config = load(&path).unwrap();
        assert_eq!(config.agent, AgentDefaults::default());
        let mut listed: Vec<&str> = config.plugins.keys().map(String::as_str).collect();
        let mut known = crate::plugins::known_ids();
        listed.sort_unstable();
        known.sort_unstable();
        assert_eq!(listed, known, "every plugin, and only those");
        let plugins =
            crate::plugins::plugin_config(config.plugins, &crate::plugins::known_ids(), &[], &path)
                .unwrap();
        assert!(plugins.disabled.is_empty());
        assert_eq!(
            tx_assistant::Settings::from_value(&plugins.settings["assistant"]).unwrap(),
            tx_assistant::Settings::default()
        );
        assert_eq!(
            plugins.settings["artifacts"],
            serde_json::json!({ "editor": tx_artifacts::DEFAULT_EDITOR })
        );
        assert_eq!(
            plugins.settings["home-themes"],
            serde_json::json!({ "theme": tx_home_themes::THEMES[0].0 })
        );
        assert_eq!(plugins.settings.len(), 3, "{:?}", plugins.settings);
    }

    #[test]
    fn an_empty_file_is_every_default() {
        for text in ["", "\n", "# only a comment\n"] {
            let config = parse_text(text).unwrap();
            assert_eq!(config.agent, AgentDefaults::default(), "{text:?}");
            assert!(config.plugins.is_empty(), "{text:?}");
        }
    }

    #[test]
    fn a_missing_file_is_every_default() {
        let config = load(Path::new("/nonexistent/tx/config.yaml")).unwrap();
        assert_eq!(config.agent.engine, EngineKind::Claude);
        assert_eq!(config.agent.roles, ["COMMON", "DEVELOPER"]);
    }

    #[test]
    fn agent_keys_left_out_keep_their_defaults() {
        let config = parse_text("agent:\n  model: sonnet\n").unwrap();
        assert_eq!(
            config.agent,
            AgentDefaults {
                model: Some("sonnet".to_owned()),
                ..AgentDefaults::default()
            }
        );
    }

    #[test]
    fn null_model_and_effort_leave_them_to_the_engine() {
        let config = parse_text("agent:\n  model: null\n  effort: null\n").unwrap();
        assert_eq!(config.agent.model, None);
        assert_eq!(config.agent.effort, None);
    }

    #[test]
    fn every_agent_key_is_read() {
        let config =
            parse_text("agent:\n  engine: codex\n  roles: [QA]\n  model: gpt-5\n  effort: low\n")
                .unwrap();
        assert_eq!(
            config.agent,
            AgentDefaults {
                engine: EngineKind::Codex,
                roles: vec!["QA".to_owned()],
                model: Some("gpt-5".to_owned()),
                effort: Some("low".to_owned()),
            }
        );
    }

    #[test]
    fn unknown_keys_name_the_file_and_the_key() {
        for (text, key) in [("agnet: {}\n", "agnet"), ("agent:\n  modle: x\n", "modle")] {
            let error = parse_text(text).unwrap_err();
            assert!(error.0.starts_with("/c/config.yaml: "), "{error}");
            assert!(error.0.contains(key), "{error}");
        }
    }

    #[test]
    fn an_unknown_engine_is_refused() {
        let error = parse_text("agent:\n  engine: kodex\n").unwrap_err();
        assert!(error.0.contains("kodex"), "{error}");
    }

    #[test]
    fn malformed_yaml_names_the_file() {
        let error = parse_text("agent: [1, 2\n").unwrap_err();
        assert!(error.0.starts_with("/c/config.yaml: "), "{error}");
    }
}
