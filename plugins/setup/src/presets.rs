//! Which plugins a directory of bundles offers, and which of them a preset or a list selects.
//! Nothing here writes a file.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tx_core::plugin::manifest::MANIFEST_FILE;
use tx_core::plugin::{Manifest, Tier};

/// Under the config directory: the user's presets, one `<name>.yaml` each.
pub const PRESETS_DIR: &str = "presets";
const PRESET_EXTENSION: &str = "yaml";

/// What an install selects when nothing says otherwise.
pub const FULL: &str = "full";
/// The presets that ship, in the order they are listed and the wizard's keys `1`, `2`, `3` tick
/// them.
pub const SHIPPED: [(&str, &[Tier]); 3] = [
    ("headless", &[Tier::Core]),
    ("ide", &[Tier::Core, Tier::Ide]),
    (FULL, &[Tier::Core, Tier::Ide, Tier::Extra]),
];

/// The plugins that can be installed: the `<id>/plugin.yaml` of one directory that read.
#[derive(Debug)]
pub struct Bundles {
    /// The directory, as it was named: what the messages print.
    pub directory: PathBuf,
    pub manifests: BTreeMap<String, Manifest>,
}

impl Bundles {
    /// The bundles of `directory`, and why each manifest that does not read was left out.
    pub fn read(directory: &Path) -> io::Result<(Self, Vec<String>)> {
        let mut manifests = BTreeMap::new();
        let mut skipped = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let file = entry.path().join(MANIFEST_FILE);
            if entry.file_name().to_string_lossy().starts_with('.') || !file.is_file() {
                continue;
            }
            match Manifest::read(&file) {
                Ok(manifest) => {
                    manifests.insert(manifest.id.clone(), manifest);
                }
                Err(error) => skipped.push(format!("{error}: skipped")),
            }
        }
        skipped.sort_unstable();
        let bundles = Self {
            directory: directory.to_owned(),
            manifests,
        };
        Ok((bundles, skipped))
    }

    fn no_plugin(&self, id: &str) -> String {
        format!("no plugin {id:?} in {}", self.directory.display())
    }

    /// `ids` as a set, each of them a bundle; else the first that is not.
    pub fn named(&self, ids: &[String]) -> Result<BTreeSet<String>, String> {
        match ids.iter().find(|id| !self.manifests.contains_key(*id)) {
            Some(id) => Err(self.no_plugin(id)),
            None => Ok(ids.iter().cloned().collect()),
        }
    }

    /// Adds to `selected` what `id` requires, and what that requires, where it has a bundle.
    /// The notes say what was added and name each requirement that has no bundle.
    pub fn require(&self, id: &str, selected: &mut BTreeSet<String>) -> Vec<String> {
        let mut notes = Vec::new();
        let mut queue = vec![id.to_owned()];
        let mut seen = BTreeSet::from([id.to_owned()]);
        while !queue.is_empty() {
            for plugin in std::mem::take(&mut queue) {
                for requirement in &self.manifests[&plugin].requires {
                    if !seen.insert(requirement.clone()) {
                        continue;
                    }
                    if !self.manifests.contains_key(requirement) {
                        notes.push(format!(
                            "{plugin} requires {requirement}, which has no bundle in {}",
                            self.directory.display()
                        ));
                        continue;
                    }
                    if selected.insert(requirement.clone()) {
                        notes.push(format!("{plugin} requires {requirement}: added"));
                    }
                    queue.push(requirement.clone());
                }
            }
        }
        notes
    }

    /// The plugins of `selected` that are of no use without `id`: the ones that require it, and
    /// the ones that require those.
    pub fn dependants(&self, id: &str, selected: &BTreeSet<String>) -> BTreeSet<String> {
        let mut found = BTreeSet::new();
        let mut queue = vec![id.to_owned()];
        while let Some(requirement) = queue.pop() {
            for plugin in selected {
                if self.manifests[plugin].requires.contains(&requirement)
                    && found.insert(plugin.clone())
                {
                    queue.push(plugin.clone());
                }
            }
        }
        found
    }
}

/// A set of tiers and ids. Every key is optional.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    /// For whoever reads the file.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tiers: Vec<Tier>,
    /// Added to what the tiers select.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// Taken out again.
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Preset {
    fn parse(text: &str) -> Result<Self, serde_norway::Error> {
        // An empty document (or only comments) is null to serde, not an empty mapping.
        let empty = |line: &str| line.trim().is_empty() || line.trim().starts_with('#');
        if text.lines().all(empty) {
            return Ok(Self::default());
        }
        serde_norway::from_str(text)
    }

    /// The bundles this preset selects: those of its tiers and its `plugins`, without its
    /// `exclude`. Exactly that: what a plugin requires is not added. An id under `plugins` that
    /// is no bundle is the error.
    pub fn resolve(&self, bundles: &Bundles) -> Result<BTreeSet<String>, String> {
        let mut selected = bundles.named(&self.plugins)?;
        for (id, manifest) in &bundles.manifests {
            if self.tiers.contains(&manifest.tier) {
                selected.insert(id.clone());
            }
        }
        for id in &self.exclude {
            selected.remove(id);
        }
        Ok(selected)
    }
}

