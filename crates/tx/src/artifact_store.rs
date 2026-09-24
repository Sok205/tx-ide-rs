//! Port of lib/tx/artifact_store.py — record persistence (`ArtifactStore`) and the content files
//! beside it (`ArtifactContent`).
//!
//! `artifacts/<id>.json` is written atomically (temp file + rename) and is authoritative: the
//! service writes it LAST. `artifacts/<id>/current<ext>` is the working copy (atomic, last write
//! wins); `artifacts/<id>/revs/<n><ext>` are immutable snapshots claimed by staging a temp file in
//! `revs/` and hard-linking it onto the slot, so `EEXIST` means a lost race and a crash can only
//! leave a stray `.tmp`.

use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::artifact::{Artifact, UnsupportedArtifactError, py_suffix};
use crate::pyjson::dumps_pretty;
use crate::storage::Home;

#[derive(Debug, thiserror::Error)]
pub enum ArtifactStoreError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Unsupported(#[from] UnsupportedArtifactError),
}

/// A record `ArtifactStore::all` skipped; the caller prints `warning()` to stderr.
#[derive(Debug)]
pub struct Skipped {
    pub name: String,
    pub error: ArtifactStoreError,
}

impl Skipped {
    pub fn warning(&self) -> String {
        format!(
            "tx: skipping unreadable artifact {}: {}",
            self.name, self.error
        )
    }
}

/// Result of a tolerant store scan: the readable records (id order) and the skipped files.
#[derive(Debug, Default)]
pub struct Scan {
    pub artifacts: Vec<Artifact>,
    pub skipped: Vec<Skipped>,
}

#[derive(Clone, Debug)]
pub struct ArtifactStore {
    directory: PathBuf,
}

impl ArtifactStore {
    pub fn new(home: &Home) -> Self {
        Self::at(home.artifacts_dir())
    }

    pub fn at(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn record_path(&self, artifact_id: &str) -> PathBuf {
        self.directory.join(format!("{artifact_id}.json"))
    }

    /// Persist the record atomically: `.<id>.*.tmp` in the store dir, then rename. No trailing
    /// newline (Python `json.dumps(indent=2)`).
    pub fn save(&self, artifact: &Artifact) -> io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let payload = dumps_pretty(&artifact.to_value());
        atomic_write(
            &self.directory,
            &format!(".{}.", artifact.id),
            payload.as_bytes(),
            &self.record_path(&artifact.id),
        )
    }

    /// One record by id; `Ok(None)` when there is no such file. A bad record is an error here (the
    /// caller named it).
    pub fn load(&self, artifact_id: &str) -> Result<Option<Artifact>, ArtifactStoreError> {
        let path = self.record_path(artifact_id);
        if !path.exists() {
            return Ok(None);
        }
        read_record(&path).map(Some)
    }

    /// Every `*.json` record (non-hidden, name order). Unreadable records are skipped, not fatal.
    pub fn all(&self) -> Scan {
        let mut scan = Scan::default();
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return scan;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name().is_some_and(|name| {
                    let name = name.as_bytes();
                    !name.starts_with(b".") && name.ends_with(b".json")
                })
            })
            .collect();
        paths.sort();
        for path in paths {
            match read_record(&path) {
                Ok(artifact) => scan.artifacts.push(artifact),
                Err(error) => scan.skipped.push(Skipped {
                    name: path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    error,
                }),
            }
        }
        scan
    }

    /// Records matching `predicate`, with the scan's skipped files.
    pub fn query(&self, predicate: impl Fn(&Artifact) -> bool) -> Scan {
        let mut scan = self.all();
        scan.artifacts.retain(|artifact| predicate(artifact));
        scan
    }
}

fn read_record(path: &Path) -> Result<Artifact, ArtifactStoreError> {
    let bytes = fs::read(path)?;
    let data: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(Artifact::from_value(&data)?)
}

