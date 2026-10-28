//! Record persistence (`ArtifactStore`) and the content directory beside it (`ArtifactContent`),
//! ported from main's `artifact_store.rs`.
//!
//! `<dir>/<id>.json` is written temp-file + fsync + rename and is authoritative: the service
//! writes it LAST. `<dir>/<id>/` holds the artifact's files under their own names; a file tx
//! writes goes through the same temp + rename, so a reader never sees a partial one.

use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::artifact::{Artifact, UnsupportedArtifactError};

#[derive(Debug, thiserror::Error)]
pub enum ArtifactStoreError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Unsupported(#[from] UnsupportedArtifactError),
    /// A named record that failed to load, with its path.
    #[error("{}: {source}", path.display())]
    Record {
        path: PathBuf,
        source: Box<ArtifactStoreError>,
    },
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
            "txc artifact: skipping unreadable artifact {}: {}",
            self.name, self.error
        )
    }
}

/// A tolerant store scan: the readable records (id order) and the skipped files.
#[derive(Debug, Default)]
pub struct Scan {
    pub artifacts: Vec<Artifact>,
    pub skipped: Vec<Skipped>,
}

/// An artifact id is a path fragment: `[A-Za-z0-9_-]+`. Anything else names no record.
pub(crate) fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[derive(Clone, Debug)]
pub struct ArtifactStore {
    directory: PathBuf,
}

impl ArtifactStore {
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

    /// Persist the record atomically ([`tx_core::write_atomic`]).
    pub fn save(&self, artifact: &Artifact) -> io::Result<()> {
        fs::create_dir_all(&self.directory)?;
        let payload = serde_json::to_string_pretty(&artifact.to_value())?;
        tx_core::write_atomic(&self.record_path(&artifact.id), payload.as_bytes())
    }

    /// One record by id; `Ok(None)` when there is no such file (or the id is not a valid one).
    /// A bad record is an error here: the caller named it.
    pub fn load(&self, artifact_id: &str) -> Result<Option<Artifact>, ArtifactStoreError> {
        if !is_valid_id(artifact_id) {
            return Ok(None);
        }
        let path = self.record_path(artifact_id);
        if !path.exists() {
            return Ok(None);
        }
        read_record(&path)
            .map(Some)
            .map_err(|source| ArtifactStoreError::Record {
                path,
                source: Box::new(source),
            })
    }

    /// Every `*.json` record (non-hidden, name order). Unreadable records are skipped, not fatal.
    pub fn all(&self) -> Scan {
        let mut scan = Scan::default();
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return scan,
            // An unreadable store is reported, never shown as empty.
            Err(error) => {
                scan.skipped.push(Skipped {
                    name: self.directory.display().to_string(),
                    error: error.into(),
                });
                return scan;
            }
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
}

fn read_record(path: &Path) -> Result<Artifact, ArtifactStoreError> {
    let bytes = fs::read(path)?;
    let data: serde_json::Value = serde_json::from_slice(&bytes)?;
    Ok(Artifact::from_value(&data)?)
}

/// The files under `<dir>/<id>/`; bytes-clean.
#[derive(Clone, Debug)]
pub struct ArtifactContent {
    directory: PathBuf,
}

impl ArtifactContent {
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
        let dir = root.path().join("plugins/artifacts");
        Fixture {
            store: ArtifactStore::at(&dir),
            content: ArtifactContent::at(&dir),
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
    fn save_is_atomic_pretty_and_0600() {
        let f = fixture();
        let a = artifact("a", "plan.md");
        f.store.save(&a).unwrap();
        let path = f.store.record_path("a");
        let bytes = fs::read_to_string(&path).unwrap();
        assert!(bytes.starts_with("{\n  \"artifact_schema_version\": 4,\n  \"id\": \"a\","));
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(tmp_files(f.store.directory()).is_empty());
        assert_eq!(f.store.load("a").unwrap(), Some(a));
        assert!(f.store.load("missing").unwrap().is_none());
    }

    #[test]
    fn load_refuses_ids_that_are_not_path_fragments() {
        let f = fixture();
        f.store.save(&artifact("a", "plan.md")).unwrap();
        for token in ["", "../a", "a/../a", ".a", "a b"] {
            assert!(f.store.load(token).unwrap().is_none(), "{token:?}");
        }
    }

    #[test]
    fn load_propagates_unsupported() {
        let f = fixture();
        fs::create_dir_all(f.store.directory()).unwrap();
        write(
            f.store.record_path("old"),
            r#"{"artifact_schema_version": 2, "id": "old", "foo": 1}"#,
        );
        let error = f.store.load("old").unwrap_err();
        let ArtifactStoreError::Record { path, source } = &error else {
            panic!("{error:?}");
        };
        assert_eq!(*path, f.store.record_path("old"));
        assert!(matches!(**source, ArtifactStoreError::Unsupported(_)));
        assert!(error.to_string().starts_with(&path.display().to_string()));
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
            r#"{"artifact_schema_version": 2, "id": "old"}"#,
        );
        fs::create_dir(dir.join("x")).unwrap();
        write(dir.join(".good.abc.tmp"), "{}");
        write(dir.join(".hidden.json"), "{");

        let scan = f.store.all();
        let ids: Vec<&str> = scan.artifacts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["b-good", "good"]);
        let warnings: Vec<String> = scan.skipped.iter().map(Skipped::warning).collect();
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].starts_with("txc artifact: skipping unreadable artifact bad.json: "));
        assert!(warnings[1].contains("old.json: record artifact_schema_version=2"));
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
        assert_eq!(f.content.content_dir(&a), dir);
        assert_eq!(f.content.file_path(&a, "plan.md"), dir.join("plan.md"));
    }
}
