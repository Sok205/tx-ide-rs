//! `SessionStore`: `<dir>/<id>.json`, one file per record, written atomically (temp file in the
//! same directory, then rename) so a reader never sees a half-written record.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::session::{RecordError, Session, SessionId};

const RECORD_SUFFIX: &str = ".json";
const TEMP_SUFFIX: &str = ".tmp";

/// Why one record file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("not UTF-8: {0}")]
    Utf8(std::str::Utf8Error),
    #[error("{0}")]
    Record(#[from] RecordError),
    #[error("not a regular file")]
    NotAFile,
    #[error("record carries id {found:?}, which is not the file's name")]
    IdMismatch { found: String },
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// A record file exists but cannot be read. Named so a human can inspect or remove it.
    #[error("unreadable record {file}: {source}")]
    Unreadable {
        file: String,
        #[source]
        source: ReadError,
    },
    /// The session cannot be written as JSON (non-UTF-8 `cwd`, non-finite timestamp).
    #[error("cannot encode record {id}: {source}")]
    Encode {
        id: SessionId,
        #[source]
        source: RecordError,
    },
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_owned(),
        source,
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, id: &SessionId) -> PathBuf {
        self.dir.join(format!("{id}{RECORD_SUFFIX}"))
    }

    /// Atomic write: `.<id>.XXXXXXXX.tmp` in the same directory (mode 0600), then rename over
    /// `<id>.json`. The temp file is removed if anything fails before the rename.
    pub fn save(&self, session: &Session) -> Result<(), StoreError> {
        let payload = session.to_json().map_err(|source| StoreError::Encode {
            id: session.id.clone(),
            source,
        })?;
        std::fs::create_dir_all(&self.dir).map_err(io_error(&self.dir))?;
        let prefix = format!(".{}.", session.id);
        let mut temp = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(TEMP_SUFFIX)
            .rand_bytes(8)
            .tempfile_in(&self.dir)
            .map_err(io_error(&self.dir))?;
        temp.write_all(payload.as_bytes())
            .map_err(io_error(temp.path()))?;
        temp.as_file().sync_all().map_err(io_error(temp.path()))?;
        let target = self.path(&session.id);
        temp.persist(&target)
            .map_err(|error| io_error(&target)(error.error))?;
        // The rename itself is durable only once the directory is synced.
        std::fs::File::open(&self.dir)
            .and_then(|dir| dir.sync_all())
            .map_err(io_error(&self.dir))?;
        Ok(())
    }

    /// One record by id; `None` if there is no such file.
    pub fn load(&self, id: &SessionId) -> Result<Option<Session>, StoreError> {
        let path = self.path(id);
        match read(&path, id) {
            Ok(session) => Ok(Some(session)),
            Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StoreError::Unreadable {
                file: file_name(&path),
                source,
            }),
        }
    }

    /// Removes the record file. `Ok(false)` when there was none.
    pub fn delete(&self, id: &SessionId) -> Result<bool, StoreError> {
        let path = self.path(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    /// Every record, sorted by `created_at` then id. A missing directory is an empty store. A
    /// record deleted between the directory scan and its read is skipped; any other unreadable
    /// record fails the scan by name. Entries that are not `<name>.json` regular files (temp
    /// files, hidden files, directories) are ignored.
    pub fn all(&self) -> Result<Vec<Session>, StoreError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(io_error(&self.dir)(source)),
        };
        let mut sessions = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io_error(&self.dir))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') || !name.ends_with(RECORD_SUFFIX) {
                continue;
            }
            let path = entry.path();
            // Only a record file can be a record: a directory, FIFO or socket named `x.json`
            // is skipped (a FIFO would block `open` forever), and one that vanished since the
            // scan is a deletion that raced us.
            let Ok(id) = name[..name.len() - RECORD_SUFFIX.len()].parse::<SessionId>() else {
                continue;
            };
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => {}
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(StoreError::Unreadable {
                        file: name.to_owned(),
                        source: ReadError::Io(source),
                    });
                }
            }
            match read(&path, &id) {
                Ok(session) => sessions.push(session),
                Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => {
                    return Err(StoreError::Unreadable {
                        file: name.to_owned(),
                        source,
                    });
                }
            }
        }
        sessions.sort_by(|a, b| {
            a.created_at
                .total_cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(sessions)
    }
}