#[derive(Debug, thiserror::Error)]
pub enum ClaimRevError {
    /// The slot already exists: this write lost (a concurrent write or a crash orphan).
    #[error("rev {0} is already claimed")]
    Taken(u64),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Working copy + revision file I/O under `artifacts/<id>/`; bytes-clean.
#[derive(Clone, Debug)]
pub struct ArtifactContent {
    directory: PathBuf,
}

impl ArtifactContent {
    pub fn new(home: &Home) -> Self {
        Self::at(home.artifacts_dir())
    }

    pub fn at(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn content_dir(&self, artifact: &Artifact) -> PathBuf {
        self.directory.join(&artifact.id)
    }

    pub fn revs_dir(&self, artifact: &Artifact) -> PathBuf {
        self.content_dir(artifact).join("revs")
    }

    pub fn current_path(&self, artifact: &Artifact) -> PathBuf {
        self.content_dir(artifact)
            .join(format!("current{}", artifact.extension()))
    }

    pub fn rev_path(&self, artifact: &Artifact, rev: u64) -> PathBuf {
        self.revs_dir(artifact)
            .join(format!("{rev}{}", artifact.extension()))
    }

    pub fn read_current(&self, artifact: &Artifact) -> io::Result<Vec<u8>> {
        fs::read(self.current_path(artifact))
    }

    pub fn read_rev(&self, artifact: &Artifact, rev: u64) -> io::Result<Vec<u8>> {
        fs::read(self.rev_path(artifact, rev))
    }

    /// Refresh the working copy atomically (last write wins between writers).
    pub fn write_current(&self, artifact: &Artifact, content: &[u8]) -> io::Result<()> {
        let dir = self.content_dir(artifact);
        fs::create_dir_all(&dir)?;
        let path = self.current_path(artifact);
        let prefix = format!(".{}.", file_name(&path));
        atomic_write(&dir, &prefix, content, &path)
    }

    /// Exclusively claim `revs/<rev><ext>`: stage the full content in `revs/.<rev>.*.tmp`, then
    /// hard-link it onto the slot. An existing slot is never overwritten (`Taken`); the staging
    /// file is always removed.
    pub fn claim_rev(
        &self,
        artifact: &Artifact,
        rev: u64,
        content: &[u8],
    ) -> Result<(), ClaimRevError> {
        let revs = self.revs_dir(artifact);
        fs::create_dir_all(&revs)?;
        let target = self.rev_path(artifact, rev);
        let mut staged = tempfile::Builder::new()
            .prefix(&format!(".{rev}."))
            .suffix(".tmp")
            .tempfile_in(&revs)?;
        staged.write_all(content)?;
        staged.flush()?;
        match fs::hard_link(staged.path(), &target) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                Err(ClaimRevError::Taken(rev))
            }
            Err(error) => Err(error.into()),
        }
        // `staged` drops here, unlinking the staging file.
    }

