//! Laying the shipped plugins out in a plugin root, for `tx update`: `<data>/shipped/<id>/` from
//! a checkout's bundles and its build ([`plan`], then [`Plan::apply`]), which plugins that is
//! ([`Selection`], written by `tx setup`) and what was laid out last ([`Stamp`]). And publishing
//! under `<data>` the resources of core and of every active plugin ([`publish`]).
//!
//! File work only: nothing here starts a program. Every file goes in through a temporary file
//! next to it and a rename, so a running program keeps the file it started from; a file that is
//! already what it should be is left alone.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tx_core::plugin::manifest::{BIN_DIR, MANIFEST_FILE, RESOURCE_KINDS};
use tx_core::plugin::{Manifest, ManifestError};

/// In the shipped root: which plugins `tx update` lays out. Only `tx setup` writes it.
pub const SELECTION_FILE: &str = ".selection";
/// In the shipped root: what `tx update` laid out last, and from where.
pub const STAMP_FILE: &str = ".stamp";
/// Under the data directory, among the published key fragments: the `ide` updater's record of
/// the fragments it composed the server config from. No resource accounts for it, and it is
/// never pruned.
pub const KEYS_INSTALLED: &str = "keys/.installed";
/// Roles are published as `agents/…` resources, and installed under `<data>/roles/`: `<data>/agents/`
/// holds agent records.
const ROLES_RESOURCE: &str = "agents";
const ROLES_DIR: &str = "roles";

/// Why the shipped plugins could not be laid out. The text is what `tx update` prints after
/// `tx: update: `.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {message}", path.display())]
    Malformed { path: PathBuf, message: String },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// A selected id that is not a bundle of the source.
    #[error("plugin {id}: no {}", manifest.display())]
    NoBundle { id: String, manifest: PathBuf },
    #[error("plugin {id}: no {name} in {}", bundle.display())]
    NoResource {
        id: String,
        name: String,
        bundle: PathBuf,
    },
    #[error(
        "plugin {id}: no bin/{name} in {} and no {}: build it (cargo build --release -p tx-{id})",
        bundle.display(),
        binary.display()
    )]
    NoProgram {
        id: String,
        name: String,
        bundle: PathBuf,
        binary: PathBuf,
    },
    /// A directory resources are published to is a symbolic link: pruning it would remove
    /// files somewhere else.
    #[error("{} is a symlink; refusing to prune", .0.display())]
    SymlinkedKind(PathBuf),
}

fn failed(path: &Path) -> impl FnOnce(io::Error) -> Error {
    let path = path.to_path_buf();
    move |source| Error::Io { path, source }
}

/// `<shipped>/.selection`: the plugins `tx update` lays out, as `tx setup` resolved them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    /// The preset `plugins` was resolved from, kept so that an upgrade resolves it again;
    /// `None` for an explicit list.
    pub preset: Option<String>,
    /// The resolved ids: all `tx update` reads.
    pub plugins: Vec<String>,
}

impl Selection {
    /// The selection of the shipped root `shipped`; `None` when none was recorded.
    pub fn read(shipped: &Path) -> Result<Option<Self>, Error> {
        read_json(&shipped.join(SELECTION_FILE))
    }

    pub fn write(&self, shipped: &Path) -> Result<(), Error> {
        write_json(&shipped.join(SELECTION_FILE), self)
    }
}

/// `<shipped>/.stamp`: the build the shipped plugins were laid out from. They link the same
/// core as `tx`, so they must come from the same build as the `tx` that runs them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// The version of the tx that laid them out.
    pub tx_version: String,
    /// The plugin API of that tx.
    pub api: u32,
    pub bundles: PathBuf,
    pub binaries: PathBuf,
    /// Whether the files are symbolic links to their sources.
    pub linked: bool,
}

impl Stamp {
    /// The stamp of the shipped root `shipped`; `None` when nothing was laid out there.
    pub fn read(shipped: &Path) -> Result<Option<Self>, Error> {
        read_json(&shipped.join(STAMP_FILE))
    }

    pub fn write(&self, shipped: &Path) -> Result<(), Error> {
        write_json(&shipped.join(STAMP_FILE), self)
    }
}

/// A file tx wrote as JSON; `None` when it is not there. Keys this tx does not know are
/// ignored: a newer tx may have written the file.
fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, Error> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failed(path)(error)),
    };
    let malformed = |error: serde_json::Error| Error::Malformed {
        path: path.to_owned(),
        message: error.to_string(),
    };
    serde_json::from_str(&text).map(Some).map_err(malformed)
}

/// One compact line.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let mut text = serde_json::to_string(value).map_err(|error| Error::Malformed {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    text.push('\n');
    place(path, &Content::Bytes(text.as_bytes(), 0o644))?;
    Ok(())
}

/// The permission bits of a file mode.
const PERMISSIONS: u32 = 0o7777;

