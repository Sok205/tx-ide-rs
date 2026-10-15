//! The operation log: `<state_dir>/log.jsonl`, one compact JSON line per mutation,
//! `{"ts":<float>,"actor":"…","type":"…","msg":"…"}`, appended with a single write on an
//! `O_APPEND` descriptor so concurrent writers never tear a line (the line fits `PIPE_BUF`).

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Number, Value, json};

use crate::session::SessionId;

/// The smallest `PIPE_BUF` among supported platforms (macOS); a line at most this long is
/// appended in one atomic write. The newline counts.
pub const PIPE_BUF: usize = 512;

/// Every event type core logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EventType {
    Created,
    Renamed,
    Tagged,
    MessageSent,
    Exited,
    Stopped,
    Forgotten,
}

impl EventType {
    pub const ALL: &'static [EventType] = &[
        EventType::Created,
        EventType::Renamed,
        EventType::Tagged,
        EventType::MessageSent,
        EventType::Exited,
        EventType::Stopped,
        EventType::Forgotten,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            EventType::Created => "created",
            EventType::Renamed => "renamed",
            EventType::Tagged => "tagged",
            EventType::MessageSent => "message-sent",
            EventType::Exited => "exited",
            EventType::Stopped => "stopped",
            EventType::Forgotten => "forgotten",
        }
    }
}

/// One thing that happened to one session. Handed to in-process sinks and written to the log.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct Event {
    pub ts: f64,
    pub type_: EventType,
    pub id: SessionId,
    pub name: String,
    /// The sending session for `message-sent`; `None` means the user or core itself.
    pub actor: Option<SessionId>,
    /// Human-readable detail (the old name on a rename, the tag list, …). Not a contract.
    pub detail: String,
}

impl Event {
    pub fn new(
        type_: EventType,
        id: SessionId,
        name: impl Into<String>,
        actor: Option<SessionId>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            ts: now(),
            type_,
            id,
            name: name.into(),
            actor,
            detail: detail.into(),
        }
    }
}

/// An in-process listener. Called after the operation's record is saved and every lock is
/// released, in registration order; cannot veto; a panic is contained. Hand slow work to your
/// own thread.
pub trait EventSink: Send + Sync {
    fn on_event(&self, event: &Event);
}

impl<F: Fn(&Event) + Send + Sync> EventSink for F {
    fn on_event(&self, event: &Event) {
        self(event)
    }
}

