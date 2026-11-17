//! What a plugin's own test calls to keep its bundle honest: the manifest reads, what it names
//! is there, and it says what the code does.

use std::path::Path;

use serde_json::Value;
use tx_core::plugin::manifest::{BIN_DIR, MANIFEST_FILE};
use tx_core::plugin::{Manifest, Registry};
use tx_core::{CoreConfig, Directories, Plugin};

/// Checks the bundle in `directory`, a plugin's package or any plugin directory, and returns
/// its manifest. It is the whole test of a plugin whose program is not built on the SDK, or
/// that has no program.
///
/// # Panics
/// With the first of: the manifest does not read; a resource it lists is not a file of the
/// plugin; a `bin` it names is neither a file under `bin/` nor the name of a `[[bin]]` of the
/// package's `Cargo.toml`.
pub fn check_manifest(directory: &Path) -> Manifest {
    let file = directory.join(MANIFEST_FILE);
    let manifest = Manifest::read(&file).unwrap_or_else(|error| panic!("{error}"));
    for resource in &manifest.resources {
        assert!(
            directory.join(resource).is_file(),
            "{}: resource {resource:?} is not a file of the plugin",
            file.display()
        );
    }
    let programs = cargo_bins(directory);
    for bin in manifest.bins() {
        assert!(
            directory.join(BIN_DIR).join(bin).is_file() || programs.iter().any(|name| name == bin),
            "{}: bin {bin:?} is neither a file under {BIN_DIR}/ nor a [[bin]] of the package",
            file.display()
        );
    }
    manifest
}

/// The names of the `[[bin]]` targets of the package in `directory`; none without a
/// `Cargo.toml`.
fn cargo_bins(directory: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(directory.join("Cargo.toml")) else {
        return Vec::new();
    };
    let mut bins = Vec::new();
    let mut in_bin = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_bin = line == "[[bin]]";
        } else if in_bin
            && let Some(value) = line.strip_prefix("name")
            && let Some(name) = value.trim_start().strip_prefix('=')
        {
            bins.push(name.trim().trim_matches('"').to_owned());
        }
    }
    bins
}