/// Every preset by name: the shipped ones first, then the user's own in name order. A user
/// preset of a shipped name stands in its place.
#[derive(Debug)]
pub struct Presets(pub Vec<(String, Preset)>);

impl Presets {
    /// The shipped presets and those of `directory` (`<config>/presets`), and why each file
    /// there that does not read was left out.
    pub fn read(directory: &Path) -> io::Result<(Self, Vec<String>)> {
        let mut own = BTreeMap::new();
        let mut skipped = Vec::new();
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => Some(entries),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        for entry in entries.into_iter().flatten() {
            let file = entry?.path();
            let name = file.file_stem().unwrap_or_default().to_string_lossy();
            if name.starts_with('.')
                || file
                    .extension()
                    .is_none_or(|found| found != PRESET_EXTENSION)
            {
                continue;
            }
            match Preset::parse(&std::fs::read_to_string(&file)?) {
                Ok(preset) => {
                    own.insert(name.into_owned(), preset);
                }
                Err(error) => skipped.push(format!("{}: {error}: skipped", file.display())),
            }
        }
        skipped.sort_unstable();
        let mut presets = Vec::new();
        for (name, tiers) in SHIPPED {
            let shipped = Preset {
                tiers: tiers.to_vec(),
                ..Preset::default()
            };
            presets.push((name.to_owned(), own.remove(name).unwrap_or(shipped)));
        }
        presets.extend(own);
        Ok((Self(presets), skipped))
    }

    /// The preset `name`; else the error that lists the known ones.
    pub fn named(&self, name: &str) -> Result<&Preset, String> {
        if let Some((_, preset)) = self.0.iter().find(|(known, _)| known == name) {
            return Ok(preset);
        }
        let mut known: Vec<&str> = self.0.iter().map(|(name, _)| name.as_str()).collect();
        known.sort_unstable();
        Err(format!("no preset {name:?} (known: {})", known.join(", ")))
    }

    /// What the preset `name` selects of `bundles`.
    pub fn resolve(&self, name: &str, bundles: &Bundles) -> Result<BTreeSet<String>, String> {
        self.named(name)?.resolve(bundles)
    }
}

#[cfg(test)]
pub mod testing {
    use super::*;

    /// Writes the bundle `id` into `directory`: a manifest of that tier and those requirements.
    pub fn bundle(directory: &Path, id: &str, tier: &str, requires: &[&str], extra: &str) {
        let bundle = directory.join(id);
        std::fs::create_dir_all(&bundle).unwrap();
        let text = format!(
            "api: 1\nid: {id}\ndescription: The {id} plugin.\ntier: {tier}\nrequires: [{}]\n{extra}",
            requires.join(", ")
        );
        std::fs::write(bundle.join(MANIFEST_FILE), text).unwrap();
    }

    /// A source like the shipped one, in small: two `core`, two `ide`, one `extra`.
    pub fn source() -> (tempfile::TempDir, Bundles) {
        let directory = tempfile::tempdir().unwrap();
        bundle(directory.path(), "engine", "core", &[], "");
        bundle(
            directory.path(),
            "labels",
            "core",
            &[],
            "needs: [tx-setup-no-such-program]\n",
        );
        bundle(directory.path(), "ide", "ide", &[], "");
        bundle(directory.path(), "home", "ide", &["ide"], "");
        bundle(directory.path(), "themes", "extra", &["home", "gone"], "");
        let (bundles, skipped) = Bundles::read(directory.path()).unwrap();
        assert_eq!(skipped, [] as [String; 0]);
        (directory, bundles)
    }

    /// Every bundle.
    pub fn all(bundles: &Bundles) -> BTreeSet<String> {
        bundles.manifests.keys().cloned().collect()
    }

    pub fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{all, bundle, set, source};
    use super::*;

