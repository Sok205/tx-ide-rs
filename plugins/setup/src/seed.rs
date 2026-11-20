//! The `config.yaml` of a new install: the defaults of the repository's file, then the plugins
//! that were selected. Written once; a file that is there is the user's and is never opened for
//! writing.

use std::io::{self, Write};
use std::path::Path;

use tx_core::plugin::Manifest;

/// `config/tx-ide/config.yaml`: the `agent:` and `worktree:` defaults with their comments.
const DEFAULTS: &str = include_str!("../../../config/tx-ide/config.yaml");

const PLUGINS: &str = "\n# The plugins that are installed. `enabled: false` switches one off; `tx plugins` shows the result.\nplugins:";

/// The file's text for the plugins `selected`, which are in id order: one entry each, with the
/// manifest's default settings spelled out under it.
pub fn text<'a>(selected: impl IntoIterator<Item = &'a Manifest>) -> String {
    let mut text = format!("{DEFAULTS}{PLUGINS}");
    let mut empty = true;
    for manifest in selected {
        empty = false;
        if manifest.settings.is_null() {
            text.push_str(&format!("\n  {}: {{ enabled: true }}", manifest.id));
            continue;
        }
        text.push_str(&format!(
            "\n  {}:\n    enabled: true\n    settings:",
            manifest.id
        ));
        let settings = serde_norway::to_string(&manifest.settings)
            .expect("settings that were read from YAML are written as YAML");
        for line in settings.lines() {
            text.push_str(&format!("\n      {line}"));
        }
    }
    if empty {
        text.push_str(" {}");
    }
    text.push('\n');
    text
}

/// Writes `text` to `file` when nothing is there, not even a link that leads nowhere. Whether
/// it was written.
pub fn write(file: &Path, text: &str) -> io::Result<bool> {
    if file.symlink_metadata().is_ok() {
        return Ok(false);
    }
    std::fs::create_dir_all(file.parent().expect("a file is in a directory"))?;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(file)?
        .write_all(text.as_bytes())?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Bundles;
    use crate::presets::testing::bundle;

    fn seeded() -> String {
        let directory = tempfile::tempdir().unwrap();
        bundle(directory.path(), "kill", "core", &[], "");
        bundle(
            directory.path(),
            "assistant",
            "ide",
            &[],
            "settings: { model: opus, effort: 2, nested: { list: [a, \"b: c\"] } }\n",
        );
        let (bundles, _) = Bundles::read(directory.path()).unwrap();
        text(bundles.manifests.values())
    }

    #[test]
    fn the_file_is_the_defaults_and_one_entry_per_plugin_in_id_order() {
        let text = seeded();
        assert!(text.starts_with(DEFAULTS), "{text}");
        let plugins = text.strip_prefix(DEFAULTS).unwrap();
        assert_eq!(
            plugins,
            "\n# The plugins that are installed. `enabled: false` switches one off; `tx plugins` shows the result.\n\
             plugins:\n  assistant:\n    enabled: true\n    settings:\n      model: opus\n      effort: 2\n      \
             nested:\n        list:\n        - a\n        - 'b: c'\n  kill: { enabled: true }\n"
        );
    }

    #[test]
    fn the_file_reads_as_the_built_in_defaults_with_the_settings_of_the_manifests() {
        let file = Path::new("config.yaml");
        let config = tx_plugin::config::parse(&seeded(), file).unwrap();
        let defaults = tx_plugin::config::parse("", file).unwrap();
        assert_eq!(config.agent, defaults.agent);
        assert_eq!(config.worktree, defaults.worktree);
        let ids: Vec<&str> = config.plugins.keys().map(String::as_str).collect();
        assert_eq!(ids, ["assistant", "kill"]);
        let entries = tx_plugin::config::entries(&config.plugins);
        assert!(entries.values().all(|entry| entry.enabled));
        assert_eq!(
            entries["assistant"].settings,
            serde_json::json!({ "model": "opus", "effort": 2, "nested": { "list": ["a", "b: c"] } })
        );
        assert!(entries["kill"].settings.is_null());
    }

    #[test]
    fn no_plugin_is_an_empty_mapping() {
        let text = text([]);
        assert!(text.ends_with("plugins: {}\n"), "{text}");
        let config = tx_plugin::config::parse(&text, Path::new("config.yaml")).unwrap();
        assert!(config.plugins.is_empty());
    }

    #[test]
    fn a_file_that_is_there_is_left_alone_and_so_is_a_dangling_link() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("config/config.yaml");
        assert!(write(&file, "first\n").unwrap());
        assert!(!write(&file, "second\n").unwrap());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "first\n");
        let link = directory.path().join("config/link.yaml");
        std::os::unix::fs::symlink(directory.path().join("nowhere"), &link).unwrap();
        assert!(!write(&link, "text\n").unwrap());
        assert!(!directory.path().join("nowhere").exists());
    }
}