/// [`check_manifest`], and the manifest says what `hosted`, the plugins its program hosts,
/// register: the same verbs with the same usages, the same engine kind, an updater exactly when
/// one is registered. The plugins are registered as a fresh install would, without settings.
///
/// # Panics
/// With the first difference. Also when a verb's `args` do not start with the verb's name: the
/// program reads its first word as the verb, so anything else runs another one.
pub fn check_bundle(directory: &Path, hosted: Vec<Box<dyn Plugin>>) {
    let manifest = check_manifest(directory);
    let file = directory.join(MANIFEST_FILE);
    let file = file.display();
    // `register` touches nothing: these directories are never created.
    let config = CoreConfig::new(Directories::under(directory.join("no-install")));
    let mut verbs: Vec<(String, String)> = Vec::new();
    let mut engines: Vec<String> = Vec::new();
    let mut updates = false;
    for plugin in &hosted {
        let mut registry = Registry::new(Value::Null, config.clone());
        plugin
            .register(&mut registry)
            .unwrap_or_else(|error| panic!("plugin {}: {error}", plugin.id()));
        verbs.extend(
            registry
                .verbs()
                .iter()
                .map(|verb| (verb.name().to_owned(), verb.usage().to_owned())),
        );
        engines.extend(
            registry
                .engines()
                .iter()
                .map(|engine| engine.kind().to_string()),
        );
        updates |= !registry.updaters().is_empty();
    }

    let mut declared: Vec<(String, String)> = manifest
        .verbs
        .iter()
        .map(|verb| (verb.name.clone(), verb.usage.clone()))
        .collect();
    declared.sort();
    verbs.sort();
    assert!(
        declared == verbs,
        "{file}: the verbs and usages differ from what the plugin registers\n the manifest: {declared:#?}\n registered: {verbs:#?}"
    );
    for verb in &manifest.verbs {
        assert!(
            verb.args.first() == Some(&verb.name),
            "{file}: verb {:?}: args must start with {:?}, the word the program reads as its verb",
            verb.name,
            verb.name
        );
    }
    let declared: Vec<&str> = manifest
        .engine
        .iter()
        .map(|engine| engine.kind.as_str())
        .collect();
    assert!(
        declared == engines,
        "{file}: the manifest's engine is {declared:?}, the plugin registers {engines:?}"
    );
    assert!(
        manifest.update.is_some() == updates,
        "{file}: the manifest {} and the plugin {}",
        if manifest.update.is_some() {
            "names an `update` program"
        } else {
            "names no `update` program"
        },
        if updates {
            "registers an updater"
        } else {
            "registers no updater"
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;

    use tx_core::updates::Outcome;
    use tx_core::{PluginError, TxCore, Updater, Verb, VerbError, VerbIo};

    /// A plugin `notes` that registers the verbs `notes` and `_notes-view`, and an updater when
    /// asked to.
    struct Notes {
        updater: bool,
    }

    struct NotesVerb(&'static str, &'static str);

    impl Verb for NotesVerb {
        fn name(&self) -> &'static str {
            self.0
        }
        fn usage(&self) -> &'static str {
            self.1
        }
        fn run(&self, _: &TxCore, _: &mut VerbIo<'_>, _: &[String]) -> Result<i32, VerbError> {
            Ok(0)
        }
    }

    struct NotesUpdater;

    impl Updater for NotesUpdater {
        fn update(&self, _: &TxCore, _: &mut VerbIo<'_>) -> Result<Outcome, VerbError> {
            Ok(Outcome::default())
        }
    }

    impl Plugin for Notes {
        fn id(&self) -> &'static str {
            "notes"
        }
        fn register(&self, registry: &mut Registry) -> Result<(), PluginError> {
            registry.verb(Arc::new(NotesVerb("notes", "notes ls   list the notes")));
            registry.verb(Arc::new(NotesVerb(
                "_notes-view",
                "_notes-view   (internal)",
            )));
            if self.updater {
                registry.updater(Arc::new(NotesUpdater));
            }
            Ok(())
        }
    }

    const MANIFEST: &str = "\
api: 1
id: notes
description: Notes.
verbs:
  - name: notes
    usage: \"notes ls   list the notes\"
    bin: tx-notes
    args: [notes]
  - name: _notes-view
    usage: \"_notes-view   (internal)\"
    bin: tx-notes
    args: [_notes-view]
resources:
  - keys/notes.tmux
  - bin/notes-helper
";

    const CARGO: &str = "\
[package]
name = \"tx-notes\"

[[bin]]
name = \"tx-notes\"
path = \"src/main.rs\"