    fn presets(files: &[(&str, &str)]) -> (Presets, Vec<String>) {
        let directory = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(directory.path().join(name), text).unwrap();
        }
        Presets::read(directory.path()).unwrap()
    }

    #[test]
    fn the_shipped_presets_follow_the_tiers() {
        let (_directory, bundles) = source();
        let (presets, _) = Presets::read(Path::new("/nonexistent/presets")).unwrap();
        let resolve = |name: &str| presets.resolve(name, &bundles).unwrap();
        assert_eq!(resolve("headless"), set(&["engine", "labels"]));
        assert_eq!(resolve("ide"), set(&["engine", "home", "ide", "labels"]));
        assert_eq!(resolve("full"), all(&bundles));
    }

    #[test]
    fn a_manifest_that_does_not_read_is_left_out_and_named() {
        let (directory, _) = source();
        bundle(directory.path(), "broken", "nonsense", &[], "");
        std::fs::create_dir(directory.path().join("no-manifest")).unwrap();
        let (bundles, skipped) = Bundles::read(directory.path()).unwrap();
        assert!(!bundles.manifests.contains_key("broken"));
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert!(skipped[0].contains("broken/plugin.yaml"), "{skipped:?}");
        assert!(skipped[0].ends_with(": skipped"), "{skipped:?}");
    }

    #[test]
    fn a_preset_of_your_own_is_its_tiers_and_plugins_without_its_excludes_and_nothing_more() {
        let (_directory, bundles) = source();
        let (presets, skipped) = presets(&[
            (
                "mine.yaml",
                "description: mine\ntiers: [core]\nplugins: [themes]\nexclude: [engine, not-a-bundle]\n",
            ),
            ("empty.yaml", "# nothing\n"),
            ("notes.txt", "not a preset"),
        ]);
        assert_eq!(skipped, [] as [String; 0]);
        let names: Vec<&str> = presets.0.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["headless", "ide", "full", "empty", "mine"]);
        // `themes` requires `home`: a preset is exact, the requirement is not added.
        assert_eq!(
            presets.resolve("mine", &bundles).unwrap(),
            set(&["labels", "themes"])
        );
        assert_eq!(presets.resolve("empty", &bundles).unwrap(), set(&[]));
    }

    #[test]
    fn a_preset_of_a_shipped_name_replaces_it_in_its_place() {
        let (_directory, bundles) = source();
        let (presets, _) = presets(&[("ide.yaml", "plugins: [ide]\n")]);
        let names: Vec<&str> = presets.0.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["headless", "ide", "full"]);
        assert_eq!(presets.resolve("ide", &bundles).unwrap(), set(&["ide"]));
    }

    #[test]
    fn an_unknown_key_leaves_the_preset_out_and_names_the_file() {
        let (presets, skipped) = presets(&[("odd.yaml", "tiers: [core]\ncolour: red\n")]);
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].contains("odd.yaml"), "{skipped:?}");
        assert!(skipped[0].contains("colour"), "{skipped:?}");
        assert_eq!(
            presets.named("odd").unwrap_err(),
            "no preset \"odd\" (known: full, headless, ide)"
        );
    }

    #[test]
    fn an_unknown_preset_and_an_unknown_plugin_are_named() {
        let (directory, bundles) = source();
        let (presets, _) = presets(&[("mine.yaml", "plugins: [nope]\n")]);
        assert_eq!(
            presets.resolve("x", &bundles).unwrap_err(),
            "no preset \"x\" (known: full, headless, ide, mine)"
        );
        let no_plugin = format!("no plugin \"nope\" in {}", directory.path().display());
        assert_eq!(presets.resolve("mine", &bundles).unwrap_err(), no_plugin);
        assert_eq!(
            bundles
                .named(&["ide".to_owned(), "nope".to_owned()])
                .unwrap_err(),
            no_plugin
        );
    }

    #[test]
    fn requirements_are_added_transitively_and_a_missing_one_is_only_named() {
        let (directory, bundles) = source();
        let mut selected = set(&["themes"]);
        let notes = bundles.require("themes", &mut selected);
        assert_eq!(selected, set(&["home", "ide", "themes"]));
        assert_eq!(
            notes,
            [
                "themes requires home: added".to_owned(),
                format!(
                    "themes requires gone, which has no bundle in {}",
                    directory.path().display()
                ),
                "home requires ide: added".to_owned(),
            ]
        );
        // Already selected: nothing is added, so nothing is said.
        assert_eq!(bundles.require("home", &mut selected), [] as [String; 0]);
    }

    #[test]
    fn dependants_are_found_transitively_among_the_selected() {
        let (_directory, bundles) = source();
        assert_eq!(
            bundles.dependants("ide", &all(&bundles)),
            set(&["home", "themes"])
        );
        assert_eq!(
            bundles.dependants("ide", &set(&["ide", "themes"])),
            set(&[])
        );
        assert_eq!(bundles.dependants("themes", &all(&bundles)), set(&[]));
    }
}
