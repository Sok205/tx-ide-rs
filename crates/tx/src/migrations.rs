//! `tx migrate` (migrations/__init__.py): the explicit, idempotent upgrade of an older
//! `$TX_IDE_HOME`.
//!
//! - Sessions v3 / v4 / v5 → v6 in one pass: a process record is re-saved through
//!   `Session::from_value` + `to_value`, which drops dead keys (`kind`, a non-llm's
//!   `engine`/`chats`/`last_activity`) and defaults the new ones (`group`, `turn_started_at`,
//!   `artifact_id`). A v3 VIEW record leaves the store: its live tmux session (matched by the
//!   exact name, Q27) is stamped `@tx_view`, then the record file is deleted.
//! - Artifacts v1 → v2: `group` defaults to null, then the strict loader re-validates.
//!
//! A file that cannot be migrated is skipped with the reason the reference prints
//! (`<ExceptionName>: <message>`, including CPython's `json` decoder texts); nothing aborts the
//! run.

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{Map, Value};

use crate::artifact::{ARTIFACT_SCHEMA_VERSION, Artifact};
use crate::artifact_store::ArtifactStore;
use crate::session::{RecordError, SCHEMA_VERSION, Session, py_repr};
use crate::store::{IgnoreSkips, SessionStore, StoreError};
use crate::tmux::Tmux;

const UPGRADABLE_FROM: [u64; 3] = [3, 4, 5];
const ARTIFACT_UPGRADABLE_FROM: [u64; 1] = [1];
const VIEW_KIND: &str = "view";

/// The outcome of [`migrate_sessions`], in file-name order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SessionsReport {
    /// File names re-saved at the current schema.
    pub migrated: Vec<String>,
    /// View NAMES whose records were retired.
    pub views_removed: Vec<String>,
    /// `(file name, reason)`.
    pub skipped: Vec<(String, String)>,
}

/// The outcome of [`migrate_artifacts`], in file-name order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ArtifactsReport {
    pub migrated: Vec<String>,
    pub skipped: Vec<(String, String)>,
}

/// Why one file was skipped: the Python exception class name and `str(error)`.
struct Skip {
    exception: &'static str,
    message: String,
}

impl Skip {
    fn new(exception: &'static str, message: impl Into<String>) -> Self {
        Self {
            exception,
            message: message.into(),
        }
    }

    fn reason(&self) -> String {
        format!("{}: {}", self.exception, self.message)
    }
}

/// What to do with one readable record.
enum Step {
    Migrated,
    ViewRemoved(String),
    /// Left untouched with a plain reason (already current / not upgradable).
    Untouched(String),
}

/// Migrate every upgradable session record under `directory` in place.
pub fn migrate_sessions(directory: &Path, tmux: &Tmux) -> SessionsReport {
    let store = SessionStore::new(directory, Rc::new(IgnoreSkips));
    let mut report = SessionsReport::default();
    for (name, path) in json_files(directory) {
        match migrate_session(&path, &store, tmux) {
            Ok(Step::Migrated) => report.migrated.push(name),
            Ok(Step::ViewRemoved(view)) => report.views_removed.push(view),
            Ok(Step::Untouched(reason)) => report.skipped.push((name, reason)),
            Err(skip) => report.skipped.push((name, skip.reason())),
        }
    }
    report
}

fn migrate_session(path: &Path, store: &SessionStore, tmux: &Tmux) -> Result<Step, Skip> {
    let raw = read_json(path)?;
    let data = as_dict(&raw)?;
    let version = data.get("schema_version").unwrap_or(&Value::Null);
    if !is_member(version, &UPGRADABLE_FROM)? {
        return Ok(Step::Untouched(if is_number(version, SCHEMA_VERSION) {
            format!("already v{SCHEMA_VERSION}")
        } else {
            format!(
                "not an upgradable record (schema_version={})",
                py_repr(version)
            )
        }));
    }
    if data.get("kind").and_then(Value::as_str) == Some(VIEW_KIND) {
        let name = match data.get("name") {
            None => return Err(Skip::new("KeyError", "'name'")),
            Some(Value::String(name)) => name.clone(),
            Some(other) => {
                return Err(Skip::new(
                    "TypeError",
                    format!(
                        "expected str, bytes or os.PathLike object, not {}",
                        py_type_name(other)
                    ),
                ));
            }
        };
        if tmux.has_session(&name) {
            tmux.set_tx_view(&name)
                .map_err(|error| Skip::new("CalledProcessError", error.to_string()))?;
        }
        std::fs::remove_file(path).map_err(|error| os_error(&error, path))?;
        return Ok(Step::ViewRemoved(name));
    }
    let mut upgraded = data.clone();
    upgraded.insert("schema_version".into(), Value::from(SCHEMA_VERSION));
    let session = Session::from_value(&Value::Object(upgraded)).map_err(record_skip)?;
    store.save(&session).map_err(|error| match error {
        StoreError::Io { path, source } => os_error(&source, &path),
        other => Skip::new("OSError", other.to_string()),
    })?;
    Ok(Step::Migrated)
}