    /// Unlink a rev file (orphan repair only); a missing file is fine.
    pub fn remove_rev(&self, artifact: &Artifact, rev: u64) -> io::Result<()> {
        match fs::remove_file(self.rev_path(artifact, rev)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Rev files on disk the history does not reference (crash orphans), ascending.
    pub fn orphan_revs(&self, artifact: &Artifact) -> io::Result<Vec<u64>> {
        let revs = self.revs_dir(artifact);
        if !revs.exists() {
            return Ok(Vec::new());
        }
        let recorded: Vec<u64> = artifact.history().iter().map(|touch| touch.rev).collect();
        let mut found = Vec::new();
        for entry in fs::read_dir(&revs)? {
            let name = entry?.file_name();
            if let Some(rev) = rev_number(&name.to_string_lossy())
                && !recorded.contains(&rev)
            {
                found.push(rev);
            }
        }
        found.sort_unstable();
        Ok(found)
    }

    /// History revs with no file on disk, in history order.
    pub fn missing_revs(&self, artifact: &Artifact) -> Vec<u64> {
        artifact
            .history()
            .iter()
            .map(|touch| touch.rev)
            .filter(|&rev| !self.rev_path(artifact, rev).exists())
            .collect()
    }

    /// Whether the working copy differs from the latest rev (un-snapshotted edits).
    pub fn current_is_dirty(&self, artifact: &Artifact) -> io::Result<bool> {
        Ok(self.read_current(artifact)? != self.read_rev(artifact, artifact.latest_rev())?)
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Write `content` to a `<prefix>*.tmp` file in `dir` (mode 0600, like `mkstemp`), then rename it
/// onto `target`. The temp file is removed on any failure.
fn atomic_write(dir: &Path, prefix: &str, content: &[u8], target: &Path) -> io::Result<()> {
    let mut temp = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(".tmp")
        .tempfile_in(dir)?;
    temp.write_all(content)?;
    temp.flush()?;
    temp.persist(target).map_err(|error| error.error)?;
    Ok(())
}

/// The rev a `revs/<n><ext>` file name encodes: `None` for a staging temp (leading dot) or a
/// name whose stem (Python `Path.stem`) is not all ASCII digits.
fn rev_number(name: &str) -> Option<u64> {
    if name.starts_with('.') {
        return None;
    }
    let stem = &name[..name.len() - py_suffix(name).len()];
    if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    stem.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::USER_ACTOR;
    use serde_json::Number;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        _root: tempfile::TempDir,
        store: ArtifactStore,
        content: ArtifactContent,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let home = Home::new(root.path());
        Fixture {
            store: ArtifactStore::new(&home),
            content: ArtifactContent::new(&home),
            _root: root,
        }
    }

    fn artifact(id: &str, filename: &str) -> Artifact {
        Artifact::new(
            id.into(),
            None,
            filename.into(),
            Number::from_f64(1.5).unwrap(),
            None,
            "s1".into(),
        )
    }

    fn tmp_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect()
    }

    fn write(path: PathBuf, content: &str) {
        fs::write(path, content).unwrap();
    }

    #[test]
    fn save_is_atomic_pretty_without_newline_and_0600() {
        let f = fixture();
        let a = artifact("a", "plan.md");
        f.store.save(&a).unwrap();
        let path = f.store.record_path("a");
        let bytes = fs::read_to_string(&path).unwrap();
        assert!(bytes.starts_with("{\n  \"artifact_schema_version\": 2,\n  \"id\": \"a\","));
        assert!(!bytes.ends_with('\n'));
        // python3.14: mkstemp + os.replace leaves the record at 0o100600.
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(tmp_files(f.store.directory()).is_empty());
        assert_eq!(f.store.load("a").unwrap(), Some(a));
        assert!(f.store.load("missing").unwrap().is_none());
    }

    #[test]
    fn load_propagates_unsupported() {
        let f = fixture();
        fs::create_dir_all(f.store.directory()).unwrap();
        write(
            f.store.record_path("old"),
            r#"{"artifact_schema_version": 1, "id": "old", "foo": 1}"#,
        );
        let error = f.store.load("old").unwrap_err();
        assert!(matches!(error, ArtifactStoreError::Unsupported(_)));
        assert!(
            error
                .to_string()
                .contains("record artifact_schema_version=1 is unsupported (expected 2)")
        );
    }

    #[test]
    fn scan_is_tolerant_and_sorted() {
        // T-ART-06 store.
        let f = fixture();
        let dir = f.store.directory().to_path_buf();
        f.store.save(&artifact("good", "plan.md")).unwrap();
        f.store.save(&artifact("b-good", "plan.md")).unwrap();
        write(dir.join("bad.json"), "{");
        write(
            dir.join("old.json"),
            r#"{"artifact_schema_version": 1, "id": "old"}"#,
        );
        fs::create_dir(dir.join("x")).unwrap();
        write(dir.join(".good.abc.tmp"), "{}");
        write(dir.join(".hidden.json"), "{");

        let scan = f.store.all();
        let ids: Vec<&str> = scan.artifacts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["b-good", "good"]);
        let warnings: Vec<String> = scan.skipped.iter().map(Skipped::warning).collect();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].starts_with("tx: skipping unreadable artifact bad.json: "));
        assert_eq!(
            warnings[1],
            "tx: skipping unreadable artifact old.json: record artifact_schema_version=1 is \
             unsupported (expected 2); tx-ide does not back-migrate older artifact records"
        );

