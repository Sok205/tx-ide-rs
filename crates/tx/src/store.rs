//! `SessionStore` (store.py): the repository over `<sessions dir>/<id>.json`, filesystem-direct
//! (atomic temp-file + rename writes, directory scans) — never through `storage::Storage`.
//!
//! Unreadable records met by a scan are reported to a [`SkipSink`] given at construction instead
//! of being printed from inside the store. Q19 FIX ("printed once per invocation"): one invocation
//! may scan several times (`tx ls` reconciles, then lists); the CLI shares one [`WarnOnce`] across
//! every store it builds, which prints each distinct skip line the first time only.
//!
//! Q26 FIX: `load` of a named unreadable record returns [`StoreError::Unreadable`] (naming the
//! file) instead of the reference's traceback.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::pyjson;
use crate::session::{RecordError, Session};

/// Why one record file could not be read. `Display` follows the reference's `str(error)` where
/// a test can see it (record errors); IO / JSON texts are the port's own.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("{0}")]
    Io(io::Error),
    /// `UnicodeDecodeError` text.
    #[error("'utf-8' codec can't decode {what}: {reason}")]
    Utf8 { what: String, reason: &'static str },
    #[error("{0}")]
    Json(serde_json::Error),
    #[error("{0}")]
    Record(#[from] RecordError),
}

impl ReadError {
    pub fn is_unsupported(&self) -> bool {
        matches!(self, ReadError::Record(RecordError::Unsupported(_)))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("unreadable record {file}: {source}")]
    Unreadable { file: String, source: ReadError },
}

/// One record a scan skipped. `Display` is the reference's stderr line (without newline).
#[derive(Debug)]
pub struct Skipped {
    pub file: String,
    pub error: ReadError,
}

impl fmt::Display for Skipped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "tx: skipping unreadable record {}: {}",
            self.file, self.error
        )
    }
}

/// Where a scan reports the records it skipped.
pub trait SkipSink {
    fn skipped(&self, skipped: &Skipped);
}

/// Writes each distinct skip line once (Q19 FIX); share one per invocation.
pub struct WarnOnce<W: Write> {
    out: RefCell<W>,
    seen: RefCell<HashSet<String>>,
}

impl<W: Write> WarnOnce<W> {
    pub fn new(out: W) -> Self {
        Self {
            out: RefCell::new(out),
            seen: RefCell::new(HashSet::new()),
        }
    }

    pub fn into_inner(self) -> W {
        self.out.into_inner()
    }
}

impl WarnOnce<io::Stderr> {
    pub fn stderr() -> Self {
        Self::new(io::stderr())
    }
}

impl<W: Write> SkipSink for WarnOnce<W> {
    fn skipped(&self, skipped: &Skipped) {
        let line = skipped.to_string();
        if self.seen.borrow_mut().insert(line.clone()) {
            // A warning that cannot be written has nowhere else to go.
            let _ = writeln!(self.out.borrow_mut(), "{line}");
        }
    }
}

/// Drops skip reports (for internal lookups whose caller already warned).
pub struct IgnoreSkips;

impl SkipSink for IgnoreSkips {
    fn skipped(&self, _skipped: &Skipped) {}
}

pub struct SessionStore {
    dir: PathBuf,
    skips: Rc<dyn SkipSink>,
}

impl SessionStore {
    /// `dir` is `Home::sessions_dir()` unless a caller needs another store.
    pub fn new(dir: impl Into<PathBuf>, skips: Rc<dyn SkipSink>) -> Self {
        Self {
            dir: dir.into(),
            skips,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{session_id}.json"))
    }

    /// Atomic write: `.{id}.XXXXXXXX.tmp` in the same directory (mode 0600, like `mkstemp`), then
    /// rename over `<id>.json`. The temp file is removed if anything fails before the rename.
    pub fn save(&self, session: &Session) -> Result<(), StoreError> {
        let io_err = |path: &Path| {
            let path = path.to_path_buf();
            move |source| StoreError::Io { path, source }
        };
        std::fs::create_dir_all(&self.dir).map_err(io_err(&self.dir))?;
        let payload = pyjson::dumps_pretty(&session.to_value());
        let prefix = format!(".{}.", session.id);
        let mut temp = tempfile::Builder::new()
            .prefix(&prefix)
            .suffix(".tmp")
            .rand_bytes(8)
            .tempfile_in(&self.dir)
            .map_err(io_err(&self.dir))?;
        temp.write_all(payload.as_bytes())
            .map_err(io_err(temp.path()))?;
        let target = self.path(&session.id);
        temp.persist(&target)
            .map_err(|error| io_err(&target)(error.error))?;
        Ok(())
    }

    /// One record by id; `None` if there is no such file. A named unreadable record is an error.
    pub fn load(&self, session_id: &str) -> Result<Option<Session>, StoreError> {
        let path = self.path(session_id);
        match read(&path) {
            Ok(session) => Ok(Some(session)),
            Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StoreError::Unreadable {
                file: file_name(&path),
                source,
            }),
        }
    }