/// Upgrade artifact records in place (v1 → v2 adds a null `group`).
pub fn migrate_artifacts(directory: &Path) -> ArtifactsReport {
    let store = ArtifactStore::at(directory);
    let mut report = ArtifactsReport::default();
    for (name, path) in json_files(directory) {
        match migrate_artifact(&path, &store) {
            Ok(None) => report.migrated.push(name),
            Ok(Some(reason)) => report.skipped.push((name, reason)),
            Err(skip) => report.skipped.push((name, skip.reason())),
        }
    }
    report
}

/// `Ok(None)` when migrated, `Ok(Some(reason))` when left untouched.
fn migrate_artifact(path: &Path, store: &ArtifactStore) -> Result<Option<String>, Skip> {
    let raw = read_json(path)?;
    let data = as_dict(&raw)?;
    let version = data.get("artifact_schema_version").unwrap_or(&Value::Null);
    if !is_member(version, &ARTIFACT_UPGRADABLE_FROM)? {
        return Ok(Some(if is_number(version, ARTIFACT_SCHEMA_VERSION) {
            format!("already v{ARTIFACT_SCHEMA_VERSION}")
        } else {
            format!(
                "not an upgradable artifact record (artifact_schema_version={})",
                py_repr(version)
            )
        }));
    }
    let mut upgraded = data.clone();
    upgraded.insert(
        "artifact_schema_version".into(),
        Value::from(ARTIFACT_SCHEMA_VERSION),
    );
    let group = data.get("group").cloned().unwrap_or(Value::Null);
    upgraded.insert("group".into(), group);
    let artifact = Artifact::from_value(&Value::Object(upgraded))
        .map_err(|error| Skip::new("UnsupportedArtifactError", error.to_string()))?;
    let target = store.record_path(&artifact.id);
    store
        .save(&artifact)
        .map_err(|error| os_error(&error, &target))?;
    Ok(None)
}

/// `sorted(directory.glob("*.json"))`: hidden files included; a missing directory is empty.
fn json_files(directory: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            name.ends_with(".json").then(|| (name, entry.path()))
        })
        .collect();
    files.sort();
    files
}

/// `json.load(open(path))`, with the reference's exception texts.
fn read_json(path: &Path) -> Result<Value, Skip> {
    let bytes = std::fs::read(path).map_err(|error| os_error(&error, path))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| Skip::new("UnicodeDecodeError", unicode_decode_message(&bytes, &error)))?;
    if let Err(message) = py_json_check(text) {
        return Err(Skip::new("JSONDecodeError", message));
    }
    serde_json::from_str(text).map_err(|error| Skip::new("JSONDecodeError", error.to_string()))
}

/// `raw.get(...)` on a non-dict raises `AttributeError`.
fn as_dict(raw: &Value) -> Result<&Map<String, Value>, Skip> {
    raw.as_object().ok_or_else(|| {
        Skip::new(
            "AttributeError",
            format!("'{}' object has no attribute 'get'", py_type_name(raw)),
        )
    })
}

/// `value in frozenset(versions)`: numbers compare by value (`3.0` is in, `True` is not), an
/// unhashable value raises `TypeError`.
fn is_member(value: &Value, versions: &[u64]) -> Result<bool, Skip> {
    match value {
        Value::Array(_) | Value::Object(_) => Err(Skip::new(
            "TypeError",
            format!(
                "cannot use '{0}' as a set element (unhashable type: '{0}')",
                py_type_name(value)
            ),
        )),
        _ => Ok(versions.iter().any(|&version| is_number(value, version))),
    }
}