/// What a file is to be.
enum Content<'a> {
    /// These bytes, with this mode.
    Bytes(&'a [u8], u32),
    /// A copy of this file, with its mode.
    Copy(&'a Path),
    /// A symbolic link to this file.
    Link(&'a Path),
}

impl Content<'_> {
    /// Whether `path` is this already.
    fn is_at(&self, path: &Path) -> io::Result<bool> {
        let found = match path.symlink_metadata() {
            Ok(found) => found,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let mode = |metadata: &std::fs::Metadata| metadata.permissions().mode() & PERMISSIONS;
        Ok(match self {
            Content::Bytes(bytes, wanted) => {
                found.is_file() && mode(&found) == *wanted && std::fs::read(path)? == *bytes
            }
            Content::Copy(source) => {
                let wanted = std::fs::metadata(source)?;
                found.is_file()
                    && found.len() == wanted.len()
                    && mode(&found) == mode(&wanted)
                    && std::fs::read(path)? == std::fs::read(source)?
            }
            Content::Link(target) => found.is_symlink() && std::fs::read_link(path)? == *target,
        })
    }
}

/// `.<name>.<pid>.tmp` next to `path`: what is renamed onto it. The name ends in `.tmp`, so one
/// left behind is never taken for a key fragment, and nothing accounts for it: the next run
/// removes it.
fn temporary(path: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().expect("a file has a name"));
    name.push(format!(".{}.tmp", std::process::id()));
    path.with_file_name(name)
}

fn remove_if_there(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Makes `path` be `content`, creating its directory: written under a temporary name, then
/// renamed, so that whoever has the old file open keeps it. Whether it was written: a file that
/// is `content` already is left alone.
fn place(path: &Path, content: &Content<'_>) -> Result<bool, Error> {
    if content.is_at(path).map_err(failed(path))? {
        return Ok(false);
    }
    let directory = path.parent().expect("a file is in a directory");
    std::fs::create_dir_all(directory).map_err(failed(directory))?;
    let temporary = temporary(path);
    remove_if_there(&temporary).map_err(failed(&temporary))?;
    match content {
        Content::Bytes(bytes, mode) => std::fs::write(&temporary, bytes).and_then(|()| {
            std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(*mode))
        }),
        Content::Copy(source) => std::fs::copy(source, &temporary).map(|_| ()),
        Content::Link(target) => std::os::unix::fs::symlink(target, &temporary),
    }
    .map_err(failed(&temporary))?;
    std::fs::rename(&temporary, path).map_err(failed(path))?;
    Ok(true)
}

/// Where the shipped plugins come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// One `<id>/plugin.yaml` per plugin, with its resources and scripts: a checkout's
    /// `plugins/`.
    pub bundles: PathBuf,
    /// The built programs: `target/release`.
    pub binaries: PathBuf,
    /// Symbolic links to the sources instead of copies.
    pub link: bool,
}

/// One plugin as it is laid out.
#[derive(Debug)]
struct Bundle {
    id: String,
    /// Each file by its path inside the plugin, and the file it comes from.
    files: BTreeMap<PathBuf, PathBuf>,
    /// Those of `files` that are a program a `bin` names.
    programs: BTreeSet<PathBuf>,
}

/// What laying the shipped plugins out will do, read from the source and checked: every file
/// it needs is there. Nothing is written before [`Plan::apply`].
#[derive(Debug)]
pub struct Plan {
    shipped: PathBuf,
    link: bool,
    bundles: Vec<Bundle>,
    removed: Vec<String>,
}