/// Reads the record at `path`, which must be a regular file whose record carries `id`.
fn read(path: &Path, id: &SessionId) -> Result<Session, ReadError> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(ReadError::NotAFile);
    }
    let bytes = std::fs::read(path)?;
    let text = std::str::from_utf8(&bytes).map_err(ReadError::Utf8)?;
    let session = Session::from_json(text)?;
    if session.id != *id {
        return Err(ReadError::IdMismatch {
            found: session.id.to_string(),
        });
    }
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(id: &str) -> SessionId {
        id.parse().unwrap()
    }
    use crate::session::{Launch, Lifecycle};
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn session(id: &str, created_at: f64) -> Session {
        Session {
            id: sid(id),
            name: format!("n-{id}"),
            parent: None,
            tags: vec![],
            cwd: PathBuf::from("/"),
            created_at,
            ended_at: None,
            lifecycle: Lifecycle::Running,
            launch: Launch::Process {
                program: "sh".into(),
                args: vec![],
            },
            ext: std::collections::BTreeMap::new(),
            status: None,
        }
    }

    fn store() -> (tempfile::TempDir, SessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("sessions"));
        (dir, store)
    }

    #[test]
    fn save_then_load_round_trips() {
        let (_dir, store) = store();
        let session = session("a", 1.0);
        store.save(&session).unwrap();
        assert_eq!(store.load(&session.id).unwrap(), Some(session));
    }

    #[test]
    fn path_is_id_dot_json() {
        let store = SessionStore::new("/s");
        assert_eq!(store.path(&sid("abc")), PathBuf::from("/s/abc.json"));
        assert_eq!(store.dir(), Path::new("/s"));
    }

    #[test]
    fn load_of_absent_id_is_none() {
        let (_dir, store) = store();
        assert_eq!(store.load(&sid("nope")).unwrap(), None);
    }

    #[test]
    fn all_on_missing_dir_is_empty() {
        let (_dir, store) = store();
        assert!(store.all().unwrap().is_empty());
    }

    #[test]
    fn all_is_sorted_by_created_at_then_id() {
        let (_dir, store) = store();
        for (id, at) in [("c", 2.0), ("a", 2.0), ("b", 1.0)] {
            store.save(&session(id, at)).unwrap();
        }
        let ids: Vec<String> = store
            .all()
            .unwrap()
            .iter()
            .map(|s| s.id.to_string())
            .collect();
        assert_eq!(ids, ["b", "a", "c"]);
    }

    #[test]
    fn save_overwrites_atomically_and_leaves_no_temp_file() {
        let (_dir, store) = store();
        let mut session = session("a", 1.0);
        store.save(&session).unwrap();
        session.name = "renamed".into();
        store.save(&session).unwrap();
        assert_eq!(store.load(&session.id).unwrap().unwrap().name, "renamed");
        let entries: Vec<String> = std::fs::read_dir(store.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(entries, ["a.json"]);
    }

    #[test]
    fn save_syncs_the_directory_entry() {
        // Observable only as "still works"; the directory fsync is a crash-safety measure.
        let (_dir, store) = store();
        store.save(&session("a", 1.0)).unwrap();
        store.save(&session("a", 2.0)).unwrap();
        assert_eq!(store.load(&sid("a")).unwrap().unwrap().created_at, 2.0);
    }

    #[test]
    fn record_file_is_private() {
        let (_dir, store) = store();
        let session = session("a", 1.0);
        store.save(&session).unwrap();
        let mode = std::fs::metadata(store.path(&session.id))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn garbage_record_is_unreadable_by_name() {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(store.path(&sid("x")), "{not json").unwrap();
        let error = store.load(&sid("x")).unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, source: ReadError::Record(RecordError::Json(_)) } if file == "x.json"),
            "{error:?}"
        );
        assert!(
            error.to_string().starts_with("unreadable record x.json: "),
            "{error}"
        );
        let error = store.all().unwrap_err();
        assert!(matches!(&error, StoreError::Unreadable { file, .. } if file == "x.json"));
    }

    #[test]
    fn non_utf8_record_is_unreadable() {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(store.path(&sid("x")), b"\xff\xfe{}").unwrap();
        let error = store.load(&sid("x")).unwrap_err();
        assert!(
            matches!(
                &error,
                StoreError::Unreadable {
                    source: ReadError::Utf8(_),
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn newer_version_record_is_unreadable() {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.dir()).unwrap();
        let text = session("x", 1.0)
            .to_json()
            .unwrap()
            .replace("\"version\": 1", "\"version\": 9");
        std::fs::write(store.path(&sid("x")), text).unwrap();
        let error = store.load(&sid("x")).unwrap_err();
        assert!(
            matches!(
                &error,
                StoreError::Unreadable {
                    source: ReadError::Record(RecordError::UnsupportedVersion(9)),
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn scan_skips_a_record_that_vanished_mid_scan() {
        let (_dir, store) = store();
        store.save(&session("a", 1.0)).unwrap();
        // A dangling symlink is what a file deleted between read_dir and read looks like.
        std::os::unix::fs::symlink("/nonexistent/zzz.json", store.dir().join("zzz.json")).unwrap();
        let ids: Vec<String> = store
            .all()
            .unwrap()
            .iter()
            .map(|s| s.id.to_string())
            .collect();
        assert_eq!(ids, ["a"]);
    }

    #[test]
    fn scan_ignores_foreign_and_hidden_entries() {
        let (_dir, store) = store();
        store.save(&session("a", 1.0)).unwrap();
        std::fs::write(store.dir().join("notes.txt"), "x").unwrap();
        std::fs::write(store.dir().join(".a.12345678.tmp"), "x").unwrap();
        std::fs::write(store.dir().join(".hidden.json"), "garbage").unwrap();
        std::fs::create_dir(store.dir().join("sub.json")).unwrap();
        let ids: Vec<String> = store
            .all()
            .unwrap()
            .iter()
            .map(|s| s.id.to_string())
            .collect();
        assert_eq!(ids, ["a"]);
    }

    #[test]
    fn non_regular_files_are_skipped_without_blocking() {
        let (_dir, store) = store();
        store.save(&session("a", 1.0)).unwrap();
        let fifo = store.dir().join("pipe.json");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap()
                .success()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let scanner = store.clone();
        std::thread::spawn(move || {
            let ids: Vec<String> = scanner
                .all()
                .unwrap()
                .iter()
                .map(|s| s.id.to_string())
                .collect();
            let by_name = scanner.load(&sid("pipe"));
            tx.send((ids, by_name.is_err())).unwrap();
        });
        let (ids, load_failed) = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the scan must not block on a FIFO");
        assert_eq!(ids, ["a"]);
        assert!(load_failed, "a named non-regular file is unreadable");
    }

    #[test]
    fn a_record_must_carry_the_id_its_file_name_says() {
        let (_dir, store) = store();
        store.save(&session("a", 1.0)).unwrap();
        std::fs::write(
            store.path(&sid("other")),
            session("inner", 1.0).to_json().unwrap(),
        )
        .unwrap();
        let error = store.load(&sid("other")).unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, source: ReadError::IdMismatch { found } } if file == "other.json" && found == "inner"),
            "{error:?}"
        );
        assert!(matches!(
            store.all().unwrap_err(),
            StoreError::Unreadable { .. }
        ));
        std::fs::remove_file(store.path(&sid("other"))).unwrap();
        std::os::unix::fs::symlink(store.path(&sid("a")), store.dir().join("alias.json")).unwrap();
        let error = store.all().unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, .. } if file == "alias.json"),
            "{error:?}"
        );
    }

    #[test]
    fn delete_reports_whether_a_record_existed() {
        let (_dir, store) = store();
        let session = session("a", 1.0);
        store.save(&session).unwrap();
        assert!(store.delete(&session.id).unwrap());
        assert!(!store.path(&session.id).exists());
        assert!(!store.delete(&session.id).unwrap());
        assert!(!store.delete(&sid("never")).unwrap());
    }

    #[test]
    fn unencodable_session_is_refused_before_touching_disk() {
        let (_dir, store) = store();
        let mut session = session("a", f64::NAN);
        let error = store.save(&session).unwrap_err();
        assert!(
            matches!(&error, StoreError::Encode { id, source: RecordError::NonFiniteTime("created_at") } if id.as_str() == "a"),
            "{error:?}"
        );
        assert!(!store.dir().exists());
        session.created_at = 1.0;
        store.save(&session).unwrap();
    }

    #[test]
    fn dir_that_is_a_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sessions");
        std::fs::write(&file, "").unwrap();
        let store = SessionStore::new(&file);
        let error = store.save(&session("a", 1.0)).unwrap_err();
        assert!(
            matches!(&error, StoreError::Io { path, .. } if *path == file),
            "{error:?}"
        );
        let error = store.all().unwrap_err();
        assert!(
            matches!(&error, StoreError::Io { path, .. } if *path == file),
            "{error:?}"
        );
    }

    #[test]
    fn save_failure_leaves_no_temp_file() {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.dir()).unwrap();
        // The target is a directory: rename over it fails; the temp file must be cleaned up.
        std::fs::create_dir(store.path(&sid("a"))).unwrap();
        assert!(store.save(&session("a", 1.0)).is_err());
        let entries: Vec<String> = std::fs::read_dir(store.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(entries, ["a.json"]);
    }
}