fn is_number(value: &Value, expected: u64) -> bool {
    value
        .as_f64()
        .is_some_and(|number| number == expected as f64)
}

fn py_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn record_skip(error: RecordError) -> Skip {
    let exception = match error {
        RecordError::Unsupported(_) => "UnsupportedRecordError",
        RecordError::MissingKey(_) => "KeyError",
        RecordError::InvalidEnum { .. } => "ValueError",
        RecordError::WrongType { .. } => "TypeError",
    };
    Skip::new(exception, error.to_string())
}

/// An `OSError` subclass name + `[Errno N] strerror: 'path'`.
fn os_error(error: &io::Error, path: &Path) -> Skip {
    let exception = match error.raw_os_error() {
        Some(libc::ENOENT) => "FileNotFoundError",
        Some(libc::EISDIR) => "IsADirectoryError",
        Some(libc::ENOTDIR) => "NotADirectoryError",
        Some(libc::EACCES | libc::EPERM) => "PermissionError",
        _ => "OSError",
    };
    let message = match error.raw_os_error() {
        Some(code) => {
            let text = error.to_string();
            let strerror = text
                .strip_suffix(&format!(" (os error {code})"))
                .unwrap_or(&text);
            format!(
                "[Errno {code}] {strerror}: {}",
                crate::session::py_repr_str(&path.to_string_lossy())
            )
        }
        None => error.to_string(),
    };
    Skip::new(exception, message)
}

/// CPython's `UnicodeDecodeError` text for invalid UTF-8.
fn unicode_decode_message(bytes: &[u8], error: &std::str::Utf8Error) -> String {
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
    format!("'utf-8' codec can't decode {what}: {reason}")
}

// ----- CPython json decoder errors ----------------------------------------------------------

/// Validate `text` the way CPython's `json.loads` scans it (the C scanner), returning the
/// `JSONDecodeError` text of the first error: `<msg>: line L column C (char P)`, positions in
/// code points. `Ok` means CPython would accept it.
fn py_json_check(text: &str) -> Result<(), String> {
    let chars: Vec<char> = text.chars().collect();
    let fail = |msg: &str, pos: usize| -> String {
        let before = &chars[..pos];
        let lineno = before.iter().filter(|&&c| c == '\n').count() + 1;
        let colno = match before.iter().rposition(|&c| c == '\n') {
            Some(newline) => pos - newline,
            None => pos + 1,
        };
        format!("{msg}: line {lineno} column {colno} (char {pos})")
    };
    if chars.first() == Some(&'\u{feff}') {
        return Err(fail("Unexpected UTF-8 BOM (decode using utf-8-sig)", 0));
    }
    let scanner = Scanner { s: &chars };
    let start = scanner.skip_ws(0);
    let end = scanner.scan_once(start).map_err(|error| match error {
        ScanError::Stop(pos) => fail("Expecting value", pos),
        ScanError::Msg(msg, pos) => fail(msg, pos),
    })?;
    let end = scanner.skip_ws(end);
    if end != chars.len() {
        return Err(fail("Extra data", end));
    }
    Ok(())
}

enum ScanError {
    /// `StopIteration(idx)`: no value starts here.
    Stop(usize),
    Msg(&'static str, usize),
}

struct Scanner<'a> {
    s: &'a [char],
}