    /// Current display name of each given id that loads. The reference skips only unsupported
    /// versions (and crashes on the rest); here every unreadable record is omitted.
    pub fn names_for<'a>(
        &self,
        session_ids: impl IntoIterator<Item = &'a str>,
    ) -> HashMap<String, String> {
        let mut names = HashMap::new();
        for session_id in session_ids {
            if names.contains_key(session_id) {
                continue;
            }
            if let Ok(Some(session)) = self.load(session_id) {
                names.insert(session_id.to_owned(), session.name);
            }
        }
        names
    }

    /// Every `*.json` entry (hidden files included, like `Path.glob`), sorted by file name.
    /// A record deleted mid-scan is skipped silently; any other unreadable one is reported to the
    /// sink and skipped. A missing directory is an empty store.
    pub fn all(&self) -> Vec<Session> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.ends_with(".json"))
            .collect();
        names.sort();
        let mut sessions = Vec::with_capacity(names.len());
        for name in names {
            match read(&self.dir.join(&name)) {
                Ok(session) => sessions.push(session),
                Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => self.skips.skipped(&Skipped { file: name, error }),
            }
        }
        sessions
    }

    /// The first live record with this name, else the first (id-sorted) match of any state.
    pub fn find_by_name(&self, name: &str) -> Option<Session> {
        let mut first = None;
        for session in self.all() {
            if session.name != name {
                continue;
            }
            if session.is_alive() {
                return Some(session);
            }
            first.get_or_insert(session);
        }
        first
    }

    pub fn query(&self, predicate: impl Fn(&Session) -> bool) -> Vec<Session> {
        self.all().into_iter().filter(|s| predicate(s)).collect()
    }

    /// Remove a record file; whether one was removed.
    pub fn delete(&self, session_id: &str) -> Result<bool, StoreError> {
        let path = self.path(session_id);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn read(path: &Path) -> Result<Session, ReadError> {
    let bytes = std::fs::read(path).map_err(ReadError::Io)?;
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        let start = error.valid_up_to();
        let byte = bytes[start];
        let count = error.error_len().unwrap_or(bytes.len() - start);
        let what = if count == 1 {
            format!("byte 0x{byte:02x} in position {start}")
        } else {
            format!("bytes in position {start}-{}", start + count - 1)
        };
        let reason = match error.error_len() {
            None => "unexpected end of data",
            Some(_) if (0x80..0xc2).contains(&byte) || byte > 0xf4 => "invalid start byte",
            Some(_) => "invalid continuation byte",
        };
        ReadError::Utf8 { what, reason }
    })?;
    let value = serde_json::from_str(text).map_err(ReadError::Json)?;
    Ok(Session::from_value(&value)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::State;

    #[derive(Default)]
    struct Collect(RefCell<Vec<String>>);

    impl SkipSink for Collect {
        fn skipped(&self, skipped: &Skipped) {
            self.0.borrow_mut().push(skipped.to_string());
        }
    }

    const RECORD: &str = r#"{"schema_version": 6, "id": "ID", "name": "NAME", "role": "llm", "state": "STATE", "cwd": "/r", "cmd": "", "tags": [], "group": null, "env": {}, "parent": null, "pid": null, "attached_to": [], "created_at": 900.0, "ended_at": null, "engine": "claude", "last_activity": null, "chats": [], "turn_started_at": null}"#;

    fn record(id: &str, name: &str, state: &str) -> String {
        RECORD
            .replace("\"ID\"", &format!("\"{id}\""))
            .replace("NAME", name)
            .replace("STATE", state)
    }

    fn setup() -> (tempfile::TempDir, Rc<Collect>, SessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let sink = Rc::new(Collect::default());
        let store = SessionStore::new(dir.path().join("sessions"), sink.clone());
        (dir, sink, store)
    }

    fn write(store: &SessionStore, file: &str, text: &str) {
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(store.dir().join(file), text).unwrap();
    }

    #[test]
    fn save_is_atomic_pretty_json_and_round_trips() {
        let (_dir, _sink, store) = setup();
        assert!(store.all().is_empty());
        let value: serde_json::Value = serde_json::from_str(&record("abc", "a", "idle")).unwrap();
        let session = Session::from_value(&value).unwrap();
        store.save(&session).unwrap();
        let entries: Vec<_> = std::fs::read_dir(store.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(entries, ["abc.json"]);
        let written = std::fs::read_to_string(store.path("abc")).unwrap();
        assert!(written.starts_with("{\n  \"schema_version\": 6,"));
        assert!(written.ends_with('}'));
        assert_eq!(store.load("abc").unwrap(), Some(session.clone()));
        let mut renamed = session;
        renamed.name = "b".into();
        store.save(&renamed).unwrap();
        assert_eq!(store.load("abc").unwrap().unwrap().name, "b");
        assert!(store.load("zzz").unwrap().is_none());
    }

    #[test]
    fn scan_is_sorted_tolerant_and_reports_skips() {
        let (_dir, sink, store) = setup();
        write(&store, "b.json", &record("b", "b", "exited"));
        write(&store, "a.json", &record("a", "a", "exited"));
        write(
            &store,
            "c.json",
            r#"{"schema_version": 3, "id": "c", "name": "c"}"#,
        );
        write(&store, "d.json", "{not json");
        write(
            &store,
            "e.json",
            &record("e", "e", "idle").replace(r#""cmd": "", "#, ""),
        );
        write(&store, "f.json", &record("f", "f", "bogus"));
        write(&store, "g.json", "\u{0}");
        std::fs::write(store.dir().join("h.json"), b"\xff").unwrap();
        std::fs::write(store.dir().join("i.json"), b"ab\xe2\x82").unwrap();
        write(&store, ".x.tmp", "garbage");
        write(&store, "notes.txt", "not a record");
        let ids: Vec<_> = store.all().into_iter().map(|s| s.id).collect();
        assert_eq!(ids, ["a", "b"]);
        let lines = sink.0.borrow();
        assert_eq!(lines.len(), 7);
        assert_eq!(
            lines[0],
            "tx: skipping unreadable record c.json: record schema_version=3 is unsupported \
             (expected 6); tx-ide does not back-migrate older records on load (§9) — run `tx \
             migrate` to upgrade older records"
        );
        assert!(lines[1].starts_with("tx: skipping unreadable record d.json: "));
        assert_eq!(lines[2], "tx: skipping unreadable record e.json: 'cmd'");
        assert_eq!(
            lines[3],
            "tx: skipping unreadable record f.json: 'bogus' is not a valid State"
        );
        // CPython: "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte".
        assert_eq!(
            lines[5],
            "tx: skipping unreadable record h.json: 'utf-8' codec can't decode byte 0xff in \
             position 0: invalid start byte"
        );
        assert_eq!(
            lines[6],
            "tx: skipping unreadable record i.json: 'utf-8' codec can't decode bytes in \
             position 2-3: unexpected end of data"
        );
    }

    #[test]
    fn warn_once_dedups_across_scans() {
        let dir = tempfile::tempdir().unwrap();
        let sink = Rc::new(WarnOnce::new(Vec::new()));
        let store = SessionStore::new(dir.path(), sink.clone());
        write(&store, "k.json", "{}");
        store.all();
        store.all();
        drop(store);
        let out = Rc::try_unwrap(sink).ok().unwrap().into_inner();
        assert_eq!(
            String::from_utf8(out).unwrap().lines().count(),
            1,
            "skip line printed more than once"
        );
    }

    #[test]
    fn load_of_unreadable_record_names_the_file() {
        let (_dir, _sink, store) = setup();
        write(&store, "bad.json", "{not json");
        write(&store, "v4.json", r#"{"schema_version": 4, "id": "v4"}"#);
        let bad = store.load("bad").unwrap_err().to_string();
        assert!(bad.starts_with("unreadable record bad.json: "), "{bad}");
        let old = store.load("v4").unwrap_err();
        assert!(
            old.to_string()
                .contains("record schema_version=4 is unsupported")
        );
    }

    #[test]
    fn find_by_name_prefers_live_and_names_for_skips_unreadable() {
        let (_dir, _sink, store) = setup();
        write(&store, "a.json", &record("a", "w", "exited"));
        write(&store, "b.json", &record("b", "w", "idle"));
        write(&store, "c.json", &record("c", "x", "archived"));
        write(&store, "d.json", r#"{"schema_version": 3}"#);
        assert_eq!(store.find_by_name("w").unwrap().id, "b");
        assert_eq!(store.find_by_name("x").unwrap().id, "c");
        assert!(store.find_by_name("nope").is_none());
        let names = store.names_for(["a", "c", "d", "zz", "a"]);
        assert_eq!(names.len(), 2);
        assert_eq!(names["a"], "w");
        let history = store.query(|s| s.state.is_terminal());
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|s| s.state != State::Idle));
    }

    #[test]
    fn delete_reports_whether_removed() {
        let (_dir, _sink, store) = setup();
        write(&store, "abc.json", &record("abc", "abc", "exited"));
        assert!(store.delete("abc").unwrap());
        assert!(!store.delete("abc").unwrap());
    }
}
