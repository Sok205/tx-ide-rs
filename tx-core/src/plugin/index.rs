//! Which plugins are installed, and which of them are on. [`Index`] is the scan of the plugin
//! roots, cached in `<cache>/plugins-index.json` and trusted while every file it read still
//! looks the same. [`Activation`] is the index met with the front end's `plugins:` entries: who
//! is active, and which verb and which engine kind is whose.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::PluginStatus;
use super::manifest::{BIN_DIR, MANIFEST_FILE, Manifest, ManifestEngine};
use crate::config::CoreConfig;
use crate::engine::EngineKind;

/// Under the cache directory: the scan of the plugin roots.
pub const INDEX_FILE: &str = "plugins-index.json";
/// The layout of the index file; a file of another format is scanned again.
const FORMAT: u32 = 1;

/// One plugin directory of a root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// The name of its directory, which is what a valid manifest's `id` says too.
    pub id: String,
    pub directory: PathBuf,
    /// The root it was found in.
    pub root: PathBuf,
    /// Its manifest, or why the plugin cannot load.
    pub manifest: Result<Manifest, String>,
}

/// What `stat` said of one path the scan depends on; `None`: nothing there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Input {
    path: PathBuf,
    stamp: Option<Stamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    /// Nanoseconds since the epoch.
    modified: i64,
    size: u64,
}

impl Stamp {
    /// What `stat` says of `path` now. It follows links: a linked manifest changes when the
    /// file behind it does.
    fn of(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;
        Some(Self {
            modified: metadata.mtime() * 1_000_000_000 + metadata.mtime_nsec(),
            size: metadata.size(),
        })
    }
}

impl Input {
    fn of(path: PathBuf) -> Self {
        Self {
            stamp: Stamp::of(&path),
            path,
        }
    }

    fn holds(&self) -> bool {
        Stamp::of(&self.path) == self.stamp
    }
}

/// The index file, as it is read.
#[derive(Deserialize)]
struct Cached {
    format: u32,
    tx_version: String,
    roots: Vec<PathBuf>,
    inputs: Vec<Input>,
    plugins: Vec<Installed>,
}

/// The index file, as it is written.
#[derive(Serialize)]
struct Caching<'a> {
    format: u32,
    tx_version: &'a str,
    roots: &'a [PathBuf],
    inputs: &'a [Input],
    plugins: &'a [Installed],
}

/// Every plugin directory of the plugin roots, in plugin order: root order, then id. A plugin
/// that cannot load is an entry with the reason; nothing here is an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Index {
    pub roots: Vec<PathBuf>,
    pub plugins: Vec<Installed>,
    /// Every path the scan looked at, with what `stat` said just before it was read.
    inputs: Vec<Input>,
    tx_version: String,
    /// Whether the index file already holds exactly this.
    cached: bool,
}

impl Index {
    /// The installed plugins of `config`'s roots: the index file when it is fresh, else a scan of
    /// the roots, written back to the file.
    pub fn load(config: &CoreConfig, tx_version: &str) -> Index {
        let index = Self::read(config, tx_version);
        index.cache(config);
        index
    }

    /// [`Self::load`] without writing: for a caller that may yet refuse the command as misuse,
    /// which creates nothing. [`Self::cache`] writes it once the command runs.
    pub fn read(config: &CoreConfig, tx_version: &str) -> Index {
        let roots = config.plugin_roots();
        let file = config.directories.cache.join(INDEX_FILE);
        Self::cached(&file, &roots, tx_version).unwrap_or_else(|| Self::scan(roots, tx_version))
    }

    /// The index file, when it is of this format, of this tx, of these roots, and everything it
    /// was built from still has the modification time and size it had.
    fn cached(file: &Path, roots: &[PathBuf], tx_version: &str) -> Option<Index> {
        let text = std::fs::read_to_string(file).ok()?;
        let cached: Cached = serde_json::from_str(&text).ok()?;
        let fresh = cached.format == FORMAT
            && cached.tx_version == tx_version
            && cached.roots == roots
            && cached.inputs.iter().all(Input::holds);
        fresh.then_some(Index {
            roots: cached.roots,
            plugins: cached.plugins,
            inputs: cached.inputs,
            tx_version: cached.tx_version,
            cached: true,
        })
    }