impl Scanner<'_> {
    fn at(&self, idx: usize) -> Option<char> {
        self.s.get(idx).copied()
    }

    fn skip_ws(&self, mut idx: usize) -> usize {
        while matches!(self.at(idx), Some(' ' | '\t' | '\n' | '\r')) {
            idx += 1;
        }
        idx
    }

    fn starts_with(&self, idx: usize, word: &str) -> bool {
        word.chars()
            .enumerate()
            .all(|(offset, c)| self.at(idx + offset) == Some(c))
    }

    /// One value at `idx`; the index just past it.
    fn scan_once(&self, idx: usize) -> Result<usize, ScanError> {
        let Some(c) = self.at(idx) else {
            return Err(ScanError::Stop(idx));
        };
        match c {
            '"' => return self.scan_string(idx + 1),
            '{' => return self.scan_object(idx + 1),
            '[' => return self.scan_array(idx + 1),
            _ => {}
        }
        for word in ["null", "true", "false", "NaN", "Infinity", "-Infinity"] {
            if self.starts_with(idx, word) {
                return Ok(idx + word.chars().count());
            }
        }
        self.scan_number(idx)
    }

    fn scan_number(&self, start: usize) -> Result<usize, ScanError> {
        let digit = |idx: usize| self.at(idx).is_some_and(|c| c.is_ascii_digit());
        let mut idx = start;
        if self.at(idx) == Some('-') {
            idx += 1;
        }
        match self.at(idx) {
            Some('1'..='9') => {
                while digit(idx) {
                    idx += 1;
                }
            }
            Some('0') => idx += 1,
            _ => return Err(ScanError::Stop(start)),
        }
        if self.at(idx) == Some('.') && digit(idx + 1) {
            idx += 1;
            while digit(idx) {
                idx += 1;
            }
        }
        if matches!(self.at(idx), Some('e' | 'E')) {
            let mut exponent = idx + 1;
            if matches!(self.at(exponent), Some('+' | '-')) {
                exponent += 1;
            }
            if digit(exponent) {
                while digit(exponent) {
                    exponent += 1;
                }
                idx = exponent;
            }
        }
        Ok(idx)
    }

    /// `scanstring`: `end` is just past the opening quote.
    fn scan_string(&self, end: usize) -> Result<usize, ScanError> {
        let begin = end - 1;
        let len = self.s.len();
        let mut next = end;
        loop {
            while next < len && self.s[next] != '"' && self.s[next] != '\\' {
                if u32::from(self.s[next]) <= 0x1f {
                    return Err(ScanError::Msg("Invalid control character at", next));
                }
                next += 1;
            }
            if next >= len {
                return Err(ScanError::Msg("Unterminated string starting at", begin));
            }
            if self.s[next] == '"' {
                return Ok(next + 1);
            }
            next += 1;
            let Some(c) = self.at(next) else {
                return Err(ScanError::Msg("Unterminated string starting at", begin));
            };
            if c != 'u' {
                if !matches!(c, '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') {
                    return Err(ScanError::Msg("Invalid \\escape", next - 1));
                }
                next += 1;
                continue;
            }
            next += 1;
            let hex_end = next + 4;
            if hex_end >= len {
                return Err(ScanError::Msg("Invalid \\uXXXX escape", next - 1));
            }
            let code = self
                .hex4(next)
                .ok_or(ScanError::Msg("Invalid \\uXXXX escape", hex_end - 5))?;
            next = hex_end;
            if (0xd800..=0xdbff).contains(&code)
                && hex_end + 6 < len
                && self.s[hex_end] == '\\'
                && self.s[hex_end + 1] == 'u'
            {
                let pair_end = hex_end + 6;
                self.hex4(hex_end + 2)
                    .ok_or(ScanError::Msg("Invalid \\uXXXX escape", pair_end - 5))?;
                next = pair_end;
            }
        }
    }

    fn hex4(&self, idx: usize) -> Option<u32> {
        (idx..idx + 4).try_fold(0u32, |acc, i| Some(acc * 16 + self.at(i)?.to_digit(16)?))
    }

    fn scan_object(&self, start: usize) -> Result<usize, ScanError> {
        let mut idx = self.skip_ws(start);
        if self.at(idx) == Some('}') {
            return Ok(idx + 1);
        }
        loop {
            if self.at(idx) != Some('"') {
                return Err(ScanError::Msg(
                    "Expecting property name enclosed in double quotes",
                    idx,
                ));
            }
            idx = self.skip_ws(self.scan_string(idx + 1)?);
            if self.at(idx) != Some(':') {
                return Err(ScanError::Msg("Expecting ':' delimiter", idx));
            }
            idx = self.skip_ws(idx + 1);
            idx = self.skip_ws(self.scan_once(idx)?);
            match self.at(idx) {
                Some('}') => return Ok(idx + 1),
                Some(',') => {}
                _ => return Err(ScanError::Msg("Expecting ',' delimiter", idx)),
            }
            let comma = idx;
            idx = self.skip_ws(idx + 1);
            if self.at(idx) == Some('}') {
                return Err(ScanError::Msg(
                    "Illegal trailing comma before end of object",
                    comma,
                ));
            }
        }
    }

    fn scan_array(&self, start: usize) -> Result<usize, ScanError> {
        let mut idx = self.skip_ws(start);
        if self.at(idx) == Some(']') {
            return Ok(idx + 1);
        }
        loop {
            idx = self.skip_ws(self.scan_once(idx)?);
            match self.at(idx) {
                Some(']') => return Ok(idx + 1),
                Some(',') => {}
                _ => return Err(ScanError::Msg("Expecting ',' delimiter", idx)),
            }
            let comma = idx;
            idx = self.skip_ws(idx + 1);
            if self.at(idx) == Some(']') {
                return Err(ScanError::Msg(
                    "Illegal trailing comma before end of array",
                    comma,
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::TmuxEnv;

    // Fixtures and expected bytes generated with the Python migrator (python3.14,
    // `migrate_sessions(dir, FakeTmux())` / `migrate_artifacts(dir)`, `has_session` → False).
    const SESSION_FIXTURES: &[(&str, &str)] = &[
        (
            "v3llm.json",
            r#"{"schema_version": 3, "kind": "process", "id": "v3llm", "name": "w", "role": "llm", "state": "working", "cwd": "/r", "cmd": "claude", "tags": ["t"], "env": {"K": "v"}, "parent": "p", "pid": 1, "attached_to": [], "created_at": 1.0, "ended_at": null, "engine": "codex", "chats": [{"id": "c1", "role": "original", "cwd": "/r", "transcript_path": "/t", "origin": {"how": "spawn", "session_id": "v3llm", "chat_id": null}, "bundle_path": null, "started_at": 1.5, "ended_at": null, "summary": "s", "engine": "codex"}], "last_activity": 2}"#,
        ),
        (
            "v3nv.json",
            r#"{"schema_version": 3.0, "kind": "process", "id": "v3nv", "name": "ed", "role": "nvim", "state": "exited", "cwd": "/repo", "cmd": "nvim", "tags": [], "env": {}, "parent": null, "pid": null, "attached_to": [{"host": "Views", "window_index": "1", "window_name": "work", "pane_id": "%41", "pane_index": "1"}], "created_at": 950.0, "ended_at": 960, "engine": null, "chats": [], "last_activity": null, "extra": 1}"#,
        ),
        (
            "view.json",
            r#"{"schema_version": 3, "kind": "view", "id": "view", "name": "Views", "role": "shell"}"#,
        ),
        (
            "viewnoname.json",
            r#"{"schema_version": 4, "kind": "view"}"#,
        ),
        (
            "v4.json",
            r#"{"schema_version": 4, "id": "v4", "name": "ed4", "role": "shell", "state": "alive", "cwd": "/", "cmd": "zsh", "tags": [], "env": {}, "parent": null, "pid": 3, "attached_to": [], "created_at": 10, "ended_at": null, "group": "g"}"#,
        ),
        (
            "v5.json",
            r#"{"schema_version": 5, "id": "v5", "name": "w5", "role": "llm", "state": "exited", "cwd": "/r", "cmd": "claude", "tags": ["t"], "env": {}, "parent": null, "pid": 2, "attached_to": [], "created_at": 20.0, "ended_at": 21.0, "engine": "claude", "last_activity": 20.5, "chats": [], "turn_started_at": 7}"#,
        ),
        ("true.json", r#"{"schema_version": true}"#),
        ("listv.json", r#"{"schema_version": [3]}"#),
        ("strv.json", r#"{"schema_version": "3"}"#),
        ("str.json", r#""x""#),
        ("int.json", r#"5"#),
        ("null.json", r#"null"#),
        ("float.json", r#"1.5"#),
        (
            "badeng.json",
            r#"{"schema_version": 3, "id": "b", "name": "b", "role": "llm", "state": "idle", "cwd": "/", "cmd": "c", "tags": [], "env": {}, "parent": null, "pid": null, "attached_to": [], "created_at": null, "ended_at": null, "engine": null, "chats": []}"#,
        ),
        (
            "badrole.json",
            r#"{"schema_version": 3, "id": "b", "name": "b", "role": "robot", "state": "idle", "cwd": "/", "cmd": "c", "tags": [], "env": {}, "parent": null, "pid": null, "attached_to": [], "created_at": null, "ended_at": null}"#,
        ),
        (".hidden.json", r#"{"schema_version": 6}"#),
        ("bad.json", "{not json"),
        ("trail.json", r#"{"a": 1,}"#),
    ];
    const ARTIFACT_FIXTURES: &[(&str, &str)] = &[
        (
            "a1.json",
            r#"{"artifact_schema_version": 1, "id": "a1", "title": "T", "filename": "t.md", "created_at": 1.0, "history": [{"session_id": "s", "at": 1.0, "rev": 0, "changes": null}]}"#,
        ),
        (
            "a2.json",
            r#"{"artifact_schema_version": 1.0, "id": "a2", "group": "g", "title": null, "filename": "t.md", "created_at": 1, "history": [{"session_id": "s", "at": 1, "rev": 0, "changes": null}, {"session_id": "u", "at": 2.5, "rev": 1, "changes": "c"}]}"#,
        ),
        ("a3.json", r#"{"artifact_schema_version": 3}"#),
        ("a4.json", r#"[1]"#),
        (
            "a5.json",
            r#"{"artifact_schema_version": 1, "id": "a5", "title": "T", "filename": "t.md", "created_at": 1.0, "history": [{"session_id": "s", "at": 1.0, "rev": 1, "changes": null}]}"#,
        ),
    ];
    const EXPECTED_V3LLM: &str = r#"{
  "schema_version": 6,
  "id": "v3llm",
  "name": "w",
  "role": "llm",
  "state": "working",
  "cwd": "/r",
  "cmd": "claude",
  "tags": [
    "t"
  ],
  "group": null,
  "env": {
    "K": "v"
  },
  "parent": "p",
  "pid": 1,
  "attached_to": [],
  "created_at": 1.0,
  "ended_at": null,
  "engine": "codex",
  "last_activity": 2,
  "chats": [
    {
      "id": "c1",
      "role": "original",
      "cwd": "/r",
      "transcript_path": "/t",
      "origin": {
        "how": "spawn",
        "session_id": "v3llm",
        "chat_id": null
      },
      "bundle_path": null,
      "started_at": 1.5,
      "ended_at": null,
      "summary": "s",
      "engine": "codex"
    }
  ],
  "turn_started_at": null
}"#;
    const EXPECTED_V3NV: &str = r#"{
  "schema_version": 6,
  "id": "v3nv",
  "name": "ed",
  "role": "nvim",
  "state": "exited",
  "cwd": "/repo",
  "cmd": "nvim",
  "tags": [],
  "group": null,
  "env": {},
  "parent": null,
  "pid": null,
  "attached_to": [
    {
      "host": "Views",
      "window_index": "1",
      "window_name": "work",
      "pane_id": "%41",
      "pane_index": "1"
    }
  ],
  "created_at": 950.0,
  "ended_at": 960,
  "artifact_id": null
}"#;
    const EXPECTED_V4: &str = r#"{
  "schema_version": 6,
  "id": "v4",
  "name": "ed4",
  "role": "shell",
  "state": "alive",
  "cwd": "/",
  "cmd": "zsh",
  "tags": [],
  "group": "g",
  "env": {},
  "parent": null,
  "pid": 3,
  "attached_to": [],
  "created_at": 10,
  "ended_at": null,
  "artifact_id": null
}"#;
    const EXPECTED_V5: &str = r#"{
  "schema_version": 6,
  "id": "v5",
  "name": "w5",
  "role": "llm",
  "state": "exited",
  "cwd": "/r",
  "cmd": "claude",
  "tags": [
    "t"
  ],
  "group": null,
  "env": {},
  "parent": null,
  "pid": 2,
  "attached_to": [],
  "created_at": 20.0,
  "ended_at": 21.0,
  "engine": "claude",
  "last_activity": 20.5,
  "chats": [],
  "turn_started_at": 7
}"#;
    const EXPECTED_A1: &str = r#"{
  "artifact_schema_version": 2,
  "id": "a1",
  "title": "T",
  "filename": "t.md",
  "created_at": 1.0,
  "group": null,
  "history": [
    {
      "session_id": "s",
      "at": 1.0,
      "rev": 0,
      "changes": null
    }
  ]
}"#;
    const EXPECTED_A2: &str = r#"{
  "artifact_schema_version": 2,
  "id": "a2",
  "title": null,
  "filename": "t.md",
  "created_at": 1,
  "group": "g",
  "history": [
    {
      "session_id": "s",
      "at": 1,
      "rev": 0,
      "changes": null
    },
    {
      "session_id": "u",
      "at": 2.5,
      "rev": 1,
      "changes": "c"
    }
  ]
}"#;

    fn no_server() -> Tmux {
        Tmux::new("/nonexistent/tmux", TmuxEnv::default())
    }

    fn skipped(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, reason)| ((*name).to_owned(), (*reason).to_owned()))
            .collect()
    }

    #[test]
    fn sessions_match_the_python_migrator() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("s");
        std::fs::create_dir(&sessions).unwrap();
        for (name, content) in SESSION_FIXTURES {
            std::fs::write(sessions.join(name), content).unwrap();
        }
        std::fs::write(sessions.join("utf.json"), b"{\"a\": \"\xff\"}").unwrap();
        std::fs::create_dir(sessions.join("dir.json")).unwrap();

        let report = migrate_sessions(&sessions, &no_server());

        assert_eq!(
            report.migrated,
            ["v3llm.json", "v3nv.json", "v4.json", "v5.json"]
        );
        assert_eq!(report.views_removed, ["Views"]);
        let dir_reason = format!(
            "IsADirectoryError: [Errno 21] Is a directory: '{}'",
            sessions.join("dir.json").display()
        );
        let expected = skipped(&[
            (".hidden.json", "already v6"),
            (
                "bad.json",
                "JSONDecodeError: Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
            ("badeng.json", "ValueError: None is not a valid Engine"),
            ("badrole.json", "ValueError: 'robot' is not a valid Role"),
            ("dir.json", &dir_reason),
            (
                "float.json",
                "AttributeError: 'float' object has no attribute 'get'",
            ),
            (
                "int.json",
                "AttributeError: 'int' object has no attribute 'get'",
            ),
            (
                "listv.json",
                "TypeError: cannot use 'list' as a set element (unhashable type: 'list')",
            ),
            (
                "null.json",
                "AttributeError: 'NoneType' object has no attribute 'get'",
            ),
            (
                "str.json",
                "AttributeError: 'str' object has no attribute 'get'",
            ),
            ("strv.json", "not an upgradable record (schema_version='3')"),
            (
                "trail.json",
                "JSONDecodeError: Illegal trailing comma before end of object: line 1 column 8 (char 7)",
            ),
            (
                "true.json",
                "not an upgradable record (schema_version=True)",
            ),
            (
                "utf.json",
                "UnicodeDecodeError: 'utf-8' codec can't decode byte 0xff in position 7: invalid start byte",
            ),
            ("viewnoname.json", "KeyError: 'name'"),
        ]);
        assert_eq!(report.skipped, expected);

        let read = |name: &str| std::fs::read_to_string(sessions.join(name)).unwrap();
        assert_eq!(read("v3llm.json"), EXPECTED_V3LLM);
        assert_eq!(read("v3nv.json"), EXPECTED_V3NV);
        assert_eq!(read("v4.json"), EXPECTED_V4);
        assert_eq!(read("v5.json"), EXPECTED_V5);
        assert!(!sessions.join("view.json").exists());
        // Untouched files keep their bytes.
        assert_eq!(read("bad.json"), "{not json");

        // Idempotent: a second run migrates nothing.
        let again = migrate_sessions(&sessions, &no_server());
        assert!(again.migrated.is_empty() && again.views_removed.is_empty());
        assert!(
            again
                .skipped
                .contains(&("v3llm.json".into(), "already v6".into()))
        );
        assert_eq!(read("v3llm.json"), EXPECTED_V3LLM);
    }

    #[test]
    fn non_string_view_name_is_skipped() {
        // The reference hands the int to subprocess, which raises TypeError before any tmux call.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.json");
        std::fs::write(&path, r#"{"schema_version": 5, "kind": "view", "name": 5}"#).unwrap();
        let report = migrate_sessions(dir.path(), &no_server());
        assert_eq!(
            report.skipped,
            skipped(&[(
                "v.json",
                "TypeError: expected str, bytes or os.PathLike object, not int"
            )])
        );
        assert!(path.exists());
    }

    #[test]
    fn artifacts_match_the_python_migrator() {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in ARTIFACT_FIXTURES {
            std::fs::write(dir.path().join(name), content).unwrap();
        }
        let report = migrate_artifacts(dir.path());
        assert_eq!(report.migrated, ["a1.json", "a2.json"]);
        assert_eq!(
            report.skipped,
            skipped(&[
                (
                    "a3.json",
                    "not an upgradable artifact record (artifact_schema_version=3)"
                ),
                (
                    "a4.json",
                    "AttributeError: 'list' object has no attribute 'get'"
                ),
                (
                    "a5.json",
                    "UnsupportedArtifactError: artifact 'a5' has non-contiguous rev numbers [1] \
                     (expected 0..0, one per touch)"
                ),
            ])
        );
        let read = |name: &str| std::fs::read_to_string(dir.path().join(name)).unwrap();
        assert_eq!(read("a1.json"), EXPECTED_A1);
        assert_eq!(read("a2.json"), EXPECTED_A2);
        assert!(migrate_artifacts(dir.path()).migrated.is_empty());
    }

    #[test]
    fn json_errors_match_cpython() {
        // python3.14: str(json.JSONDecodeError) for json.loads(case); None = accepted.
        let cases: &[(&str, Option<&str>)] = &[
            (
                "{not json",
                Some("Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
            ),
            ("", Some("Expecting value: line 1 column 1 (char 0)")),
            ("  ", Some("Expecting value: line 1 column 3 (char 2)")),
            (
                "[1,]",
                Some("Illegal trailing comma before end of array: line 1 column 3 (char 2)"),
            ),
            (
                "{\"a\":1,}",
                Some("Illegal trailing comma before end of object: line 1 column 7 (char 6)"),
            ),
            (
                "{\"a\" 1}",
                Some("Expecting ':' delimiter: line 1 column 6 (char 5)"),
            ),
            (
                "{\"a\":1 \"b\"}",
                Some("Expecting ',' delimiter: line 1 column 8 (char 7)"),
            ),
            (
                "[1 2]",
                Some("Expecting ',' delimiter: line 1 column 4 (char 3)"),
            ),
            (
                "\"abc",
                Some("Unterminated string starting at: line 1 column 1 (char 0)"),
            ),
            (
                "\"a\\qb\"",
                Some("Invalid \\escape: line 1 column 3 (char 2)"),
            ),
            (
                "\"\\u12\"",
                Some("Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
            ),
            (
                "\"\\u12zz\"",
                Some("Invalid \\uXXXX escape: line 1 column 3 (char 2)"),
            ),
            (
                "\"\\ud800\\u12zz\"",
                Some("Invalid \\uXXXX escape: line 1 column 9 (char 8)"),
            ),
            (
                "\"a\x01\"",
                Some("Invalid control character at: line 1 column 3 (char 2)"),
            ),
            ("{\"a\":1}x", Some("Extra data: line 1 column 8 (char 7)")),
            (
                "\n\n  [1,\n  nul]",
                Some("Expecting value: line 4 column 3 (char 10)"),
            ),
            ("-", Some("Expecting value: line 1 column 1 (char 0)")),
            ("-x", Some("Expecting value: line 1 column 1 (char 0)")),
            ("01", Some("Extra data: line 1 column 2 (char 1)")),
            ("1.", Some("Extra data: line 1 column 2 (char 1)")),
            ("1e", Some("Extra data: line 1 column 2 (char 1)")),
            (
                "[\"\\",
                Some("Unterminated string starting at: line 1 column 2 (char 1)"),
            ),
            (
                "\u{feff}{}",
                Some("Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)"),
            ),
            (
                "{\"é\":tru}",
                Some("Expecting value: line 1 column 6 (char 5)"),
            ),
            (
                "[1,2",
                Some("Expecting ',' delimiter: line 1 column 5 (char 4)"),
            ),
            (
                "{",
                Some("Expecting property name enclosed in double quotes: line 1 column 2 (char 1)"),
            ),
            ("[", Some("Expecting value: line 1 column 2 (char 1)")),
            (
                "{\"a\"",
                Some("Expecting ':' delimiter: line 1 column 5 (char 4)"),
            ),
            ("{\"a\":", Some("Expecting value: line 1 column 6 (char 5)")),
            ("NaN", None),
            ("-Infinity", None),
            ("[1e5, 2.5E-3, -0]", None),
            ("\"\\ud800\\udc00\"", None),
        ];
        for (text, expected) in cases {
            assert_eq!(py_json_check(text).err().as_deref(), *expected, "{text:?}");
        }
    }
}
