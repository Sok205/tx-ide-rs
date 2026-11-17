//! `<config>/config.yaml`: read by `tx_plugin::config`, so that a plugin program reads it as `tx`
//! does.

pub use tx_plugin::config::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The repository's file spells out exactly the built-in defaults of `agent` and `worktree`,
    /// as a missing file has them. It lists no plugin: `tx setup` adds the selected ones when it
    /// seeds the file.
    #[test]
    fn the_installed_file_is_the_built_in_defaults() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/tx-ide/config.yaml");
        let config = load(&path).unwrap();
        assert_eq!(config.agent, AgentDefaults::default());
        assert_eq!(config.worktree, WorktreeSettings::default());
        assert!(config.plugins.is_empty(), "{:?}", config.plugins);
    }
}
