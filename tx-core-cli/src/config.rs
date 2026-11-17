//! `<config>/config.yaml`: read by `tx_plugin::config`, so that a plugin program reads it as `tx`
//! does.

pub use tx_plugin::config::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The file `install` writes spells out exactly the built-in defaults: every plugin on, each
    /// plugin's own default settings, and `agent` as a missing file has it.
    #[test]
    fn the_installed_file_is_the_built_in_defaults() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/tx-ide/config.yaml");
        let config = load(&path).unwrap();
        assert_eq!(config.agent, AgentDefaults::default());
        assert_eq!(config.worktree, WorktreeSettings::default());
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
}