impl<T: EventSink + ?Sized> EventSink for std::sync::Arc<T> {
    fn on_event(&self, event: &Event) {
        (**self).on_event(event)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventLog {
    path: PathBuf,
}

impl EventLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event as one line.
    pub fn append(&self, event: &Event) -> io::Result<()> {
        self.append_at(
            event.ts,
            event.type_.as_str(),
            event.id.as_str(),
            &event.name,
            &event.detail,
            event.actor.as_ref().map_or("", SessionId::as_str),
        )
    }

    pub fn append_at(
        &self,
        ts: f64,
        type_: &str,
        id: &str,
        name: &str,
        msg: &str,
        actor: &str,
    ) -> io::Result<()> {
        let line = encode(ts, actor, type_, id, name, msg);
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

/// Seconds since the epoch as a float.
pub fn now() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    nanos as f64 / 1e9
}

fn line(ts: f64, actor: &str, type_: &str, id: &str, name: &str, msg: &str) -> String {
    let ts = Number::from_f64(ts).map_or(Value::Null, Value::Number);
    let mut line = json!({
        "ts": ts, "actor": actor, "type": type_, "id": id, "name": name, "msg": msg
    })
    .to_string();
    line.push('\n');
    line
}

/// Compact JSON + newline, `msg` trimmed on a char boundary so the line fits `PIPE_BUF`.
pub fn encode(ts: f64, actor: &str, type_: &str, id: &str, name: &str, msg: &str) -> Vec<u8> {
    let full = line(ts, actor, type_, id, name, msg);
    if full.len() <= PIPE_BUF {
        return full.into_bytes();
    }
    let skeleton = line(ts, actor, type_, id, name, "").len();
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
    line(ts, actor, type_, id, name, &msg[..end]).into_bytes()
}

/// Bytes `c` occupies inside a serde_json string: short escapes are 2 bytes, other C0 controls
/// are `\u00XX`, everything else is its UTF-8 length.
fn escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        '\0'..='\u{1f}' => 6,
        c => c.len_utf8(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::os::unix::fs::PermissionsExt;

    fn parse(line: &[u8]) -> Value {
        serde_json::from_slice(line).unwrap()
    }

    #[test]
    fn short_line_shape() {
        let line = encode(1727000000.123456, "sess-1", "created", "id-9", "w1", "wörker→ (r2)");
        assert_eq!(
            String::from_utf8(line).unwrap(),
            "{\"ts\":1727000000.123456,\"actor\":\"sess-1\",\"type\":\"created\",\"id\":\"id-9\",\"name\":\"w1\",\"msg\":\"wörker→ (r2)\"}\n"
        );
    }

    #[test]
    fn ascii_overflow_is_trimmed_to_exactly_pipe_buf() {
        let msg = format!("{} (r2)", "y".repeat(1000));
        let line = encode(1727000000.1234567, "sess-1", "renamed", "i", "n", &msg);
        assert_eq!(line.len(), PIPE_BUF);
        let value = parse(&line);
        assert!(msg.starts_with(value["msg"].as_str().unwrap()));
    }

    #[test]
    fn exact_pipe_buf_is_kept_and_one_over_loses_one_char() {
        let skeleton = encode(0.0, "sess-1", "t", "i", "n", "").len();
        let fits = "y".repeat(PIPE_BUF - skeleton);
        let line = encode(0.0, "sess-1", "t", "i", "n", &fits);
        assert_eq!(line.len(), PIPE_BUF);
        assert_eq!(parse(&line)["msg"], fits);
        let over = "y".repeat(PIPE_BUF + 1 - skeleton);
        let trimmed = encode(0.0, "sess-1", "t", "i", "n", &over);
        assert_eq!(trimmed, line);
    }

    #[test]
    fn multibyte_and_escaped_chars_stay_within_pipe_buf() {
        for msg in [
            "é".repeat(600),
            "😀".repeat(200),
            "\"".repeat(600),
            "\\".repeat(600),
            "\n".repeat(600),
            "\u{1}".repeat(600),
            "\u{7f}".repeat(600),
            "a\u{1}é😀\"".repeat(100),
        ] {
            let line = encode(0.0, "a", "t", "i", "n", &msg);
            assert!(line.len() <= PIPE_BUF, "{}", line.len());
            assert!(line.len() > PIPE_BUF - 8, "{} too short for {msg:?}", line.len());
            let kept = parse(&line);
            assert!(msg.starts_with(kept["msg"].as_str().unwrap()));
        }
    }

    #[test]
    fn control_char_counts_six_bytes() {
        let skeleton = encode(0.0, "", "t", "i", "n", "").len();
        let budget = PIPE_BUF - skeleton;
        let msg = "\u{1}".repeat(budget / 6 + 1);
        let line = encode(0.0, "", "t", "i", "n", &msg);
        assert_eq!(parse(&line)["msg"].as_str().unwrap().chars().count(), budget / 6);
    }

    #[test]
    fn non_finite_timestamp_is_null() {
        assert_eq!(parse(&encode(f64::NAN, "", "t", "i", "n", "m"))["ts"], Value::Null);
        assert_eq!(parse(&encode(f64::INFINITY, "", "t", "i", "n", "m"))["ts"], Value::Null);
    }

    #[test]
    fn append_creates_parent_dir_with_mode_0644() {
        let dir = tempfile::tempdir().unwrap();
        let log = EventLog::new(dir.path().join("sub/log.jsonl"));
        log.append_at(1.0, "created", "i", "n", "a", "").unwrap();
        let event = Event::new(
            EventType::Tagged,
            "i".parse().unwrap(),
            "n",
            Some("s".parse().unwrap()),
            "b",
        );
        log.append(&event).unwrap();
        let text = std::fs::read_to_string(log.path()).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[0], r#"{"ts":1.0,"actor":"","type":"created","id":"i","name":"n","msg":"a"}"#);
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["type"], "tagged");
        assert_eq!(second["id"], "i");
        assert_eq!(second["actor"], "s");
        assert_eq!(second["msg"], "b");
        assert!(second["ts"].as_f64().unwrap() > 1.7e9);
        let mode = std::fs::metadata(log.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & !0o644, 0, "{mode:o}");
    }

    #[test]
    fn concurrent_appends_never_tear() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.jsonl");
        let handles: Vec<_> = (0..4)
            .map(|worker| {
                let log = EventLog::new(&path);
                std::thread::spawn(move || {
                    let msg = format!("{worker}").repeat(400);
                    for _ in 0..200 {
                        log.append_at(0.0, "t", "i", "n", &msg, "").unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 800);
        for line in lines {
            let value: Value = serde_json::from_str(line).unwrap();
            let msg = value["msg"].as_str().unwrap();
            assert!(msg.chars().all(|c| c == msg.chars().next().unwrap()), "torn: {msg}");
        }
    }

    #[test]
    fn event_type_names() {
        assert_eq!(EventType::MessageSent.as_str(), "message-sent");
        let names: Vec<&str> = EventType::ALL.iter().map(|t| t.as_str()).collect();
        assert_eq!(
            names,
            ["created", "renamed", "tagged", "message-sent", "exited", "stopped", "forgotten"]
        );
    }

    #[test]
    fn now_is_recent_epoch_seconds() {
        let now = now();
        assert!(now > 1.7e9 && now < 4e9, "{now}");
    }
}