    /// Reads the roots. An entry of a root is a plugin when it is a directory holding a
    /// `plugin.yaml` and its name does not start with `.`; a directory without one is remembered
    /// as such, so that a manifest written into it later is seen. The second directory of an id
    /// is an entry that says where the first is.
    fn scan(roots: Vec<PathBuf>, tx_version: &str) -> Index {
        let mut inputs = Vec::new();
        let mut plugins: Vec<Installed> = Vec::new();
        for root in &roots {
            inputs.push(Input::of(root.clone()));
            let mut names: Vec<String> = std::fs::read_dir(root)
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .filter(|name| !name.starts_with('.'))
                .collect();
            names.sort_unstable();
            for id in names {
                let directory = root.join(&id);
                let file = directory.join(MANIFEST_FILE);
                let input = Input::of(file.clone());
                let found = input.stamp.is_some();
                inputs.push(input);
                if !found {
                    continue;
                }
                let manifest = match plugins.iter().find(|earlier| earlier.id == id) {
                    Some(first) => Err(format!("also installed in {}", first.directory.display())),
                    None => Manifest::read(&file).map_err(|error| error.reason()),
                };
                plugins.push(Installed {
                    id,
                    directory,
                    root: root.clone(),
                    manifest,
                });
            }
        }
        Index {
            roots,
            plugins,
            inputs,
            tx_version: tx_version.to_owned(),
            cached: false,
        }
    }

    /// Writes the index file unless it already holds this index: a temporary file, then a
    /// rename, so two processes writing at once leave one whole file. A cache directory that
    /// cannot be written costs a scan on every run, never the command.
    pub fn cache(&self, config: &CoreConfig) {
        if !self.cached {
            let _ = self.write(&config.directories.cache);
        }
    }

    fn write(&self, directory: &Path) -> std::io::Result<()> {
        let text = serde_json::to_vec(&Caching {
            format: FORMAT,
            tx_version: &self.tx_version,
            roots: &self.roots,
            inputs: &self.inputs,
            plugins: &self.plugins,
        })?;
        std::fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(&text)?;
        file.persist(directory.join(INDEX_FILE))
            .map_err(|error| error.error)?;
        Ok(())
    }

    /// Whether a plugin directory named `id` is installed, loadable or not.
    pub fn contains(&self, id: &str) -> bool {
        self.plugins.iter().any(|installed| installed.id == id)
    }
}

/// One `plugins.<id>` entry of the front end's config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub enabled: bool,
    /// `Null` when the entry has none.
    pub settings: Value,
}

/// One plugin as this run sees it: an installed one, or an entry of the config that names no
/// installed plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginState {
    pub id: String,
    pub status: PluginStatus,
    /// What the config gives it; `Null` without an entry or without settings.
    pub settings: Value,
    /// Its position in [`Index::plugins`]; `None` for a `Missing` one.
    pub installed: Option<usize>,
}

/// A verb of an active plugin, as it is run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveVerb {
    /// The id of the plugin it belongs to.
    pub plugin: String,
    /// `<plugin>/bin/<bin>`.
    pub program: PathBuf,
    /// The words before the caller's arguments.
    pub args: Vec<String>,
    pub usage: String,
}

/// The engine of an active plugin, as the activation gave it to that plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveEngine {
    /// The id of the plugin it belongs to.
    pub plugin: String,
    /// The `ext` key the engine owns.
    pub ext_key: String,
}

/// Something an active plugin declares and does not get. The plugin stays active; the wording
/// here is the one every front end prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// Its verb has the name of one of the front end's own.
    BuiltInVerb { plugin: String, verb: String },
    /// Its verb is already the verb of `owner`, a plugin earlier in plugin order.
    TakenVerb {
        plugin: String,
        verb: String,
        owner: String,
    },
    /// The kind of its engine is already the kind of `owner`'s, a plugin earlier in plugin
    /// order.
    TakenEngineKind {
        plugin: String,
        kind: EngineKind,
        owner: String,
    },
    /// The `ext` key of its engine is already the key of `owner`'s engine, a plugin earlier in
    /// plugin order.
    TakenExtKey {
        plugin: String,
        key: String,
        owner: String,
    },
}

impl Problem {
    /// The plugin that lost something.
    pub fn plugin(&self) -> &str {
        match self {
            Problem::BuiltInVerb { plugin, .. }
            | Problem::TakenVerb { plugin, .. }
            | Problem::TakenEngineKind { plugin, .. }
            | Problem::TakenExtKey { plugin, .. } => plugin,
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::BuiltInVerb { verb, .. } => write!(f, "verb {verb:?} is a built-in"),
            Problem::TakenVerb { verb, owner, .. } => {
                write!(f, "verb {verb:?} is already {owner:?}'s")
            }
            Problem::TakenEngineKind { kind, owner, .. } => {
                write!(f, "engine kind {kind:?} is already {owner:?}'s")
            }
            Problem::TakenExtKey { key, owner, .. } => {
                write!(f, "ext key {key:?} is already {owner:?}'s")
            }
        }
    }
}