        let only_good = f.store.query(|a| a.id == "good");
        assert_eq!(only_good.artifacts.len(), 1);
        assert_eq!(only_good.skipped.len(), 2);
    }

    #[test]
    fn scan_of_missing_directory_is_empty() {
        let f = fixture();
        let scan = f.store.all();
        assert!(scan.artifacts.is_empty() && scan.skipped.is_empty());
    }

    #[test]
    fn content_layout() {
        let f = fixture();
        let a = artifact("id", "plan.md");
        let dir = f.store.directory().join("id");
        assert_eq!(f.content.current_path(&a), dir.join("current.md"));
        assert_eq!(f.content.rev_path(&a, 3), dir.join("revs/3.md"));
        let bare = artifact("bare", "artifact");
        f.content.claim_rev(&bare, 0, b"b").unwrap();
        f.content.write_current(&bare, b"b").unwrap();
        let bare_dir = f.store.directory().join("bare");
        let mut names: Vec<String> = fs::read_dir(&bare_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["current", "revs"]);
        assert!(bare_dir.join("revs/0").is_file());
    }

    #[test]
    fn claim_rev_is_exclusive() {
        // T-ART-08: a pre-existing slot is never overwritten and no staging file survives.
        let f = fixture();
        let a = artifact("id", "v0.md");
        f.content.claim_rev(&a, 0, b"v0").unwrap();
        f.content.write_current(&a, b"v0").unwrap();
        write(f.content.rev_path(&a, 1), "other");
        let error = f.content.claim_rev(&a, 1, b"new").unwrap_err();
        assert!(matches!(error, ClaimRevError::Taken(1)));
        assert_eq!(fs::read(f.content.rev_path(&a, 1)).unwrap(), b"other");
        assert_eq!(f.content.read_rev(&a, 0).unwrap(), b"v0");
        assert_eq!(f.content.read_current(&a).unwrap(), b"v0");
        assert!(tmp_files(&f.content.revs_dir(&a)).is_empty());
        assert!(tmp_files(&f.content.content_dir(&a)).is_empty());
        let mode = fs::metadata(f.content.rev_path(&a, 0))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn orphan_missing_dirty_detection() {
        // T-ART-09.
        let f = fixture();
        let mut a = artifact("id", "p.md");
        assert_eq!(f.content.orphan_revs(&a).unwrap(), Vec::<u64>::new());
        f.content.claim_rev(&a, 0, b"p0").unwrap();
        let rev = a.record_touch(USER_ACTOR.into(), Number::from(2), None);
        f.content.claim_rev(&a, rev, b"p1").unwrap();
        f.content.write_current(&a, b"p1").unwrap();
        let revs = f.content.revs_dir(&a);
        for (name, body) in [
            (".1.xyz.tmp", "stage"),
            ("7.md", "orphan"),
            ("notes.txt", "foreign"),
            ("12.txt", "other ext"),
            ("1.5.md", "not a rev"),
        ] {
            write(revs.join(name), body);
        }
        assert_eq!(f.content.orphan_revs(&a).unwrap(), [7, 12]);
        assert!(f.content.missing_revs(&a).is_empty());
        assert!(!f.content.current_is_dirty(&a).unwrap());

        write(f.content.current_path(&a), "c");
        assert!(f.content.current_is_dirty(&a).unwrap());

        fs::remove_file(f.content.rev_path(&a, 1)).unwrap();
        assert_eq!(f.content.missing_revs(&a), [1]);
        assert!(f.content.current_is_dirty(&a).is_err());

        f.content.remove_rev(&a, 7).unwrap();
        f.content.remove_rev(&a, 7).unwrap();
        assert_eq!(f.content.orphan_revs(&a).unwrap(), [12]);
    }

    #[test]
    fn rev_number_matches_python() {
        // Expected values from python3.14 `artifact_store._rev_number`.
        let cases = [
            ("7.md", Some(7)),
            (".1.x.tmp", None),
            ("notes.txt", None),
            ("007.md", Some(7)),
            ("3", Some(3)),
            ("1.5.md", None),
            ("12", Some(12)),
            ("x.7", None),
        ];
        for (name, rev) in cases {
            assert_eq!(rev_number(name), rev, "{name}");
        }
    }
}