/// Plans the shipped root `shipped` from `source`. The plugins are the ids of the
/// [`Selection`], else every bundle of the source. Every failure is returned, not only the
/// first: a selected id that is no bundle, a manifest that does not read, a resource the bundle
/// lacks, a program that is neither a script of the bundle nor built.
pub fn plan(source: &Source, shipped: &Path) -> Result<Plan, Vec<Error>> {
    let ids = selected(source, shipped).map_err(|error| vec![error])?;
    let removed = unselected(shipped, &ids).map_err(|error| vec![error])?;
    let mut bundles = Vec::new();
    let mut errors = Vec::new();
    for id in &ids {
        match bundle(source, id) {
            Ok(bundle) => bundles.push(bundle),
            Err(found) => errors.extend(found),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Plan {
        shipped: shipped.to_owned(),
        link: source.link,
        bundles,
        removed,
    })
}

/// The ids to lay out: the selection's, else every directory of the source that holds a
/// manifest.
fn selected(source: &Source, shipped: &Path) -> Result<BTreeSet<String>, Error> {
    if let Some(selection) = Selection::read(shipped)? {
        return Ok(selection.plugins.into_iter().collect());
    }
    let bundles = &source.bundles;
    let mut ids = BTreeSet::new();
    for entry in std::fs::read_dir(bundles).map_err(failed(bundles))? {
        let entry = entry.map_err(failed(bundles))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with('.') && entry.path().join(MANIFEST_FILE).is_file() {
            ids.insert(name);
        }
    }
    Ok(ids)
}

/// What the shipped root holds that is not selected, by name. Names starting with `.` are the
/// root's own files.
fn unselected(shipped: &Path, ids: &BTreeSet<String>) -> Result<Vec<String>, Error> {
    let entries = match std::fs::read_dir(shipped) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(failed(shipped)(error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let name = entry.map_err(failed(shipped))?.file_name();
        let name = name.to_string_lossy().into_owned();
        if !name.starts_with('.') && !ids.contains(&name) {
            names.push(name);
        }
    }
    names.sort_unstable();
    Ok(names)
}

/// The bundle `id` of the source: its manifest, each resource, and each program a `bin` names,
/// taken from the bundle's own `bin/` when the file is there (a script), else from the build.
fn bundle(source: &Source, id: &str) -> Result<Bundle, Vec<Error>> {
    let directory = source.bundles.join(id);
    let file = directory.join(MANIFEST_FILE);
    let no_bundle = || {
        vec![Error::NoBundle {
            id: id.to_owned(),
            manifest: file.clone(),
        }]
    };
    if !file.is_file() {
        return Err(no_bundle());
    }
    let manifest = Manifest::read(&file).map_err(|error| vec![Error::Manifest(error)])?;
    // A manifest's id is the name of its directory: an id that is a path to one is refused.
    if manifest.id != id {
        return Err(no_bundle());
    }
    let mut files = BTreeMap::from([(PathBuf::from(MANIFEST_FILE), file)]);
    let mut programs = BTreeSet::new();
    let mut errors = Vec::new();
    for name in &manifest.resources {
        let resource = directory.join(name);
        if resource.is_file() {
            files.insert(PathBuf::from(name), resource);
        } else {
            errors.push(Error::NoResource {
                id: id.to_owned(),
                name: name.clone(),
                bundle: directory.clone(),
            });
        }
    }
    for name in manifest.bins() {
        let program = Path::new(BIN_DIR).join(name);
        let script = directory.join(&program);
        let binary = source.binaries.join(name);
        if script.is_file() {
            files.insert(program.clone(), script);
            programs.insert(program);
        } else if binary.is_file() {
            files.insert(program.clone(), binary);
            programs.insert(program);
        } else {
            errors.push(Error::NoProgram {
                id: id.to_owned(),
                name: name.to_owned(),
                bundle: directory.clone(),
                binary,
            });
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(Bundle {
        id: id.to_owned(),
        files,
        programs,
    })
}

/// A plugin of the shipped root that [`Plan::apply`] changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub id: String,
    /// It is no longer selected: its directory is gone.
    pub removed: bool,
}

/// A program [`Plan::apply`] wrote: a file a `bin` of the plugin `id` names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub id: String,
    pub path: PathBuf,
}

/// What [`Plan::apply`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The plugins whose directory changed, each once: the removed ones, then the others in id
    /// order.
    pub changes: Vec<Change>,
    /// The programs written.
    pub programs: Vec<Program>,
}

impl Plan {
    /// What the shipped root holds that is not selected and will be removed: a plugin there is
    /// retired before [`Self::apply`].
    pub fn removed(&self) -> &[String] {
        &self.removed
    }

    /// Lays the plugins out: what is not selected goes; each selected plugin loses the files
    /// its bundle no longer names and gets every file that differs from its source.
    pub fn apply(&self) -> Result<Report, Error> {
        let mut report = Report::default();
        for name in &self.removed {
            let path = self.shipped.join(name);
            let is_directory = path
                .symlink_metadata()
                .map_err(failed(&path))?
                .file_type()
                .is_dir();
            if is_directory {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            }
            .map_err(failed(&path))?;
            report.changes.push(Change {
                id: name.clone(),
                removed: true,
            });
        }
        for bundle in &self.bundles {
            let directory = self.shipped.join(&bundle.id);
            let keep: HashSet<PathBuf> = bundle
                .files
                .keys()
                .map(|file| directory.join(file))
                .collect();
            let mut changed = sweep(&directory, &keep).map_err(failed(&directory))?;
            for (file, source) in &bundle.files {
                let path = directory.join(file);
                let content = if self.link {
                    Content::Link(source)
                } else {
                    Content::Copy(source)
                };
                if place(&path, &content)? {
                    changed = true;
                    if bundle.programs.contains(file) {
                        report.programs.push(Program {
                            id: bundle.id.clone(),
                            path,
                        });
                    }
                }
            }
            if changed {
                report.changes.push(Change {
                    id: bundle.id.clone(),
                    removed: false,
                });
            }
        }
        Ok(report)
    }
}

/// Removes what `keep` does not account for under `directory`, and each directory that leaves
/// empty. A symbolic link is removed itself and never entered; `directory` too, when it is one
/// or a file. Whether anything was removed.
fn sweep(directory: &Path, keep: &HashSet<PathBuf>) -> io::Result<bool> {
    let found = match directory.symlink_metadata() {
        Ok(found) => found,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !found.is_dir() {
        std::fs::remove_file(directory)?;
        return Ok(true);
    }
    let mut removed = false;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            removed |= sweep(&path, keep)?;
            if std::fs::read_dir(&path)?.next().is_none() {
                std::fs::remove_dir(&path)?;
                removed = true;
            }
        } else if !keep.contains(&path) {
            std::fs::remove_file(&path)?;
            removed = true;
        }
    }
    Ok(removed)
}