/// The index met with the config: every plugin's status, and the verbs and engines of the
/// active ones. Clashes of verbs, engine kinds and ext keys depend on who is enabled, so they
/// are found here and never stored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activation {
    /// The installed plugins in plugin order, then the config's entries that name none.
    pub plugins: Vec<PluginState>,
    /// By verb name.
    pub verbs: BTreeMap<String, ActiveVerb>,
    /// By engine kind.
    pub engines: BTreeMap<EngineKind, ActiveEngine>,
    pub problems: Vec<Problem>,
}

impl Activation {
    /// A plugin is active when its manifest loaded and its entry does not say `enabled: false`.
    /// A verb of an active plugin that is one of `reserved` (the front end's own verbs), or that
    /// an earlier plugin already has, is dropped with a problem; the earlier plugin keeps it.
    /// So is its engine, when an earlier plugin's engine already has the kind or the `ext` key.
    /// An entry naming no installed plugin is `Missing`.
    pub fn new(index: &Index, entries: &BTreeMap<String, Entry>, reserved: &[&str]) -> Activation {
        let mut activation = Activation::default();
        for (position, installed) in index.plugins.iter().enumerate() {
            let entry = entries.get(&installed.id);
            let status = match &installed.manifest {
                Err(reason) => PluginStatus::Error(reason.clone()),
                Ok(_) if entry.is_some_and(|entry| !entry.enabled) => PluginStatus::Disabled,
                Ok(manifest) => {
                    activation.claim_verbs(installed, manifest, reserved);
                    if let Some(engine) = &manifest.engine {
                        activation.claim_engine(installed, engine);
                    }
                    PluginStatus::Active
                }
            };
            activation.plugins.push(PluginState {
                id: installed.id.clone(),
                status,
                settings: entry.map_or(Value::Null, |entry| entry.settings.clone()),
                installed: Some(position),
            });
        }
        for (id, entry) in entries {
            if !index.contains(id) {
                activation.plugins.push(PluginState {
                    id: id.clone(),
                    status: PluginStatus::Missing,
                    settings: entry.settings.clone(),
                    installed: None,
                });
            }
        }
        activation
    }

    fn claim_verbs(&mut self, installed: &Installed, manifest: &Manifest, reserved: &[&str]) {
        for verb in &manifest.verbs {
            let plugin = installed.id.clone();
            let name = verb.name.clone();
            if reserved.contains(&name.as_str()) {
                self.problems
                    .push(Problem::BuiltInVerb { plugin, verb: name });
            } else if let Some(taken) = self.verbs.get(&name) {
                self.problems.push(Problem::TakenVerb {
                    plugin,
                    verb: name,
                    owner: taken.plugin.clone(),
                });
            } else {
                self.verbs.insert(
                    name,
                    ActiveVerb {
                        plugin,
                        program: installed.directory.join(BIN_DIR).join(&verb.bin),
                        args: verb.args.clone(),
                        usage: verb.usage.clone(),
                    },
                );
            }
        }
    }

    /// The kind first: a plugin that loses it is not also told about its `ext` key.
    fn claim_engine(&mut self, installed: &Installed, engine: &ManifestEngine) {
        let plugin = installed.id.clone();
        let kind: EngineKind = engine
            .kind
            .parse()
            .expect("the engine kind of a manifest that loaded");
        if let Some(taken) = self.engines.get(&kind) {
            self.problems.push(Problem::TakenEngineKind {
                plugin,
                kind,
                owner: taken.plugin.clone(),
            });
        } else if let Some(taken) = self
            .engines
            .values()
            .find(|taken| taken.ext_key == engine.ext_key)
        {
            self.problems.push(Problem::TakenExtKey {
                plugin,
                key: engine.ext_key.clone(),
                owner: taken.plugin.clone(),
            });
        } else {
            self.engines.insert(
                kind,
                ActiveEngine {
                    plugin,
                    ext_key: engine.ext_key.clone(),
                },
            );
        }
    }

    /// Whether an installed plugin `id` is active.
    pub fn is_active(&self, id: &str) -> bool {
        self.plugins
            .iter()
            .any(|state| state.id == id && state.status == PluginStatus::Active)
    }

