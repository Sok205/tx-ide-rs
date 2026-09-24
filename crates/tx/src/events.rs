//! Provenance log (events.py): `$TX_IDE_HOME/log.jsonl`, one compact JSON line per mutation,
//! `{"ts":<float>,"actor":…,"type":…,"msg":…}`, appended with one write on an `O_APPEND` fd so
//! concurrent writers never tear a line (`PIPE_BUF`).
//!
//! The caller resolves `actor` (the reference defaults it to `$TX_SESSION_ID`, else `""`).
//!
//! T-EVENTS-05 FIX: the reference trims `msg` to a byte budget of its UTF-8 form, but the line
//! carries the `\uXXXX`-escaped form, so a non-ASCII (or `"`/`\`-heavy) message still overflows
//! 512 bytes. Here `msg` keeps the longest prefix whose escaped form fits; for plain ASCII that is
//! exactly the reference's `msg[:budget]`.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Number, Value, json};

use crate::pyjson;

pub const ACTOR_ENV: &str = "TX_SESSION_ID";
/// macOS `PIPE_BUF`; the newline counts toward it.
pub const PIPE_BUF: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    /// `path` is `Home::log_path()` unless a caller needs another log.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one line stamped with the current time.
    pub fn append(&self, type_: &str, msg: &str, actor: &str) -> io::Result<()> {
        self.append_at(now(), type_, msg, actor)
    }

    pub fn append_at(&self, ts: f64, type_: &str, msg: &str, actor: &str) -> io::Result<()> {
        let line = encode(ts, actor, type_, msg);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .append(true)
            .create(true)
            .mode(0o644)
            .open(&self.path)?;
        file.write_all(&line)
    }
}

/// `time.time()`: nanoseconds since the epoch divided by 1e9, as CPython computes it.
pub fn now() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    nanos as f64 / 1e9
}

fn record(ts: f64, actor: &str, type_: &str, msg: &str) -> Value {
    let ts = Number::from_f64(ts).map_or(Value::Null, Value::Number);
    json!({"ts": ts, "actor": actor, "type": type_, "msg": msg})
}

fn line(ts: f64, actor: &str, type_: &str, msg: &str) -> String {
    let mut line = pyjson::dumps_compact(&record(ts, actor, type_, msg));
    line.push('\n');
    line
}

/// `_encode`: compact JSON + newline, `msg` trimmed so the line fits in `PIPE_BUF`.
pub fn encode(ts: f64, actor: &str, type_: &str, msg: &str) -> Vec<u8> {
    let full = line(ts, actor, type_, msg);
    if full.len() <= PIPE_BUF {
        return full.into_bytes();
    }
    let skeleton = line(ts, actor, type_, "").len();
    let mut budget = PIPE_BUF.saturating_sub(skeleton);
    let mut end = 0;
    for (index, c) in msg.char_indices() {
        let escaped = escaped_len(c);
        if escaped > budget {
            break;
        }
        budget -= escaped;
        end = index + c.len_utf8();
    }
    line(ts, actor, type_, &msg[..end]).into_bytes()
}

/// Bytes `c` takes inside a Python (`ensure_ascii`) JSON string.
fn escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        ' '..='~' => 1,
        c => 6 * c.len_utf16(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected lines from CPython 3.14: `EventLog()._encode({"ts": ts, ...})`.
    #[test]
    fn short_line_shape() {
        let line = encode(1727000000.123456, "sess-1", "rm", "wörker→ (r2)");
        assert_eq!(
            String::from_utf8(line).unwrap(),
            "{\"ts\":1727000000.123456,\"actor\":\"sess-1\",\"type\":\"rm\",\"msg\":\"w\\u00f6rker\\u2192 (r2)\"}\n"
        );
    }

    #[test]
    fn ascii_truncation_matches_reference() {
        let msg = format!("{} (r2)", "y".repeat(1000));
        let line = encode(1727000000.1234567, "sess-1", "rm", &msg);
        assert_eq!(line.len(), 512);
        let skeleton =
            "{\"ts\":1727000000.1234567,\"actor\":\"sess-1\",\"type\":\"rm\",\"msg\":\"\"}\n";
        let budget = 512 - skeleton.len();
        let want = format!(
            "{{\"ts\":1727000000.1234567,\"actor\":\"sess-1\",\"type\":\"rm\",\"msg\":\"{}\"}}\n",
            "y".repeat(budget)
        );
        assert_eq!(String::from_utf8(line).unwrap(), want);
    }

    #[test]
    fn exact_512_is_kept_513_loses_one_char() {
        let skeleton = line(0.0, "sess-1", "rm", "").len();
        let fits = "y".repeat(512 - skeleton);
        assert_eq!(
            encode(0.0, "sess-1", "rm", &fits),
            line(0.0, "sess-1", "rm", &fits).into_bytes()
        );
        let over = "y".repeat(513 - skeleton);
        let trimmed = encode(0.0, "sess-1", "rm", &over);
        assert_eq!(trimmed.len(), 512);
        assert_eq!(trimmed, line(0.0, "sess-1", "rm", &fits).into_bytes());
    }

    #[test]
    fn non_ascii_truncation_stays_within_pipe_buf() {
        let msg = format!("{} (r3)", "é".repeat(600));
        let line = encode(1727000000.1234567, "me", "rm", &msg);
        assert!(line.len() <= 512);
        let value: Value = serde_json::from_slice(&line).unwrap();
        let kept = value["msg"].as_str().unwrap();
        assert!(
            msg.starts_with(kept) && kept.chars().count() == 75,
            "{kept}"
        );
        let quotes = encode(0.0, "a", "t", &"\"".repeat(600));
        assert!(quotes.len() <= 512 && quotes.len() >= 511);
        let astral = encode(0.0, "a", "t", &"😀".repeat(100));
        assert!(astral.len() <= 512);
    }

    #[test]
    fn append_writes_lines_with_mode_0644() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::new(dir.path().join("sub/log.jsonl"));
        log.append_at(1.0, "rm", "a", "").unwrap();
        log.append("tag", "b", "s").unwrap();
        let text = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[0], r#"{"ts":1.0,"actor":"","type":"rm","msg":"a"}"#);
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert!(second["ts"].is_f64());
        let mode = std::fs::metadata(log.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & !0o644, 0, "{mode:o}");
    }
}