/// Where a resource is published: `<data>/<name>`, but `agents/…` under `<data>/roles/`.
fn published_path(data: &Path, name: &str) -> PathBuf {
    match name.strip_prefix(&format!("{ROLES_RESOURCE}/")) {
        Some(role) => data.join(ROLES_DIR).join(role),
        None => data.join(name),
    }
}

/// The directory a resource kind is published to; never with a trailing slash, so a symlink
/// there is seen as one.
fn kind_dir(data: &Path, kind: &str) -> PathBuf {
    if kind == ROLES_RESOURCE {
        data.join(ROLES_DIR)
    } else {
        data.join(kind)
    }
}

/// Publishes `resources`, each a name and its bytes, under `data`: every `keys/`, `conf/`,
/// `bin/`, `nvim/`, `agents/` and `definitions/` name is written to its place, and the files in
/// those six directories that no resource accounts for are removed. `bin/` and `keys/` files
/// are executable: scripts, and fragments the server config runs. The caller decides which
/// source wins a name: it is a map.
///
/// A directory of the six that is a symbolic link is refused before a byte is written.
pub fn publish(data: &Path, resources: &BTreeMap<String, Vec<u8>>) -> Result<(), Error> {
    for kind in RESOURCE_KINDS {
        let directory = kind_dir(data, kind);
        if directory
            .symlink_metadata()
            .is_ok_and(|found| found.is_symlink())
        {
            return Err(Error::SymlinkedKind(directory));
        }
    }
    let mut keep: HashSet<PathBuf> = HashSet::from([data.join(KEYS_INSTALLED)]);
    for (name, bytes) in resources {
        let path = published_path(data, name);
        let executable = name.starts_with("bin/") || name.starts_with("keys/");
        let mode = if executable { 0o755 } else { 0o644 };
        place(&path, &Content::Bytes(bytes, mode))?;
        keep.insert(path);
    }
    for kind in RESOURCE_KINDS {
        let directory = kind_dir(data, kind);
        prune(&directory, &keep).map_err(failed(&directory))?;
    }
    Ok(())
}

