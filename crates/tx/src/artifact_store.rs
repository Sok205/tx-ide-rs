//! Record persistence (`ArtifactStore`) and the content directory beside it (`ArtifactContent`).
//!
//! `artifacts/<id>.json` is written atomically (temp file + rename) and is authoritative: the
//! service writes it LAST. `artifacts/<id>/` holds the artifact's files, named as they are; a
//! file written by tx goes through the same temp + rename, so a reader never sees a partial one.

use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::artifact::{Artifact, UnsupportedArtifactError};
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
    /// newline.
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

/// The files under `artifacts/<id>/`; bytes-clean.
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

    pub fn file_path(&self, artifact: &Artifact, name: &str) -> PathBuf {
        self.content_dir(artifact).join(name)
    }

    /// Write `{dir}/<name>` atomically (last write wins between writers).
    pub fn write_file(&self, artifact: &Artifact, name: &str, content: &[u8]) -> io::Result<()> {
        let dir = self.content_dir(artifact);
        fs::create_dir_all(&dir)?;
        atomic_write(&dir, &format!(".{name}."), content, &dir.join(name))
    }

    /// Copy `source` into the directory under its own file name; returns that name.
    pub fn copy_in(&self, artifact: &Artifact, source: &Path) -> io::Result<String> {
        let name = source
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a file path"))?
            .to_string_lossy()
            .into_owned();
        self.write_file(artifact, &name, &fs::read(source)?)?;
        Ok(name)
    }
}

/// Write `content` to a `<prefix>*.tmp` file in `dir` (mode 0600, like `mkstemp`), then rename it
/// onto `target`. The temp file is removed on any failure.
pub(crate) fn atomic_write(
    dir: &Path,
    prefix: &str,
    content: &[u8],
    target: &Path,
) -> io::Result<()> {
    let mut temp = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(".tmp")
        .tempfile_in(dir)?;
    temp.write_all(content)?;
    temp.flush()?;
    temp.persist(target).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifact::Kind;
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

    fn artifact(id: &str, file: &str) -> Artifact {
        Artifact::new(
            id.into(),
            Kind::Document { file: file.into() },
            "T".into(),
            "s1".into(),
            Number::from_f64(1.5).unwrap(),
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
        assert!(bytes.starts_with("{\n  \"artifact_schema_version\": 4,\n  \"id\": \"a\","));
        assert!(!bytes.ends_with('\n'));
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
            r#"{"artifact_schema_version": 3, "id": "old", "foo": 1}"#,
        );
        let error = f.store.load("old").unwrap_err();
        assert!(matches!(error, ArtifactStoreError::Unsupported(_)));
        assert!(
            error
                .to_string()
                .contains("record artifact_schema_version=3 is unsupported (expected 4)")
        );
    }

    #[test]
    fn scan_is_tolerant_and_sorted() {
        let f = fixture();
        let dir = f.store.directory().to_path_buf();
        f.store.save(&artifact("good", "plan.md")).unwrap();
        f.store.save(&artifact("b-good", "plan.md")).unwrap();
        write(dir.join("bad.json"), "{");
        write(
            dir.join("old.json"),
            r#"{"artifact_schema_version": 3, "id": "old"}"#,
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
            "tx: skipping unreadable artifact old.json: record artifact_schema_version=3 is \
             unsupported (expected 4); run `tx migrate` to upgrade older artifact records"
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
    fn content_layout_and_atomic_write() {
        let f = fixture();
        let a = artifact("id", "plan.md");
        let dir = f.store.directory().join("id");
        assert_eq!(f.content.content_dir(&a), dir);
        assert_eq!(f.content.file_path(&a, "plan.md"), dir.join("plan.md"));
        f.content.write_file(&a, "plan.md", b"one").unwrap();
        f.content.write_file(&a, "plan.md", b"two").unwrap();
        assert_eq!(fs::read(dir.join("plan.md")).unwrap(), b"two");
        assert!(tmp_files(&dir).is_empty());
        let mode = fs::metadata(dir.join("plan.md"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn copy_in_uses_the_source_name() {
        let f = fixture();
        let a = artifact("id", "notes.md");
        let source = f._root.path().join("scratch").join("notes.md");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        write(source.clone(), "body");
        assert_eq!(f.content.copy_in(&a, &source).unwrap(), "notes.md");
        assert_eq!(
            fs::read(f.content.file_path(&a, "notes.md")).unwrap(),
            b"body"
        );
        assert!(f.content.copy_in(&a, Path::new("/")).is_err());
    }
}