    /// The active plugins with their index entry and manifest, in plugin order.
    pub fn active<'a>(
        &'a self,
        index: &'a Index,
    ) -> impl Iterator<Item = (&'a PluginState, &'a Installed, &'a Manifest)> {
        self.plugins.iter().filter_map(|state| {
            let installed = &index.plugins[state.installed?];
            let manifest = installed.manifest.as_ref().ok()?;
            (state.status == PluginStatus::Active).then_some((state, installed, manifest))
        })
    }

    /// What a listing says next to a plugin's status: why it is in error, or the problems of an
    /// active one joined with `; `.
    pub fn detail(&self, state: &PluginState) -> Option<String> {
        match &state.status {
            PluginStatus::Error(reason) => Some(reason.clone()),
            PluginStatus::Active => {
                let problems: Vec<String> = self
                    .problems
                    .iter()
                    .filter(|problem| problem.plugin() == state.id)
                    .map(Problem::to_string)
                    .collect();
                (!problems.is_empty()).then(|| problems.join("; "))
            }
            PluginStatus::Disabled | PluginStatus::Missing => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Directories;

    const VERSION: &str = "0.1.0";

    /// A sandbox: the five directories and two plugin roots of its own, `shipped` then `user`.
    struct Sandbox {
        dir: tempfile::TempDir,
    }

    impl Sandbox {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            for root in ["shipped", "user"] {
                std::fs::create_dir_all(dir.path().join(root)).unwrap();
            }
            Self { dir }
        }

        fn root(&self, name: &str) -> PathBuf {
            self.dir.path().join(name)
        }

        fn config(&self) -> CoreConfig {
            CoreConfig::new(Directories::under(self.dir.path().join("home")))
                .plugin_dirs([self.root("shipped"), self.root("user")])
        }

        fn index_file(&self) -> PathBuf {
            self.config().directories.cache.join(INDEX_FILE)
        }

        /// A plugin `id` in `root` whose manifest declares `verbs`, each run by `bin/<id>`.
        fn plugin(&self, root: &str, id: &str, verbs: &[&str]) -> PathBuf {
            let mut text = format!("api: 1\nid: {id}\ndescription: The {id} plugin.\n");
            if !verbs.is_empty() {
                text.push_str("verbs:\n");
            }
            for verb in verbs {
                text.push_str(&format!(
                    "  - name: {verb}\n    usage: \"{verb} of {id}\"\n    bin: {id}\n    args: [{verb}]\n"
                ));
            }
            self.manifest(root, id, &text)
        }

        fn manifest(&self, root: &str, id: &str, text: &str) -> PathBuf {
            let directory = self.root(root).join(id);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(MANIFEST_FILE), text).unwrap();
            directory
        }

        fn load(&self) -> Index {
            Index::load(&self.config(), VERSION)
        }
    }

    fn ids(index: &Index) -> Vec<&str> {
        index
            .plugins
            .iter()
            .map(|installed| installed.id.as_str())
            .collect()
    }

    fn entries(pairs: &[(&str, bool, Value)]) -> BTreeMap<String, Entry> {
        pairs
            .iter()
            .map(|(id, enabled, settings)| {
                (
                    (*id).to_owned(),
                    Entry {
                        enabled: *enabled,
                        settings: settings.clone(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn plugin_order_is_root_order_then_id() {
        let sandbox = Sandbox::new();
        sandbox.plugin("user", "alpha", &[]);
        sandbox.plugin("shipped", "zeta", &[]);
        sandbox.plugin("shipped", "beta", &[]);
        let index = sandbox.load();
        assert_eq!(ids(&index), ["beta", "zeta", "alpha"]);
        assert_eq!(index.roots, [sandbox.root("shipped"), sandbox.root("user")]);
        let beta = &index.plugins[0];
        assert_eq!(beta.directory, sandbox.root("shipped").join("beta"));
        assert_eq!(beta.root, sandbox.root("shipped"));
        assert_eq!(beta.manifest.as_ref().unwrap().id, "beta");
        assert_eq!(index.plugins[2].root, sandbox.root("user"));
        assert!(index.contains("zeta") && !index.contains("gamma"));
    }

    #[test]
    fn only_a_directory_holding_a_manifest_is_a_plugin() {
        let sandbox = Sandbox::new();
        sandbox.plugin("user", "real", &[]);
        sandbox.plugin("user", ".hidden", &[]);
        std::fs::create_dir_all(sandbox.root("user").join("data-only")).unwrap();
        std::fs::write(sandbox.root("user").join("a-file"), "x").unwrap();
        std::fs::write(sandbox.root("user").join(MANIFEST_FILE), "api: 1\n").unwrap();
        assert_eq!(ids(&sandbox.load()), ["real"]);
    }

    #[test]
    fn a_root_that_is_not_there_holds_nothing() {
        let sandbox = Sandbox::new();
        std::fs::remove_dir(sandbox.root("shipped")).unwrap();
        sandbox.plugin("user", "mine", &[]);
        assert_eq!(ids(&sandbox.load()), ["mine"]);
        // It appearing later is seen.
        sandbox.plugin("shipped", "theirs", &[]);
        assert_eq!(ids(&sandbox.load()), ["theirs", "mine"]);
    }

    #[test]
    fn a_plugin_that_cannot_load_is_an_entry_with_the_reason() {
        let sandbox = Sandbox::new();
        sandbox.manifest("user", "future", "api: 2\nid: future\ndescription: x\n");
        sandbox.manifest(
            "user",
            "typo",
            "api: 1\nid: typo\ndescription: x\ncolour: red\n",
        );
        sandbox.manifest("user", "renamed", "api: 1\nid: other\ndescription: x\n");
        let index = sandbox.load();
        assert_eq!(ids(&index), ["future", "renamed", "typo"]);
        let reason = |position: usize| index.plugins[position].manifest.clone().unwrap_err();
        assert_eq!(reason(0), "api 2 is not supported (this tx: 1)");
        assert_eq!(
            reason(1),
            "id \"other\" is not the name of its directory, \"renamed\""
        );
        assert!(
            reason(2).starts_with("line 4: unknown field `colour`"),
            "{}",
            reason(2)
        );
    }

    #[test]
    fn a_second_directory_of_an_id_says_where_the_first_is() {
        let sandbox = Sandbox::new();
        let first = sandbox.plugin("shipped", "mockup", &["mockup"]);
        sandbox.plugin("user", "mockup", &["mockup"]);
        let index = sandbox.load();
        assert_eq!(ids(&index), ["mockup", "mockup"]);
        assert!(index.plugins[0].manifest.is_ok());
        assert_eq!(
            index.plugins[1].manifest.clone().unwrap_err(),
            format!("also installed in {}", first.display())
        );
    }

    #[test]
    fn the_index_file_is_written_once_and_read_back() {
        let sandbox = Sandbox::new();
        sandbox.plugin("shipped", "mockup", &["mockup"]);
        assert!(!sandbox.index_file().exists());
        let scanned = sandbox.load();
        assert!(!scanned.cached);
        assert!(sandbox.index_file().is_file());
        let written = std::fs::metadata(sandbox.index_file())
            .unwrap()
            .modified()
            .unwrap();
        let read = sandbox.load();
        assert!(read.cached, "the second load is the file");
        assert_eq!(read.plugins, scanned.plugins);
        assert_eq!(read.roots, scanned.roots);
        assert_eq!(
            std::fs::metadata(sandbox.index_file())
                .unwrap()
                .modified()
                .unwrap(),
            written,
            "a fresh index is not written again"
        );
    }

    #[test]
    fn read_never_writes_and_cache_writes_what_was_read() {
        let sandbox = Sandbox::new();
        sandbox.plugin("user", "hello", &["hello"]);
        let config = sandbox.config();
        let index = Index::read(&config, VERSION);
        assert_eq!(ids(&index), ["hello"]);
        assert!(!config.directories.cache.exists(), "read created the cache");
        index.cache(&config);
        assert!(sandbox.index_file().is_file());
        assert!(Index::read(&config, VERSION).cached);
    }

    #[test]
    fn the_index_follows_the_roots() {
        let sandbox = Sandbox::new();
        let hello = sandbox.plugin("user", "hello", &["hello"]);
        assert_eq!(ids(&sandbox.load()), ["hello"]);

        // A manifest edited, to the same size.
        let file = hello.join(MANIFEST_FILE);
        let text = std::fs::read_to_string(&file).unwrap();
        std::fs::write(&file, text.replace("hello of hello", "HELLO OF HELLO")).unwrap();
        let index = sandbox.load();
        assert!(!index.cached);
        let manifest = index.plugins[0].manifest.as_ref().unwrap();
        assert_eq!(manifest.verbs[0].usage, "HELLO OF HELLO");

        // A plugin added, then removed.
        let second = sandbox.plugin("user", "second", &[]);
        assert_eq!(ids(&sandbox.load()), ["hello", "second"]);
        std::fs::remove_dir_all(second).unwrap();
        assert_eq!(ids(&sandbox.load()), ["hello"]);

        // A manifest that becomes invalid, and valid again.
        std::fs::write(&file, "api: 2\n").unwrap();
        assert!(sandbox.load().plugins[0].manifest.is_err());
        std::fs::write(&file, text).unwrap();
        assert!(sandbox.load().plugins[0].manifest.is_ok());
        assert!(sandbox.load().cached);
    }

    #[test]
    fn a_manifest_written_into_a_directory_that_was_already_there_is_seen() {
        let sandbox = Sandbox::new();
        let directory = sandbox.root("user").join("late");
        std::fs::create_dir_all(&directory).unwrap();
        assert!(sandbox.load().plugins.is_empty());
        assert!(sandbox.load().cached);
        // The root's own modification time does not move for this.
        std::fs::write(
            directory.join(MANIFEST_FILE),
            "api: 1\nid: late\ndescription: x\n",
        )
        .unwrap();
        assert_eq!(ids(&sandbox.load()), ["late"]);
        // And a manifest removed from a directory that stays.
        std::fs::remove_file(directory.join(MANIFEST_FILE)).unwrap();
        assert!(sandbox.load().plugins.is_empty());
    }

    #[test]
    fn another_tx_version_format_or_root_list_is_scanned_again() {
        let sandbox = Sandbox::new();
        sandbox.plugin("user", "hello", &[]);
        let config = sandbox.config();
        sandbox.load();
        assert!(Index::load(&config, VERSION).cached);
        assert!(!Index::load(&config, "0.2.0").cached);
        assert!(Index::load(&config, "0.2.0").cached);

        let fewer = config.clone().plugin_dirs([sandbox.root("user")]);
        let index = Index::load(&fewer, "0.2.0");
        assert!(!index.cached);
        assert_eq!(index.roots, [sandbox.root("user")]);

        let text = std::fs::read_to_string(sandbox.index_file()).unwrap();
        assert!(
            text.starts_with(r#"{"format":1,"tx_version":"0.2.0","#),
            "{text}"
        );
        std::fs::write(
            sandbox.index_file(),
            text.replacen(r#""format":1"#, r#""format":0"#, 1),
        )
        .unwrap();
        assert!(!Index::load(&fewer, "0.2.0").cached);

        std::fs::write(sandbox.index_file(), "not json").unwrap();
        let index = Index::load(&fewer, "0.2.0");
        assert!(!index.cached);
        assert_eq!(ids(&index), ["hello"]);
        assert!(Index::load(&fewer, "0.2.0").cached);
    }

    #[test]
    fn a_cache_that_cannot_be_written_is_a_scan_every_time() {
        let sandbox = Sandbox::new();
        sandbox.plugin("user", "hello", &[]);
        let config = sandbox.config();
        // The cache directory's place is taken by a file.
        std::fs::create_dir_all(config.directories.cache.parent().unwrap()).unwrap();
        std::fs::write(&config.directories.cache, "in the way").unwrap();
        for _ in 0..2 {
            let index = Index::load(&config, VERSION);
            assert!(!index.cached);
            assert_eq!(ids(&index), ["hello"]);
        }
    }

    #[test]
    fn a_linked_manifest_changes_with_the_file_behind_it() {
        let sandbox = Sandbox::new();
        let source = sandbox.dir.path().join("source.yaml");
        std::fs::write(&source, "api: 1\nid: linked\ndescription: one\n").unwrap();
        let directory = sandbox.root("shipped").join("linked");
        std::fs::create_dir_all(&directory).unwrap();
        std::os::unix::fs::symlink(&source, directory.join(MANIFEST_FILE)).unwrap();
        let description = |index: &Index| {
            index.plugins[0]
                .manifest
                .as_ref()
                .unwrap()
                .description
                .clone()
        };
        assert_eq!(description(&sandbox.load()), "one");
        std::fs::write(&source, "api: 1\nid: linked\ndescription: two\n").unwrap();
        assert_eq!(description(&sandbox.load()), "two");
    }

    #[test]
    fn an_activation_without_entries_makes_every_loadable_plugin_active() {
        let sandbox = Sandbox::new();
        sandbox.plugin("shipped", "mockup", &["mockup", "_mockup-view"]);
        sandbox.plugin("user", "hello", &["hello"]);
        sandbox.manifest("user", "future", "api: 2\n");
        let index = sandbox.load();
        let activation = Activation::new(&index, &BTreeMap::new(), &[]);
        let states: Vec<(&str, &PluginStatus, Option<usize>)> = activation
            .plugins
            .iter()
            .map(|state| (state.id.as_str(), &state.status, state.installed))
            .collect();
        assert_eq!(
            states,
            [
                ("mockup", &PluginStatus::Active, Some(0)),
                (
                    "future",
                    &PluginStatus::Error("api 2 is not supported (this tx: 1)".into()),
                    Some(1)
                ),
                ("hello", &PluginStatus::Active, Some(2)),
            ]
        );
        assert!(
            activation
                .plugins
                .iter()
                .all(|state| state.settings == Value::Null)
        );
        assert_eq!(
            activation.verbs.keys().collect::<Vec<_>>(),
            ["_mockup-view", "hello", "mockup"]
        );
        assert_eq!(
            activation.verbs["hello"],
            ActiveVerb {
                plugin: "hello".into(),
                program: sandbox.root("user").join("hello/bin/hello"),
                args: vec!["hello".into()],
                usage: "hello of hello".into(),
            }
        );
        assert!(activation.problems.is_empty());
        assert!(activation.is_active("hello") && !activation.is_active("future"));
        assert!(!activation.is_active("nowhere"));
        let active: Vec<&str> = activation
            .active(&index)
            .map(|(state, installed, manifest)| {
                assert_eq!(state.id, installed.id);
                assert_eq!(state.id, manifest.id);
                state.id.as_str()
            })
            .collect();
        assert_eq!(active, ["mockup", "hello"]);
        assert_eq!(
            activation.detail(&activation.plugins[1]).as_deref(),
            Some("api 2 is not supported (this tx: 1)")
        );
        assert_eq!(activation.detail(&activation.plugins[0]), None);
    }

    #[test]
    fn entries_switch_plugins_off_hand_them_settings_and_may_name_none() {
        let sandbox = Sandbox::new();
        sandbox.plugin("user", "hello", &["hello"]);
        sandbox.plugin("user", "quiet", &["quiet"]);
        sandbox.manifest("user", "future", "api: 2\n");
        let index = sandbox.load();
        let entries = entries(&[
            ("hello", true, serde_json::json!({"greeting": "hi"})),
            ("quiet", false, serde_json::json!({"volume": 0})),
            ("future", false, Value::Null),
            ("pikcer", false, Value::Null),
            (
                "hooks",
                true,
                serde_json::json!({"stuck_after_seconds": 600}),
            ),
        ]);
        let activation = Activation::new(&index, &entries, &[]);
        let states: Vec<(&str, &str, Option<usize>)> = activation
            .plugins
            .iter()
            .map(|state| (state.id.as_str(), state.status.as_str(), state.installed))
            .collect();
        assert_eq!(
            states,
            [
                ("future", "error", Some(0)),
                ("hello", "active", Some(1)),
                ("quiet", "disabled", Some(2)),
                ("hooks", "missing", None),
                ("pikcer", "missing", None),
            ]
        );
        assert_eq!(
            activation.plugins[1].settings,
            serde_json::json!({"greeting": "hi"})
        );
        assert_eq!(
            activation.plugins[2].settings,
            serde_json::json!({"volume": 0})
        );
        assert_eq!(activation.verbs.keys().collect::<Vec<_>>(), ["hello"]);
        assert!(!activation.is_active("quiet") && !activation.is_active("pikcer"));
        assert_eq!(activation.active(&index).count(), 1);
    }

    #[test]
    fn a_clashing_verb_is_dropped_with_a_problem_and_the_plugin_stays_active() {
        let sandbox = Sandbox::new();
        sandbox.plugin("user", "hello", &["hello"]);
        sandbox.plugin("user", "hello2", &["hello", "create", "wave"]);
        let index = sandbox.load();
        let activation = Activation::new(&index, &BTreeMap::new(), &["create", "ls"]);
        assert_eq!(activation.verbs["hello"].plugin, "hello");
        assert_eq!(activation.verbs["wave"].plugin, "hello2");
        assert!(!activation.verbs.contains_key("create"));
        assert_eq!(
            activation.problems,
            [
                Problem::TakenVerb {
                    plugin: "hello2".into(),
                    verb: "hello".into(),
                    owner: "hello".into(),
                },
                Problem::BuiltInVerb {
                    plugin: "hello2".into(),
                    verb: "create".into(),
                },
            ]
        );
        assert!(activation.is_active("hello2"));
        assert_eq!(activation.problems[0].plugin(), "hello2");
        assert_eq!(
            activation.detail(&activation.plugins[1]).as_deref(),
            Some("verb \"hello\" is already \"hello\"'s; verb \"create\" is a built-in")
        );
        assert_eq!(activation.detail(&activation.plugins[0]), None);

        // The first in plugin order is the shipped one, whatever its id.
        sandbox.plugin("shipped", "zebra", &["hello"]);
        let index = sandbox.load();
        let activation = Activation::new(&index, &BTreeMap::new(), &[]);
        assert_eq!(activation.verbs["hello"].plugin, "zebra");
        // Disabled, it claims nothing.
        let off = entries(&[("zebra", false, Value::Null)]);
        let activation = Activation::new(&index, &off, &[]);
        assert_eq!(activation.verbs["hello"].plugin, "hello");
    }

    #[test]
    fn a_clashing_engine_kind_or_ext_key_is_dropped_with_a_problem_and_the_plugin_stays_active() {
        let sandbox = Sandbox::new();
        let engine = |root: &str, id: &str, section: &str| {
            sandbox.manifest(
                root,
                id,
                &format!(
                    "api: 1\nid: {id}\ndescription: An engine.\nengine:\n  bin: {id}\n{section}"
                ),
            );
        };
        engine("user", "engine-a", "  kind: alpha\n");
        engine("user", "engine-b", "  kind: alpha\n  ext_key: other\n");
        engine("user", "engine-c", "  kind: gamma\n  ext_key: alpha\n");
        engine("user", "engine-d", "  kind: delta\n");
        let index = sandbox.load();
        let activation = Activation::new(&index, &BTreeMap::new(), &[]);
        let alpha: EngineKind = "alpha".parse().unwrap();
        let delta: EngineKind = "delta".parse().unwrap();
        assert_eq!(
            activation.engines,
            BTreeMap::from([
                (
                    alpha,
                    ActiveEngine {
                        plugin: "engine-a".into(),
                        ext_key: "alpha".into(),
                    }
                ),
                (
                    delta,
                    ActiveEngine {
                        plugin: "engine-d".into(),
                        ext_key: "delta".into(),
                    }
                ),
            ])
        );
        assert_eq!(
            activation.problems,
            [
                Problem::TakenEngineKind {
                    plugin: "engine-b".into(),
                    kind: alpha,
                    owner: "engine-a".into(),
                },
                Problem::TakenExtKey {
                    plugin: "engine-c".into(),
                    key: "alpha".into(),
                    owner: "engine-a".into(),
                },
            ]
        );
        for id in ["engine-a", "engine-b", "engine-c", "engine-d"] {
            assert!(activation.is_active(id), "{id}");
        }
        assert_eq!(activation.problems[0].plugin(), "engine-b");
        assert_eq!(activation.problems[1].plugin(), "engine-c");
        assert_eq!(
            activation.detail(&activation.plugins[1]).as_deref(),
            Some("engine kind \"alpha\" is already \"engine-a\"'s")
        );
        assert_eq!(
            activation.detail(&activation.plugins[2]).as_deref(),
            Some("ext key \"alpha\" is already \"engine-a\"'s")
        );
        assert_eq!(activation.detail(&activation.plugins[0]), None);

        // The first in plugin order is the shipped one, whatever its id.
        engine("shipped", "zebra", "  kind: alpha\n");
        let index = sandbox.load();
        let activation = Activation::new(&index, &BTreeMap::new(), &[]);
        assert_eq!(activation.engines[&alpha].plugin, "zebra");
        // Disabled, it claims nothing; nor does one whose owner is off lose anything.
        let off = entries(&[
            ("zebra", false, Value::Null),
            ("engine-a", false, Value::Null),
        ]);
        let activation = Activation::new(&index, &off, &[]);
        assert_eq!(activation.engines[&alpha].plugin, "engine-b");
        assert_eq!(activation.engines[&alpha].ext_key, "other");
        assert_eq!(
            activation.engines["gamma".parse::<EngineKind>().as_ref().unwrap()].plugin,
            "engine-c"
        );
        assert!(activation.problems.is_empty());
    }

    #[test]
    fn the_second_directory_of_an_id_is_in_error_next_to_the_first() {
        let sandbox = Sandbox::new();
        sandbox.plugin("shipped", "mockup", &["mockup"]);
        sandbox.plugin("user", "mockup", &["mockup"]);
        let index = sandbox.load();
        let activation = Activation::new(&index, &BTreeMap::new(), &[]);
        assert_eq!(activation.plugins[0].status, PluginStatus::Active);
        assert!(matches!(
            &activation.plugins[1].status,
            PluginStatus::Error(reason) if reason.starts_with("also installed in ")
        ));
        assert_eq!(
            activation.verbs["mockup"].program,
            sandbox.root("shipped").join("mockup/bin/mockup")
        );
        assert!(activation.problems.is_empty());
        assert!(activation.is_active("mockup"));
    }
}
