//! `SessionStore`: `<dir>/<id>/agent.json`, one directory per agent holding its record, written
//! atomically (temp file in the same directory, then rename) so a reader never sees a
//! half-written record. The directory is the agent's: its engine archive lives next to the
//! record (`CoreConfig::archive_directory`).

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::session::{RecordError, Session, SessionId};

/// The record inside an agent's directory.
pub const RECORD_FILE: &str = "agent.json";
const TEMP_PREFIX: &str = ".agent.";
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
    #[error("record carries id {found:?}, which is not its directory's name")]
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
    /// A record file exists but cannot be read. Named (`<id>/agent.json`) so a human can
    /// inspect or remove it.
    #[error("unreadable record {file}: {source}")]
    Unreadable {
        file: String,
        #[source]
        source: ReadError,
    },
    /// The session cannot be written as JSON (non-UTF-8 `working_directory`, non-finite
    /// timestamp).
    #[error("cannot encode record {id}: {source}")]
    Encode {
        id: SessionId,
        #[source]
        source: RecordError,
    },
}

/// A record every read skips, and why ([`SessionStore::skipped`]).
pub type SkippedRecord = (SessionId, RecordError);

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_owned(),
        source,
    }
}

/// `<id>/agent.json`: how an unreadable record is named.
fn record_name(id: &SessionId) -> String {
    format!("{id}/{RECORD_FILE}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory holding one directory per agent.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `<dir>/<id>`: the agent's directory.
    pub fn agent_dir(&self, id: &SessionId) -> PathBuf {
        self.dir.join(id.to_string())
    }

    /// `<dir>/<id>/agent.json`: the record.
    pub fn path(&self, id: &SessionId) -> PathBuf {
        self.agent_dir(id).join(RECORD_FILE)
    }

    /// Atomic write: `.agent.XXXXXXXX.tmp` in the agent's directory (mode 0600), then rename
    /// over `agent.json`. The temp file is removed if anything fails before the rename.
    pub fn save(&self, session: &Session) -> Result<(), StoreError> {
        let payload = session.to_json().map_err(|source| StoreError::Encode {
            id: session.id.clone(),
            source,
        })?;
        let agent_dir = self.agent_dir(&session.id);
        std::fs::create_dir_all(&agent_dir).map_err(io_error(&agent_dir))?;
        let mut temp = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .suffix(TEMP_SUFFIX)
            .rand_bytes(8)
            .tempfile_in(&agent_dir)
            .map_err(io_error(&agent_dir))?;
        temp.write_all(payload.as_bytes())
            .map_err(io_error(temp.path()))?;
        temp.as_file().sync_all().map_err(io_error(temp.path()))?;
        let target = self.path(&session.id);
        temp.persist(&target)
            .map_err(|error| io_error(&target)(error.error))?;
        // The rename itself is durable only once the directory is synced.
        std::fs::File::open(&agent_dir)
            .and_then(|dir| dir.sync_all())
            .map_err(io_error(&agent_dir))?;
        Ok(())
    }

    /// One record by id; `None` if there is no such file, or if the file is a record this build
    /// skips (see [`SessionStore::skipped`]).
    pub fn load(&self, id: &SessionId) -> Result<Option<Session>, StoreError> {
        match read(&self.path(id), id) {
            Ok(session) => Ok(Some(session)),
            Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(ReadError::Record(RecordError::UnknownEngine(_))) => Ok(None),
            Err(source) => Err(StoreError::Unreadable {
                file: record_name(id),
                source,
            }),
        }
    }

    /// Removes the record file; the agent's directory and whatever else it holds (the archive)
    /// stay. `Ok(false)` when there was none.
    pub fn delete(&self, id: &SessionId) -> Result<bool, StoreError> {
        let path = self.path(id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    /// Every record, sorted by `created_at` then id. A missing directory is an empty store. A
    /// record deleted between the directory scan and its read is skipped, and so is a record
    /// this build does not hold ([`SessionStore::skipped`] names those); any other unreadable
    /// record fails the scan by name. Entries that are not `<id>/agent.json` regular files
    /// (temp files, hidden files, directories not named like an id, agent directories without
    /// a record) are ignored.
    pub fn all(&self) -> Result<Vec<Session>, StoreError> {
        Ok(self.scan()?.0)
    }

    /// The records this build skips, each with why: records naming an engine this build does
    /// not know. Every read skips them. Sorted by id.
    pub fn skipped(&self) -> Result<Vec<SkippedRecord>, StoreError> {
        Ok(self.scan()?.1)
    }

    /// One pass over the directory: the readable records, and the skipped records with why.
    fn scan(&self) -> Result<(Vec<Session>, Vec<SkippedRecord>), StoreError> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((Vec::new(), Vec::new()));
            }
            Err(source) => return Err(io_error(&self.dir)(source)),
        };
        let mut sessions = Vec::new();
        let mut skipped = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io_error(&self.dir))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Only an agent directory can hold a record: a file named like an id is skipped,
            // and so is a directory that vanished since the scan.
            let Ok(id) = name.parse::<SessionId>() else {
                continue;
            };
            match std::fs::metadata(entry.path()) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(StoreError::Unreadable {
                        file: record_name(&id),
                        source: ReadError::Io(source),
                    });
                }
            }
            let path = self.path(&id);
            // Only a record file can be a record: a directory, FIFO or socket named
            // `agent.json` is skipped (a FIFO would block `open` forever), and one that vanished
            // since the scan is a deletion that raced us.
            match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => {}
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => {
                    return Err(StoreError::Unreadable {
                        file: record_name(&id),
                        source: ReadError::Io(source),
                    });
                }
            }
            match read(&path, &id) {
                Ok(session) => sessions.push(session),
                Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
                Err(ReadError::Record(reason @ RecordError::UnknownEngine(_))) => {
                    skipped.push((id, reason));
                }
                Err(source) => {
                    return Err(StoreError::Unreadable {
                        file: record_name(&id),
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
        skipped.sort_by(|a, b| a.0.cmp(&b.0));
        Ok((sessions, skipped))
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
    use crate::engine::EngineKind;
    use crate::state::LlmEngineState;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    fn session(id: &str, created_at: f64) -> Session {
        Session {
            id: sid(id),
            name: format!("n-{id}"),
            engine: EngineKind::Claude,
            working_directory: PathBuf::from("/"),
            roles: vec![],
            state: LlmEngineState::Idle,
            created_at,
            last_modified_at: created_at,
            archived_at: None,
            tags: vec![],
            parent_id: None,
            ext: std::collections::BTreeMap::new(),
            other: serde_json::Map::new(),
        }
    }

    fn store() -> (tempfile::TempDir, SessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::new(dir.path().join("agents"));
        (dir, store)
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn save_then_load_round_trips() {
        let (_dir, store) = store();
        let session = session("a", 1.0);
        store.save(&session).unwrap();
        assert_eq!(store.load(&session.id).unwrap(), Some(session));
    }

    #[test]
    fn path_is_the_agent_directory_and_its_record() {
        let store = SessionStore::new("/s");
        assert_eq!(store.agent_dir(&sid("abc")), PathBuf::from("/s/abc"));
        assert_eq!(store.path(&sid("abc")), PathBuf::from("/s/abc/agent.json"));
        assert_eq!(store.dir(), Path::new("/s"));
    }

    fn ids_of(skipped: &[SkippedRecord]) -> Vec<SessionId> {
        skipped.iter().map(|(id, _)| id.clone()).collect()
    }

    /// A record naming an engine this build does not know (hand-edited, or another build's) is
    /// skipped and named, so the other records still list; a record whose engine is not even a
    /// string is unreadable, as before.
    #[test]
    fn records_with_an_unknown_engine_are_skipped_and_named() {
        let (_dir, store) = store();
        store.save(&session("a", 1.0)).unwrap();
        store.save(&session("k", 2.0)).unwrap();
        let text = std::fs::read_to_string(store.path(&sid("k")))
            .unwrap()
            .replace("\"claude\"", "\"kodex\"");
        assert!(text.contains("\"kodex\""), "{text}");
        std::fs::write(store.path(&sid("k")), text).unwrap();
        let all = store.all().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, sid("a"));
        assert_eq!(store.load(&sid("k")).unwrap(), None);
        let skipped = store.skipped().unwrap();
        assert_eq!(ids_of(&skipped), [sid("k")]);
        assert!(
            matches!(&skipped[0].1, RecordError::UnknownEngine(engine) if engine == "kodex"),
            "{skipped:?}"
        );
        let message = skipped[0].1.to_string();
        assert!(
            message.contains("\"kodex\"") && message.contains("claude, codex"),
            "{message}"
        );
        let (_dir, empty) = self::tests::store();
        assert!(empty.skipped().unwrap().is_empty());
    }

    /// A version-1 file placed where a record belongs is unreadable by name, with the pointer
    /// to `txc migrate`: never a silent skip, never a serde error.
    #[test]
    fn a_version_one_record_is_unreadable_with_the_migrate_hint() {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.agent_dir(&sid("v"))).unwrap();
        std::fs::write(
            store.path(&sid("v")),
            r#"{"version":1,"id":"v","name":"n","parent":null,"tags":[],"cwd":"/","created_at":1.0,"ended_at":null,"lifecycle":"running","launch":{"kind":"agent","engine":"claude","profile":[]}}"#,
        )
        .unwrap();
        let error = store.load(&sid("v")).unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, source: ReadError::Record(RecordError::NeedsMigration(1)) } if file == "v/agent.json"),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "unreadable record v/agent.json: record version 1: run `txc migrate`"
        );
        assert!(matches!(
            store.all().unwrap_err(),
            StoreError::Unreadable { .. }
        ));
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
        assert_eq!(names_in(store.dir()), ["a"]);
        assert_eq!(names_in(&store.agent_dir(&session.id)), [RECORD_FILE]);
    }

    /// The agent's directory is the engine's too: whatever else is in it (the archive) is
    /// left alone by a save and a delete.
    #[test]
    fn the_agent_directory_keeps_its_other_files() {
        let (_dir, store) = store();
        let session = session("a", 1.0);
        let archive = store.agent_dir(&session.id).join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        std::fs::write(archive.join("c.jsonl"), "{}").unwrap();
        store.save(&session).unwrap();
        assert_eq!(
            names_in(&store.agent_dir(&session.id)),
            ["agent.json", "archive"]
        );
        assert!(store.delete(&session.id).unwrap());
        assert_eq!(names_in(&store.agent_dir(&session.id)), ["archive"]);
        assert_eq!(store.load(&session.id).unwrap(), None);
        assert!(
            store.all().unwrap().is_empty(),
            "a directory without a record is not a record"
        );
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
        std::fs::create_dir_all(store.agent_dir(&sid("x"))).unwrap();
        std::fs::write(store.path(&sid("x")), "{not json").unwrap();
        let error = store.load(&sid("x")).unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, source: ReadError::Record(RecordError::Json(_)) } if file == "x/agent.json"),
            "{error:?}"
        );
        assert!(
            error
                .to_string()
                .starts_with("unreadable record x/agent.json: "),
            "{error}"
        );
        let error = store.all().unwrap_err();
        assert!(matches!(&error, StoreError::Unreadable { file, .. } if file == "x/agent.json"));
    }

    #[test]
    fn non_utf8_record_is_unreadable() {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.agent_dir(&sid("x"))).unwrap();
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
        std::fs::create_dir_all(store.agent_dir(&sid("x"))).unwrap();
        let text = session("x", 1.0)
            .to_json()
            .unwrap()
            .replace("\"version\": 2", "\"version\": 9");
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
        // A dangling symlink is what a directory deleted between read_dir and read looks like.
        std::os::unix::fs::symlink("/nonexistent/zzz", store.dir().join("zzz")).unwrap();
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
        std::fs::write(store.dir().join("file-named-like-an-id"), "garbage").unwrap();
        std::fs::create_dir(store.dir().join(".hidden")).unwrap();
        std::fs::write(store.dir().join(".hidden").join(RECORD_FILE), "garbage").unwrap();
        std::fs::create_dir(store.dir().join("not an id")).unwrap();
        std::fs::write(store.dir().join("not an id").join(RECORD_FILE), "garbage").unwrap();
        std::fs::create_dir(store.dir().join("empty")).unwrap();
        std::fs::write(store.agent_dir(&sid("a")).join(".agent.12345678.tmp"), "x").unwrap();
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
        std::fs::create_dir_all(store.agent_dir(&sid("pipe"))).unwrap();
        let fifo = store.path(&sid("pipe"));
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
    fn a_record_must_carry_the_id_its_directory_says() {
        let (_dir, store) = store();
        store.save(&session("a", 1.0)).unwrap();
        std::fs::create_dir_all(store.agent_dir(&sid("other"))).unwrap();
        std::fs::write(
            store.path(&sid("other")),
            session("inner", 1.0).to_json().unwrap(),
        )
        .unwrap();
        let error = store.load(&sid("other")).unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, source: ReadError::IdMismatch { found } } if file == "other/agent.json" && found == "inner"),
            "{error:?}"
        );
        assert!(matches!(
            store.all().unwrap_err(),
            StoreError::Unreadable { .. }
        ));
        std::fs::remove_dir_all(store.agent_dir(&sid("other"))).unwrap();
        std::os::unix::fs::symlink(store.agent_dir(&sid("a")), store.dir().join("alias")).unwrap();
        let error = store.all().unwrap_err();
        assert!(
            matches!(&error, StoreError::Unreadable { file, .. } if file == "alias/agent.json"),
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
        session.last_modified_at = 1.0;
        store.save(&session).unwrap();
    }

    #[test]
    fn dir_that_is_a_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("agents");
        std::fs::write(&file, "").unwrap();
        let store = SessionStore::new(&file);
        let error = store.save(&session("a", 1.0)).unwrap_err();
        assert!(
            matches!(&error, StoreError::Io { path, .. } if path.starts_with(&file)),
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
        // The target is a directory: rename over it fails; the temp file must be cleaned up.
        std::fs::create_dir_all(store.path(&sid("a"))).unwrap();
        assert!(store.save(&session("a", 1.0)).is_err());
        assert_eq!(names_in(&store.agent_dir(&sid("a"))), [RECORD_FILE]);
    }
}