[dependencies]
name = \"not-a-bin\"
";

    /// A package directory `notes` with `manifest`, the `Cargo.toml` above and both resources.
    fn package(manifest: &str) -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("notes");
        for sub in ["keys", "bin"] {
            std::fs::create_dir_all(directory.join(sub)).unwrap();
        }
        std::fs::write(directory.join("plugin.yaml"), manifest).unwrap();
        std::fs::write(directory.join("Cargo.toml"), CARGO).unwrap();
        std::fs::write(directory.join("keys/notes.tmux"), "# keys: none\n").unwrap();
        std::fs::write(directory.join("bin/notes-helper"), "#!/bin/sh\n").unwrap();
        (root, directory)
    }

    fn notes(updater: bool) -> Vec<Box<dyn Plugin>> {
        vec![Box::new(Notes { updater })]
    }

    /// The panic message of `check`.
    fn panics(check: impl FnOnce() + std::panic::UnwindSafe) -> String {
        let payload = std::panic::catch_unwind(check).expect_err("the check passed");
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| {
                payload
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
            })
            .unwrap()
    }

    #[test]
    fn an_honest_bundle_passes() {
        let (_root, directory) = package(MANIFEST);
        let manifest = check_manifest(&directory);
        assert_eq!(manifest.id, "notes");
        check_bundle(&directory, notes(false));
        assert_eq!(cargo_bins(&directory), ["tx-notes"]);
        assert!(!directory.join("no-install").exists());
    }

    #[test]
    fn a_manifest_that_does_not_read_is_the_first_failure() {
        let (_root, directory) = package(&MANIFEST.replace("api: 1", "api: 2"));
        let message = panics(|| {
            check_manifest(&directory);
        });
        assert_eq!(
            message,
            format!(
                "{}: api 2 is not supported (this tx: 1)",
                directory.join("plugin.yaml").display()
            )
        );
    }

    #[test]
    fn a_resource_that_is_not_there_fails() {
        let (_root, directory) = package(MANIFEST);
        std::fs::remove_file(directory.join("keys/notes.tmux")).unwrap();
        let message = panics(|| {
            check_manifest(&directory);
        });
        assert!(
            message.ends_with("resource \"keys/notes.tmux\" is not a file of the plugin"),
            "{message}"
        );
    }

    #[test]
    fn a_bin_is_a_file_under_bin_or_a_program_of_the_package() {
        // A script of the plugin.
        let (_root, directory) = package(&MANIFEST.replace("bin: tx-notes", "bin: notes-helper"));
        check_manifest(&directory);
        // Neither.
        let (_root, directory) = package(&MANIFEST.replace("bin: tx-notes", "bin: tx-other"));
        let message = panics(|| {
            check_manifest(&directory);
        });
        assert!(
            message.ends_with(
                "bin \"tx-other\" is neither a file under bin/ nor a [[bin]] of the package"
            ),
            "{message}"
        );
        // A key of another table named `name` is not a program.
        let (_root, directory) = package(&MANIFEST.replace("bin: tx-notes", "bin: not-a-bin"));
        panics(|| {
            check_manifest(&directory);
        });
    }

    #[test]
    fn verbs_and_usages_are_what_the_plugin_registers() {
        for (old, new) in [
            // A usage that drifted.
            ("list the notes", "lists notes"),
            // A verb the code does not have.
            ("name: _notes-view", "name: _notes-show"),
        ] {
            let (_root, directory) = package(&MANIFEST.replace(old, new));
            let message = panics(|| check_bundle(&directory, notes(false)));
            assert!(
                message.contains("the verbs and usages differ from what the plugin registers"),
                "{message}"
            );
        }
        // A verb the manifest leaves out.
        let cut = MANIFEST.find("  - name: _notes-view").unwrap();
        let end = MANIFEST.find("resources:").unwrap();
        let (_root, directory) = package(&format!("{}{}", &MANIFEST[..cut], &MANIFEST[end..]));
        let message = panics(|| check_bundle(&directory, notes(false)));
        assert!(message.contains("_notes-view"), "{message}");
    }

    #[test]
    fn a_verbs_args_start_with_its_name() {
        for args in ["args: [_notes-view]\n    # none", "args: [view]"] {
            let manifest = MANIFEST.replacen("args: [notes]", args, 1);
            let (_root, directory) = package(&manifest);
            if args.starts_with("args: [view]") {
                let message = panics(|| check_bundle(&directory, notes(false)));
                assert!(
                    message.contains("verb \"notes\": args must start with \"notes\""),
                    "{message}"
                );
            } else {
                panics(|| check_bundle(&directory, notes(false)));
            }
        }
        let (_root, directory) = package(&MANIFEST.replacen("    args: [notes]\n", "", 1));
        let message = panics(|| check_bundle(&directory, notes(false)));
        assert!(
            message.contains("args must start with \"notes\""),
            "{message}"
        );
    }

    #[test]
    fn an_updater_and_the_update_program_come_together() {
        let (_root, directory) = package(MANIFEST);
        let message = panics(|| check_bundle(&directory, notes(true)));
        assert!(
            message.ends_with(
                "the manifest names no `update` program and the plugin registers an updater"
            ),
            "{message}"
        );
        let with_update = format!("{MANIFEST}update:\n  bin: tx-notes\n  args: [--tx-update]\n");
        let (_root, directory) = package(&with_update);
        check_bundle(&directory, notes(true));
        let message = panics(|| check_bundle(&directory, notes(false)));
        assert!(
            message.ends_with(
                "the manifest names an `update` program and the plugin registers no updater"
            ),
            "{message}"
        );
    }

    #[test]
    fn an_engine_the_code_does_not_register_fails() {
        let with_engine = format!("{MANIFEST}engine:\n  kind: claude\n  bin: tx-notes\n");
        let (_root, directory) = package(&with_engine);
        let message = panics(|| check_bundle(&directory, notes(false)));
        assert!(
            message.ends_with("the manifest's engine is [\"claude\"], the plugin registers []"),
            "{message}"
        );
    }
}