/// Removes the files under `directory` that `keep` does not account for; the directories stay.
fn prune(directory: &Path, keep: &HashSet<PathBuf>) -> io::Result<()> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        // `file_type` does not follow symlinks: a link is removed itself, never entered.
        if entry.file_type()?.is_dir() {
            prune(&path, keep)?;
        } else if !keep.contains(&path) {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    /// A checkout's bundles and its build, and a shipped root to lay them out in.
    struct Checkout {
        dir: tempfile::TempDir,
    }

    impl Checkout {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            for name in ["plugins", "build"] {
                std::fs::create_dir_all(dir.path().join(name)).unwrap();
            }
            Self { dir }
        }

        fn bundles(&self) -> PathBuf {
            self.dir.path().join("plugins")
        }

        fn binaries(&self) -> PathBuf {
            self.dir.path().join("build")
        }

        fn shipped(&self) -> PathBuf {
            self.dir.path().join("data/shipped")
        }

        fn source(&self, link: bool) -> Source {
            Source {
                bundles: self.bundles(),
                binaries: self.binaries(),
                link,
            }
        }

        /// A bundle `id` whose manifest is the three required keys and then `rest`.
        fn bundle(&self, id: &str, rest: &str) {
            let directory = self.bundles().join(id);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join(MANIFEST_FILE),
                format!("api: 1\nid: {id}\ndescription: The {id} plugin.\n{rest}"),
            )
            .unwrap();
        }

        /// A file of the checkout with `mode`, under `plugins/` or `build/`.
        fn file(&self, path: &str, text: &str, mode: u32) -> PathBuf {
            let path = self.dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            path
        }

        /// `hello`: a verb run by the built program `tx-hello`, an updater that is the script
        /// `bin/up`, and a key fragment.
        fn hello(&self) {
            self.bundle(
                "hello",
                "verbs:\n  - {name: hello, usage: \"hello\", bin: tx-hello}\n\
                 update: {bin: up}\nresources: [keys/hello.tmux]\n",
            );
            self.file("build/tx-hello", "the program", 0o755);
            self.file("plugins/hello/bin/up", "#!/bin/sh\n", 0o755);
            self.file("plugins/hello/keys/hello.tmux", "# keys: H\n", 0o644);
        }

        fn plan(&self, link: bool) -> Plan {
            plan(&self.source(link), &self.shipped()).unwrap_or_else(|errors| panic!("{errors:?}"))
        }

        fn refused(&self) -> Vec<String> {
            plan(&self.source(false), &self.shipped())
                .unwrap_err()
                .iter()
                .map(Error::to_string)
                .collect()
        }

        fn select(&self, plugins: &[&str]) {
            Selection {
                preset: None,
                plugins: plugins.iter().map(|id| (*id).to_owned()).collect(),
            }
            .write(&self.shipped())
            .unwrap();
        }
    }

    fn changes(report: &Report) -> Vec<String> {
        report
            .changes
            .iter()
            .map(|change| {
                format!(
                    "{}{}",
                    change.id,
                    if change.removed { " removed" } else { "" }
                )
            })
            .collect()
    }

    fn inode(path: &Path) -> u64 {
        std::fs::metadata(path).unwrap().ino()
    }

    #[test]
    fn a_selection_is_one_compact_line_and_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let shipped = dir.path().join("shipped");
        assert_eq!(Selection::read(&shipped).unwrap(), None);
        let selection = Selection {
            preset: None,
            plugins: vec!["mockup".to_owned()],
        };
        selection.write(&shipped).unwrap();
        assert_eq!(
            std::fs::read_to_string(shipped.join(SELECTION_FILE)).unwrap(),
            "{\"preset\":null,\"plugins\":[\"mockup\"]}\n"
        );
        assert_eq!(Selection::read(&shipped).unwrap(), Some(selection));
        std::fs::write(
            shipped.join(SELECTION_FILE),
            r#"{"preset": "full", "plugins": ["a", "b"], "later": 1}"#,
        )
        .unwrap();
        assert_eq!(
            Selection::read(&shipped).unwrap(),
            Some(Selection {
                preset: Some("full".to_owned()),
                plugins: vec!["a".to_owned(), "b".to_owned()],
            }),
            "a key of a newer tx is ignored"
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SELECTION_FILE), "{\"plugins\": 3}").unwrap();
        std::fs::write(dir.path().join(STAMP_FILE), "not json").unwrap();
        let error = Selection::read(dir.path()).unwrap_err().to_string();
        assert!(
            error.starts_with(&format!("{}: ", dir.path().join(SELECTION_FILE).display())),
            "{error}"
        );
        let error = Stamp::read(dir.path()).unwrap_err().to_string();
        assert!(
            error.starts_with(&format!("{}: ", dir.path().join(STAMP_FILE).display())),
            "{error}"
        );
    }

    #[test]
    fn a_stamp_reads_back_and_is_rewritten_only_when_it_changes() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Stamp::read(dir.path()).unwrap(), None);
        let stamp = Stamp {
            tx_version: "0.1.0".to_owned(),
            api: 1,
            bundles: "/checkout/plugins".into(),
            binaries: "/checkout/target/release".into(),
            linked: false,
        };
        stamp.write(dir.path()).unwrap();
        let file = dir.path().join(STAMP_FILE);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\"tx_version\":\"0.1.0\",\"api\":1,\"bundles\":\"/checkout/plugins\",\
             \"binaries\":\"/checkout/target/release\",\"linked\":false}\n"
        );
        assert_eq!(Stamp::read(dir.path()).unwrap().as_ref(), Some(&stamp));
        let first = inode(&file);
        stamp.write(dir.path()).unwrap();
        assert_eq!(inode(&file), first, "the same stamp is left alone");
        Stamp {
            linked: true,
            ..stamp
        }
        .write(dir.path())
        .unwrap();
        assert_ne!(inode(&file), first, "another one replaces the file");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn copies_every_file_of_every_bundle_and_then_leaves_them_alone() {
        let checkout = Checkout::new();
        checkout.hello();
        checkout.bundle("plain", "");
        // Not a bundle: no manifest; and not looked at: a dot name.
        std::fs::create_dir_all(checkout.bundles().join("notes")).unwrap();
        checkout.bundle(".hidden", "");

        let report = checkout.plan(false).apply().unwrap();
        assert_eq!(changes(&report), ["hello", "plain"]);
        let hello = checkout.shipped().join("hello");
        assert_eq!(
            report.programs,
            [
                Program {
                    id: "hello".to_owned(),
                    path: hello.join("bin/tx-hello"),
                },
                Program {
                    id: "hello".to_owned(),
                    path: hello.join("bin/up"),
                },
            ],
            "the programs a bin names, not the key fragment"
        );
        for (file, text, mode) in [
            ("bin/tx-hello", "the program", 0o755),
            ("bin/up", "#!/bin/sh\n", 0o755),
            ("keys/hello.tmux", "# keys: H\n", 0o644),
        ] {
            let path = hello.join(file);
            let metadata = path.symlink_metadata().unwrap();
            assert!(metadata.is_file(), "{file} is a copy");
            assert_eq!(metadata.permissions().mode() & PERMISSIONS, mode, "{file}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        }
        assert!(hello.join(MANIFEST_FILE).is_file());
        assert!(
            checkout
                .shipped()
                .join("plain")
                .join(MANIFEST_FILE)
                .is_file()
        );
        assert!(!checkout.shipped().join("notes").exists());
        assert!(!checkout.shipped().join(".hidden").exists());

        let program = inode(&hello.join("bin/tx-hello"));
        assert_eq!(checkout.plan(false).apply().unwrap(), Report::default());
        assert_eq!(inode(&hello.join("bin/tx-hello")), program);
    }

    #[test]
    fn a_file_whose_source_changed_is_replaced_by_a_new_file() {
        let checkout = Checkout::new();
        checkout.hello();
        checkout.plan(false).apply().unwrap();
        let program = checkout.shipped().join("hello/bin/tx-hello");
        let before = inode(&program);
        let running = std::fs::File::open(&program).unwrap();

        checkout.file("build/tx-hello", "rebuilt program", 0o755);
        let report = checkout.plan(false).apply().unwrap();
        assert_eq!(changes(&report), ["hello"]);
        assert_eq!(report.programs.len(), 1, "only the program that changed");
        assert_ne!(inode(&program), before);
        assert_eq!(
            std::fs::read_to_string(&program).unwrap(),
            "rebuilt program"
        );
        assert_eq!(
            running.metadata().unwrap().ino(),
            before,
            "whoever has the old file open keeps it"
        );

        // The same bytes with another mode: replaced too.
        checkout.file("plugins/hello/bin/up", "#!/bin/sh\n", 0o700);
        let report = checkout.plan(false).apply().unwrap();
        assert_eq!(report.programs.len(), 1);
        let up = checkout.shipped().join("hello/bin/up");
        assert_eq!(
            up.metadata().unwrap().permissions().mode() & PERMISSIONS,
            0o700
        );
    }

    #[test]
    fn links_point_at_the_sources_and_switch_with_copies() {
        let checkout = Checkout::new();
        checkout.hello();
        let report = checkout.plan(true).apply().unwrap();
        assert_eq!(changes(&report), ["hello"]);
        let hello = checkout.shipped().join("hello");
        for (file, source) in [
            (MANIFEST_FILE, "plugins/hello/plugin.yaml"),
            ("bin/tx-hello", "build/tx-hello"),
            ("bin/up", "plugins/hello/bin/up"),
            ("keys/hello.tmux", "plugins/hello/keys/hello.tmux"),
        ] {
            assert_eq!(
                std::fs::read_link(hello.join(file)).unwrap(),
                checkout.dir.path().join(source),
                "{file}"
            );
        }
        assert_eq!(checkout.plan(true).apply().unwrap(), Report::default());

        let report = checkout.plan(false).apply().unwrap();
        assert_eq!(changes(&report), ["hello"]);
        assert!(
            hello
                .join("bin/tx-hello")
                .symlink_metadata()
                .unwrap()
                .is_file()
        );
        let report = checkout.plan(true).apply().unwrap();
        assert_eq!(changes(&report), ["hello"]);
        assert!(hello.join("bin/tx-hello").is_symlink());
    }

    #[test]
    fn the_selection_decides_which_bundles_are_laid_out() {
        let checkout = Checkout::new();
        checkout.hello();
        checkout.bundle("plain", "");
        checkout.bundle("other", "");
        checkout.select(&["plain"]);
        assert_eq!(changes(&checkout.plan(false).apply().unwrap()), ["plain"]);
        assert!(!checkout.shipped().join("hello").exists());

        checkout.select(&["plain", "hello"]);
        assert_eq!(changes(&checkout.plan(false).apply().unwrap()), ["hello"]);

        checkout.select(&["hello"]);
        let plan = checkout.plan(false);
        assert_eq!(plan.removed(), ["plain"]);
        assert!(
            checkout.shipped().join("plain").exists(),
            "a plan removes nothing"
        );
        assert_eq!(changes(&plan.apply().unwrap()), ["plain removed"]);
        assert!(!checkout.shipped().join("plain").exists());
        assert!(checkout.shipped().join(SELECTION_FILE).is_file());

        checkout.select(&[]);
        assert_eq!(
            changes(&checkout.plan(false).apply().unwrap()),
            ["hello removed"]
        );
    }

    #[test]
    fn what_a_bundle_no_longer_names_is_removed_with_its_empty_directories() {
        let checkout = Checkout::new();
        checkout.hello();
        checkout.plan(false).apply().unwrap();
        let hello = checkout.shipped().join("hello");
        std::fs::write(hello.join(".plugin.yaml.1.tmp"), "left behind").unwrap();

        checkout.bundle(
            "hello",
            "verbs:\n  - {name: hello, usage: \"hello\", bin: tx-hello}\n",
        );
        let report = checkout.plan(false).apply().unwrap();
        assert_eq!(changes(&report), ["hello"]);
        assert!(report.programs.is_empty());
        assert!(hello.join("bin/tx-hello").is_file());
        assert!(!hello.join("bin/up").exists());
        assert!(!hello.join("keys").exists(), "the emptied directory goes");
        assert!(!hello.join(".plugin.yaml.1.tmp").exists());
    }

    #[test]
    fn a_link_in_the_way_is_removed_and_never_entered() {
        let checkout = Checkout::new();
        checkout.hello();
        let precious = checkout.dir.path().join("precious");
        std::fs::create_dir_all(precious.join("bin")).unwrap();
        std::fs::write(precious.join("bin/keep"), "x").unwrap();
        std::fs::create_dir_all(checkout.shipped()).unwrap();
        // The plugin's directory is a link, and so is the directory of another one.
        std::os::unix::fs::symlink(&precious, checkout.shipped().join("hello")).unwrap();
        std::os::unix::fs::symlink(&precious, checkout.shipped().join("gone")).unwrap();

        let report = checkout.plan(false).apply().unwrap();
        assert_eq!(changes(&report), ["gone removed", "hello"]);
        let hello = checkout.shipped().join("hello");
        assert!(hello.symlink_metadata().unwrap().is_dir());
        assert!(hello.join("bin/tx-hello").is_file());
        assert!(checkout.shipped().join("gone").symlink_metadata().is_err());
        assert_eq!(
            std::fs::read_to_string(precious.join("bin/keep")).unwrap(),
            "x"
        );
        assert_eq!(std::fs::read_dir(precious.join("bin")).unwrap().count(), 1);
    }

    #[test]
    fn every_missing_file_is_named_and_nothing_is_written() {
        let checkout = Checkout::new();
        checkout.hello();
        checkout.plan(false).apply().unwrap();
        let installed = checkout.shipped().join("hello/bin/tx-hello");
        let before = inode(&installed);

        std::fs::remove_file(checkout.binaries().join("tx-hello")).unwrap();
        std::fs::remove_file(checkout.bundles().join("hello/keys/hello.tmux")).unwrap();
        checkout.bundle(
            "second",
            "verbs:\n  - {name: second, usage: \"second\", bin: tx-second}\n",
        );
        checkout.bundle("broken", "colour: red\n");
        let bundles = checkout.bundles();
        let binaries = checkout.binaries();
        let refused = checkout.refused();
        assert_eq!(refused.len(), 4, "{refused:?}");
        assert!(
            refused[0].starts_with(&format!(
                "{}:",
                bundles.join("broken/plugin.yaml").display()
            )),
            "{refused:?}"
        );
        assert_eq!(
            refused[1..],
            [
                format!(
                    "plugin hello: no keys/hello.tmux in {}",
                    bundles.join("hello").display()
                ),
                format!(
                    "plugin hello: no bin/tx-hello in {} and no {}: build it \
                     (cargo build --release -p tx-hello)",
                    bundles.join("hello").display(),
                    binaries.join("tx-hello").display()
                ),
                format!(
                    "plugin second: no bin/tx-second in {} and no {}: build it \
                     (cargo build --release -p tx-second)",
                    bundles.join("second").display(),
                    binaries.join("tx-second").display()
                ),
            ]
        );
        assert_eq!(inode(&installed), before);
        assert!(!checkout.shipped().join("second").exists());
    }

    #[test]
    fn a_selected_id_must_be_a_bundle_of_the_source() {
        let checkout = Checkout::new();
        checkout.hello();
        checkout.select(&["hello", "nope", "../plugins/hello"]);
        let bundles = checkout.bundles();
        assert_eq!(
            checkout.refused(),
            [
                format!(
                    "plugin ../plugins/hello: no {}",
                    bundles.join("../plugins/hello/plugin.yaml").display()
                ),
                format!(
                    "plugin nope: no {}",
                    bundles.join("nope/plugin.yaml").display()
                ),
            ]
        );
        assert!(!checkout.shipped().join("hello").exists());
    }

    #[test]
    fn a_source_that_is_not_a_directory_is_an_error() {
        let checkout = Checkout::new();
        let source = Source {
            bundles: checkout.dir.path().join("nowhere"),
            binaries: checkout.binaries(),
            link: false,
        };
        let errors = plan(&source, &checkout.shipped()).unwrap_err();
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0]
                .to_string()
                .starts_with(&format!("{}: ", source.bundles.display())),
            "{}",
            errors[0]
        );
    }

    fn resources(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(name, text)| ((*name).to_owned(), text.as_bytes().to_vec()))
            .collect()
    }

    fn mode(path: &Path) -> u32 {
        path.symlink_metadata().unwrap().permissions().mode() & PERMISSIONS
    }

    #[test]
    fn roles_are_published_under_roles() {
        let data = Path::new("/d");
        assert_eq!(
            published_path(data, "agents/skills/x/SKILL.md"),
            Path::new("/d/roles/skills/x/SKILL.md")
        );
        assert_eq!(kind_dir(data, "agents"), Path::new("/d/roles"));
        assert_eq!(kind_dir(data, "keys"), Path::new("/d/keys"));
        assert_eq!(
            published_path(data, "definitions/skills/x/SKILL.md"),
            Path::new("/d/definitions/skills/x/SKILL.md")
        );
        assert_eq!(
            published_path(data, "bin/tmux-nav"),
            Path::new("/d/bin/tmux-nav")
        );
    }

    #[test]
    fn publishes_each_resource_to_its_place_with_its_mode() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        publish(
            data,
            &resources(&[
                ("keys/hello.tmux", "# keys: H\n"),
                ("bin/tmux-nav", "#!/bin/sh\n"),
                ("conf/tmux.conf", "set -g x y\n"),
                ("nvim/lua/tx.lua", "-- lua\n"),
                ("agents/COMMON.md", "common\n"),
                ("definitions/agents/developer.md", "developer\n"),
            ]),
        )
        .unwrap();
        for (file, text, wanted) in [
            ("keys/hello.tmux", "# keys: H\n", 0o755),
            ("bin/tmux-nav", "#!/bin/sh\n", 0o755),
            ("conf/tmux.conf", "set -g x y\n", 0o644),
            ("nvim/lua/tx.lua", "-- lua\n", 0o644),
            ("roles/COMMON.md", "common\n", 0o644),
            ("definitions/agents/developer.md", "developer\n", 0o644),
        ] {
            let path = data.join(file);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{file}");
            assert_eq!(mode(&path), wanted, "{file}");
        }
        assert!(!data.join("agents").exists(), "that is where records live");
    }

    #[test]
    fn an_unchanged_resource_is_left_alone_and_a_changed_one_is_a_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        let first = resources(&[("keys/a.tmux", "one\n"), ("conf/b.conf", "two\n")]);
        publish(data, &first).unwrap();
        let (fragment, conf) = (data.join("keys/a.tmux"), data.join("conf/b.conf"));
        let (fragment_before, conf_before) = (inode(&fragment), inode(&conf));

        publish(data, &first).unwrap();
        assert_eq!(inode(&fragment), fragment_before);

        publish(
            data,
            &resources(&[("keys/a.tmux", "changed\n"), ("conf/b.conf", "two\n")]),
        )
        .unwrap();
        assert_ne!(inode(&fragment), fragment_before);
        assert_eq!(std::fs::read_to_string(&fragment).unwrap(), "changed\n");
        assert_eq!(inode(&conf), conf_before);

        // The right bytes with the wrong mode are put right.
        std::fs::set_permissions(&fragment, std::fs::Permissions::from_mode(0o644)).unwrap();
        publish(
            data,
            &resources(&[("keys/a.tmux", "changed\n"), ("conf/b.conf", "two\n")]),
        )
        .unwrap();
        assert_eq!(mode(&fragment), 0o755);
        assert_eq!(std::fs::read_dir(data.join("keys")).unwrap().count(), 1);
    }

    #[test]
    fn what_no_resource_accounts_for_is_pruned_from_the_six_directories_only() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path();
        publish(data, &resources(&[("keys/a.tmux", "a\n")])).unwrap();
        let stale = [
            "bin/old-script",
            "keys/old.tmux",
            "keys/.a.tmux.1.tmp",
            "conf/old.conf",
            "nvim/lua/old.lua",
            "roles/OLD.md",
            "definitions/skills/old/SKILL.md",
        ];
        let kept = [
            "keys/.installed",
            "agents/1/agent.json",
            "plugins/a/data",
            "tmux.conf",
        ];
        for file in stale.iter().chain(&kept) {
            let path = data.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        publish(data, &resources(&[("keys/a.tmux", "a\n")])).unwrap();
        for file in stale {
            assert!(!data.join(file).exists(), "{file}");
        }
        for file in kept {
            assert!(data.join(file).is_file(), "{file}");
        }
        assert!(data.join("keys/a.tmux").is_file());
        assert!(
            data.join("definitions/skills/old").is_dir(),
            "directories stay"
        );
    }

    #[test]
    fn a_link_among_the_published_files_is_replaced_or_removed_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let victim = dir.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("precious"), "x").unwrap();
        std::fs::create_dir_all(data.join("bin")).unwrap();
        std::os::unix::fs::symlink(victim.join("precious"), data.join("bin/script")).unwrap();
        std::os::unix::fs::symlink(&victim, data.join("bin/link")).unwrap();

        publish(&data, &resources(&[("bin/script", "#!/bin/sh\n")])).unwrap();
        assert!(
            data.join("bin/script")
                .symlink_metadata()
                .unwrap()
                .is_file()
        );
        assert!(data.join("bin/link").symlink_metadata().is_err());
        assert_eq!(
            std::fs::read_to_string(victim.join("precious")).unwrap(),
            "x"
        );
        assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);
    }

    #[test]
    fn a_symlinked_kind_directory_is_refused_before_a_byte_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let victim = dir.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("precious"), "x").unwrap();
        std::fs::create_dir_all(&data).unwrap();
        // The last kind checked: nothing of the earlier ones may be written either.
        std::os::unix::fs::symlink(&victim, data.join("definitions")).unwrap();

        let error = publish(
            &data,
            &resources(&[("keys/a.tmux", "a\n"), ("definitions/mcp.json", "{}\n")]),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "{} is a symlink; refusing to prune",
                data.join("definitions").display()
            )
        );
        assert!(!data.join("keys").exists());
        assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);

        std::fs::remove_file(data.join("definitions")).unwrap();
        std::os::unix::fs::symlink(&victim, data.join("roles")).unwrap();
        let error = publish(&data, &resources(&[("agents/COMMON.md", "c\n")])).unwrap_err();
        assert!(matches!(error, Error::SymlinkedKind(path) if path == data.join("roles")));
    }
}
