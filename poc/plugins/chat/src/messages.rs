//! The inter-agent + user message stream, reconstructed from chat transcripts (messages.py).
//!
//! A message is typed into the recipient's input, so the engine records it verbatim as a `user`
//! turn. Classification keys on the leading marker of a genuine typed turn:
//! `<from-agent session="X">…</from-agent>` (peer; the legacy `<from-claude>` still parses),
//! `<from-user session="X">…</from-user>` (the operator, typed in session X),
//! `<tx-command-prompt …/> text` (prefix+/), the viewer composer template, or plain typed text.
//! [`parse_message`] is the pure per-turn classifier; [`collect_messages`] and
//! [`source_signature`] are the only functions touching the filesystem.
//!
//! The reference's regexes are hand-matched here (same anchoring, greediness and `\s` set).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use tx_core::{Launch, Lifecycle, Session};

use crate::engines::BUNDLE_TRANSCRIPT_NAME;
use crate::ext::history_ext;

/// What a peer sender is, as the classifier needs it: a send from a non-agent home is yours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SenderRole {
    Agent,
    Shell,
}

pub const SENDER_YOU: &str = "you";
/// The peer channel — `tx send-message`.
pub const TAG_AGENT: &str = "from-agent";
/// The operator channel — `tx send-user-message`.
pub const TAG_USER: &str = "from-user";

/// Harness-injected user turns that nobody sent.
const HARNESS_PREFIXES: [&str; 4] = [
    "<task-notification",
    "<local-command",
    "<command-",
    "[Request interrupted",
];

/// Build a message envelope. `tag` is [`TAG_AGENT`] (peer) or [`TAG_USER`] (operator); one
/// builder for both channels so their wire shape cannot drift.
pub fn build_envelope(sender: &str, body: &str, tag: &str) -> String {
    format!("<{tag} session=\"{sender}\">{body}</{tag}>")
}

/// Who sent a message: an llm session (the inter-agent channel) or you.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Agent,
    You,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::You => "you",
        }
    }
}

/// How a message was captured (provenance).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Via {
    SendMessage,
    SendUserMessage,
    Prompt,
    Composer,
    Typed,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::SendMessage => "send-message",
            Via::SendUserMessage => "send-user-message",
            Via::Prompt => "prompt",
            Via::Composer => "composer",
            Via::Typed => "typed",
        }
    }
}

/// One reconstructed message.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    /// Epoch seconds — the sort key.
    pub ts: f64,
    /// The transcript timestamp, verbatim.
    pub iso: String,
    pub kind: Kind,
    pub via: Via,
    /// Peer sender name, or [`SENDER_YOU`].
    pub sender: String,
    /// Recipient display name (its id when the record is gone).
    pub recipient: String,
    /// The tx session id owning the transcript.
    pub recipient_id: String,
    pub body: String,
    pub chat_id: String,
    /// The transcript line uuid — the dedup key.
    pub uuid: String,
    /// The session the operator typed in (`<from-user>` only; empty everywhere else).
    pub origin: String,
}

impl Message {
    /// `dataclasses.asdict` in field order.
    pub fn to_value(&self) -> Value {
        json!({
            "ts": self.ts,
            "iso": self.iso,
            "kind": self.kind.as_str(),
            "via": self.via.as_str(),
            "sender": self.sender,
            "recipient": self.recipient,
            "recipient_id": self.recipient_id,
            "body": self.body,
            "chat_id": self.chat_id,
            "uuid": self.uuid,
            "origin": self.origin,
        })
    }
}

/// The session owning a transcript, as `parse_message` needs it.
#[derive(Clone, Copy, Debug)]
pub struct Recipient<'a> {
    pub id: &'a str,
    pub name: &'a str,
    /// The launch command: a plain turn embedded in it is the spawn prompt, not a message.
    pub cmd: &'a str,
    pub chat_id: &'a str,
}

/// Classify ONE transcript line, or `None` if it is not a message anyone sent. Pure.
/// `sender_role(name)` resolves a peer sender's role (a send from a non-llm home is yours).
pub fn parse_message(
    obj: &Value,
    recipient: &Recipient<'_>,
    sender_role: &dyn Fn(&str) -> Option<SenderRole>,
) -> Option<Message> {
    let obj = obj.as_object()?;
    if obj.get("type").and_then(Value::as_str) != Some("user") {
        return None;
    }
    let message = obj.get("message")?.as_object()?;
    if message.get("role").and_then(Value::as_str) != Some("user") {
        return None;
    }
    if truthy(obj.get("isMeta")) || truthy(obj.get("isSidechain")) {
        return None;
    }
    match obj.get("userType") {
        None | Some(Value::Null) => {}
        Some(user_type) if user_type.as_str() == Some("external") => {}
        Some(_) => return None,
    }
    let text = plain_text(message.get("content"))?;
    let classified = classify(py_lstrip(&text), recipient.cmd, sender_role)?;
    if classified.body.is_empty() {
        return None;
    }
    let (ts, iso) = parse_timestamp(obj.get("timestamp"));
    let uuid = match obj.get("uuid") {
        Some(Value::String(uuid)) if !uuid.is_empty() => uuid.clone(),
        _ => format!(
            "{}:{iso}:{}",
            recipient.chat_id,
            classified.body.chars().count()
        ),
    };
    let recipient_name = if recipient.name.is_empty() {
        recipient.id
    } else {
        recipient.name
    };
    Some(Message {
        ts,
        iso,
        kind: classified.kind,
        via: classified.via,
        sender: classified.sender,
        recipient: recipient_name.to_owned(),
        recipient_id: recipient.id.to_owned(),
        body: classified.body,
        chat_id: recipient.chat_id.to_owned(),
        uuid,
        origin: classified.origin,
    })
}

struct Classified {
    kind: Kind,
    via: Via,
    sender: String,
    body: String,
    origin: String,
}

/// The classification of a left-stripped plain-text turn, or `None` if it is not a message.
fn classify(
    text: &str,
    recipient_cmd: &str,
    sender_role: &dyn Fn(&str) -> Option<SenderRole>,
) -> Option<Classified> {
    let you = |via, body: &str, origin: &str| Classified {
        kind: Kind::You,
        via,
        sender: SENDER_YOU.to_owned(),
        body: py_strip(body).to_owned(),
        origin: origin.to_owned(),
    };
    if let Some((sender, body)) = match_envelope(text, &["from-agent", "from-claude"]) {
        let kind = match sender_role(sender) {
            Some(role) if role != SenderRole::Agent => Kind::You,
            _ => Kind::Agent,
        };
        return Some(Classified {
            kind,
            via: Via::SendMessage,
            sender: sender.to_owned(),
            body: py_strip(body).to_owned(),
            origin: String::new(),
        });
    }
    if let Some((origin, body)) = match_envelope(text, &["from-user"]) {
        return Some(you(Via::SendUserMessage, body, origin));
    }
    if let Some(body) = match_command_prompt(text) {
        return Some(you(Via::Prompt, body, ""));
    }
    if is_composer(text) {
        return Some(you(Via::Composer, text, ""));
    }
    if HARNESS_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
    {
        return None;
    }
    if is_priming(text) {
        return None;
    }
    let stripped = py_strip(text);
    if !stripped.is_empty() && !recipient_cmd.is_empty() && recipient_cmd.contains(stripped) {
        return None;
    }
    Some(you(Via::Typed, stripped, ""))
}

/// `^<(T) session="([^"]*)">(.*)</(T)>\s*\Z` (DOTALL, greedy body) for any open/close tag of
/// `tags`: `(session, body)`.
fn match_envelope<'t>(text: &'t str, tags: &[&str]) -> Option<(&'t str, &'t str)> {
    let rest = tags.iter().find_map(|tag| {
        text.strip_prefix('<')?
            .strip_prefix(tag)?
            .strip_prefix(" session=\"")
    })?;
    let (session, rest) = rest.split_once('"')?;
    let rest = rest.strip_prefix('>')?;
    let trimmed = rest.trim_end_matches(py_isspace);
    let body = tags.iter().find_map(|tag| {
        trimmed
            .strip_suffix('>')?
            .strip_suffix(tag)?
            .strip_suffix("</")
    })?;
    Some((session, body))
}

/// `^<tx-command-prompt\b[^>]*/>\s*(.*)\Z` (DOTALL): the text after the focus envelope.
fn match_command_prompt(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("<tx-command-prompt")?;
    if rest.chars().next().is_some_and(is_word_char) {
        return None;
    }
    let close = rest.find('>')?;
    if !rest[..close].ends_with('/') {
        return None;
    }
    Some(rest[close + 1..].trim_start_matches(py_isspace))
}

/// `^Act on (?:tx session|these \d+ tx sessions)\b`.
fn is_composer(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("Act on ") else {
        return false;
    };
    let tail = if let Some(tail) = rest.strip_prefix("tx session") {
        tail
    } else if let Some(after) = rest.strip_prefix("these ") {
        let digits = after.len() - after.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        if digits == 0 {
            return false;
        }
        match after[digits..].strip_prefix(" tx sessions") {
            Some(tail) => tail,
            None => return false,
        }
    } else {
        return false;
    };
    !tail.chars().next().is_some_and(is_word_char)
}

/// Spawn priming: the role-file read instruction, matched in the first 400 characters.
fn is_priming(text: &str) -> bool {
    let head = match text.char_indices().nth(400) {
        Some((end, _)) => &text[..end],
        None => text,
    };
    (head.starts_with("Read ") && head.contains(".tx-ide/agents/"))
        || head.contains("as your first actions")
}

/// The typed text of a user turn: a string, or an all-`text` block list joined by newlines.
/// Anything else (a `tool_result`, an image, a non-string `text`) is harness machinery.
fn plain_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(text) => Some(text.clone()),
        Value::Array(items) => {
            let blocks: Vec<&Map<String, Value>> =
                items.iter().filter_map(Value::as_object).collect();
            if blocks.is_empty()
                || !blocks
                    .iter()
                    .all(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            {
                return None;
            }
            let texts: Option<Vec<&str>> = blocks
                .iter()
                .map(|block| match block.get("text") {
                    None => Some(""),
                    Some(text) => text.as_str(),
                })
                .collect();
            Some(texts?.join("\n"))
        }
        _ => None,
    }
}

/// An ISO-8601 transcript timestamp → (epoch seconds, original string); unparseable or missing
/// sorts to the epoch but keeps whatever string was there.
fn parse_timestamp(iso: Option<&Value>) -> (f64, String) {
    let Some(Value::String(iso)) = iso else {
        return (0.0, String::new());
    };
    if iso.is_empty() {
        return (0.0, String::new());
    }
    let ts = fromisoformat_timestamp(&iso.replace('Z', "+00:00")).unwrap_or(0.0);
    (ts, iso.clone())
}

// ----- collection (the only filesystem-touching path) --------------------------------------------

/// Every message across all chat history, oldest first: the durable history bundles under
/// `history_dir`, then the live transcripts the running records point at, deduped globally by
/// line uuid.
pub fn collect_messages(sessions: &[Session], history_dir: &Path) -> Vec<Message> {
    let role_of = role_resolver(sessions);
    let sender_role = |name: &str| role_of.get(name).copied();
    let mut messages = Vec::new();
    let mut seen = HashSet::new();
    for source in sources(sessions, history_dir) {
        let Ok(bytes) = std::fs::read(&source.path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let recipient = Recipient {
            id: &source.recipient_id,
            name: &source.recipient_name,
            cmd: "",
            chat_id: &source.chat_id,
        };
        for line in py_splitlines(&text) {
            let line = py_strip(line);
            if line.is_empty() {
                continue;
            }
            let Ok(obj) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let Some(message) = parse_message(&obj, &recipient, &sender_role) else {
                continue;
            };
            if seen.insert(message.uuid.clone()) {
                messages.push(message);
            }
        }
    }
    messages.sort_by(|a, b| a.ts.partial_cmp(&b.ts).unwrap_or(Ordering::Equal));
    messages
}

/// A stat-only digest (sha1 hex) of every message source — `path:size:mtime_ns` lines — so a
/// dashboard rebuilds the feed only when a transcript grows or appears.
pub fn source_signature(sessions: &[Session], history_dir: &Path) -> String {
    let parts: Vec<String> = sources(sessions, history_dir)
        .into_iter()
        .filter_map(|source| {
            let status = std::fs::metadata(&source.path).ok()?;
            let mtime_ns =
                i128::from(status.mtime()) * 1_000_000_000 + i128::from(status.mtime_nsec());
            Some(format!(
                "{}:{}:{mtime_ns}",
                source.path.display(),
                status.len()
            ))
        })
        .collect();
    sha1_hex(parts.join("\n").as_bytes())
}

struct Source {
    path: PathBuf,
    recipient_id: String,
    recipient_name: String,
    chat_id: String,
}

/// Every transcript to read: the history bundles (`<history_dir>/*/*/transcript.jsonl`,
/// path-sorted), then the mirrored transcripts of running records (`ext.history.chats`). Shared
/// by the collector and the signature.
fn sources(sessions: &[Session], history_dir: &Path) -> Vec<Source> {
    let by_id: HashMap<&str, &Session> = sessions
        .iter()
        .map(|session| (session.id.as_str(), session))
        .collect();
    let mut bundles = Vec::new();
    for tx_dir in read_dir_paths(history_dir) {
        for chat_dir in read_dir_paths(&tx_dir) {
            let path = chat_dir.join(BUNDLE_TRANSCRIPT_NAME);
            if std::fs::symlink_metadata(&path).is_ok() {
                bundles.push(path);
            }
        }
    }
    bundles.sort();
    let mut out = Vec::new();
    for path in bundles {
        let name_of = |dir: Option<&Path>| {
            dir.and_then(|dir| dir.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let chat_id = name_of(path.parent());
        let recipient_id = name_of(path.parent().and_then(|dir| dir.parent()));
        let session = by_id.get(recipient_id.as_str());
        out.push(Source {
            recipient_name: session.map_or_else(|| recipient_id.clone(), |s| s.name.clone()),
            path,
            recipient_id,
            chat_id,
        });
    }
    for session in sessions {
        if session.lifecycle != Lifecycle::Running {
            continue;
        }
        let Ok(history) = history_ext(session) else {
            continue;
        };
        for (chat_id, chat) in history.chats {
            out.push(Source {
                path: PathBuf::from(chat.transcript),
                recipient_id: session.id.to_string(),
                recipient_name: session.name.clone(),
                chat_id,
            });
        }
    }
    out
}

/// Child directories of `dir` (a missing dir has none).
fn read_dir_paths(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect()
}

/// The latest sign of life of a record: creation, end, or its last hook state change.
fn activity_at(session: &Session) -> f64 {
    let since = session
        .ext
        .get("hooks")
        .and_then(|hooks| hooks.get("since"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    session
        .created_at
        .max(session.ended_at.unwrap_or(0.0))
        .max(since)
}

/// name → role of the most recently active record of that name (names are reusable).
fn role_resolver(sessions: &[Session]) -> HashMap<String, SenderRole> {
    let mut best: HashMap<&str, (f64, SenderRole)> = HashMap::new();
    for session in sessions {
        let activity = activity_at(session);
        let role = match session.launch {
            Launch::Agent { .. } => SenderRole::Agent,
            Launch::Process { .. } => SenderRole::Shell,
        };
        let replace = best
            .get(session.name.as_str())
            .is_none_or(|(current, _)| activity > *current);
        if replace {
            best.insert(&session.name, (activity, role));
        }
    }
    best.into_iter()
        .map(|(name, (_, role))| (name.to_owned(), role))
        .collect()
}

// ----- Python str semantics ----------------------------------------------------------------------

/// `str.isspace` (also the `\s` of a `str` regex): Unicode White_Space plus `\x1c`–`\x1f`.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(py_isspace)
}

fn py_lstrip(s: &str) -> &str {
    s.trim_start_matches(py_isspace)
}

/// `\w` of a `str` regex.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `str.splitlines()` (no trailing empty piece).
fn py_splitlines(text: &str) -> Vec<&str> {
    let is_break = |c: char| {
        matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        )
    };
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if !is_break(c) {
            continue;
        }
        lines.push(&text[start..index]);
        let mut end = index + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|&(_, next)| next == '\n') {
            chars.next();
            end += 1;
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

// ----- datetime.fromisoformat(...).timestamp() ---------------------------------------------------

/// `datetime.fromisoformat(s).timestamp()` (CPython 3.14 grammar: extended/basic calendar and
/// week dates, any single separator char, `HH[:MM[:SS[.f…]]]` or basic, `24:00`, offsets). A naive
/// value is local time (`mktime`).
pub(crate) fn fromisoformat_timestamp(s: &str) -> Option<f64> {
    let (date, rest) = parse_date(s)?;
    let (days, time) = match rest {
        None => (date, Time::default()),
        Some(rest) => {
            let mut chars = rest.chars();
            chars.next()?;
            let time_text = chars.as_str();
            let time = parse_time(time_text)?;
            let days = if time.next_day { date + 1 } else { date };
            (days, time)
        }
    };
    if !(MIN_DAYS..=MAX_DAYS).contains(&days) {
        return None;
    }
    let seconds = days * 86_400 + time.seconds;
    match time.offset_micros {
        Some(offset) => {
            let micros = seconds * 1_000_000 + time.micros - offset;
            Some(micros as f64 / 1e6)
        }
        None => Some(local_to_epoch(days, time.seconds)? as f64 + time.micros as f64 / 1e6),
    }
}

/// Days since the epoch of 0001-01-01 and 9999-12-31.
const MIN_DAYS: i64 = -719_162;
const MAX_DAYS: i64 = 2_932_896;

#[derive(Default)]
struct Time {
    seconds: i64,
    micros: i64,
    next_day: bool,
    offset_micros: Option<i64>,
}

fn digits(s: &[u8], at: usize, count: usize) -> Option<i64> {
    let slice = s.get(at..at + count)?;
    slice.iter().try_fold(0i64, |acc, &b| {
        b.is_ascii_digit().then(|| acc * 10 + i64::from(b - b'0'))
    })
}

/// The date part → (days since epoch, the rest after it, if any).
fn parse_date(s: &str) -> Option<(i64, Option<&str>)> {
    let b = s.as_bytes();
    let year = digits(b, 0, 4)?;
    let extended = b.get(4) == Some(&b'-');
    let at = if extended { 5 } else { 4 };
    let (days, end) = if b.get(at) == Some(&b'W') {
        let week = digits(b, at + 1, 2)?;
        let mut end = at + 3;
        let mut weekday = 1;
        let dash = extended && b.get(end) == Some(&b'-');
        let day_at = if dash { end + 1 } else { end };
        if let Some(day) = digits(b, day_at, 1) {
            weekday = day;
            end = day_at + 1;
        } else if dash {
            return None;
        }
        (iso_week_days(year, week, weekday)?, end)
    } else {
        let month = digits(b, at, 2)?;
        let day_at = if extended {
            (b.get(at + 2) == Some(&b'-')).then_some(at + 3)?
        } else {
            at + 2
        };
        let day = digits(b, day_at, 2)?;
        (civil_days(year, month, day)?, day_at + 2)
    };
    Some((days, (end < s.len()).then(|| &s[end..])))
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a validated proleptic Gregorian date.
fn civil_days(year: i64, month: i64, day: i64) -> Option<i64> {
    if !(1..=9999).contains(&year)
        || !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
    {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// `date.fromisocalendar(year, week, weekday)` as days since the epoch.
fn iso_week_days(year: i64, week: i64, weekday: i64) -> Option<i64> {
    if !(1..=7).contains(&weekday) {
        return None;
    }
    let jan4 = civil_days(year, 1, 4)?;
    // 1970-01-01 was a Thursday: Monday-based weekday index 3.
    let jan4_weekday = (jan4 + 3).rem_euclid(7);
    let week1_monday = jan4 - jan4_weekday;
    let max_week = {
        let dec28 = civil_days(year, 12, 28)?;
        (dec28 - week1_monday) / 7 + 1
    };
    if !(1..=max_week).contains(&week) {
        return None;
    }
    Some(week1_monday + (week - 1) * 7 + weekday - 1)
}

/// `HH[:MM[:SS[(.|,)f…]]]` (or the basic forms) → (seconds, micros, bytes consumed).
fn parse_clock(b: &[u8]) -> Option<(i64, i64, i64, i64, usize)> {
    let mut values = [0i64; 3];
    let mut pos = 0;
    let mut separator: Option<bool> = None;
    let mut micros = 0;
    for (index, value) in values.iter_mut().enumerate() {
        *value = digits(b, pos, 2)?;
        pos += 2;
        let Some(&next) = b.get(pos) else {
            break;
        };
        if index == 2 {
            if next == b'.' || next == b',' {
                pos += 1;
                let start = pos;
                while b.get(pos).is_some_and(u8::is_ascii_digit) {
                    pos += 1;
                }
                let fraction = &b[start..pos];
                let kept = fraction.len().min(6);
                let parsed = digits(fraction, 0, kept).unwrap_or(0);
                micros = parsed * 10i64.pow((6 - kept) as u32);
            }
            break;
        }
        let colon = next == b':';
        match separator {
            None => separator = Some(colon),
            Some(expected) if expected != colon => return None,
            Some(_) => {}
        }
        if colon {
            pos += 1;
        } else if !next.is_ascii_digit() {
            break;
        }
    }
    Some((values[0], values[1], values[2], micros, pos))
}

fn parse_time(text: &str) -> Option<Time> {
    let b = text.as_bytes();
    let tz_at = b.iter().position(|&c| matches!(c, b'+' | b'-' | b'Z'));
    let clock = &b[..tz_at.unwrap_or(b.len())];
    let (hour, minute, second, micros, used) = parse_clock(clock)?;
    if used != clock.len() || minute > 59 || second > 59 {
        return None;
    }
    let mut time = Time {
        micros,
        ..Time::default()
    };
    if hour == 24 {
        if minute != 0 || second != 0 || micros != 0 {
            return None;
        }
        time.next_day = true;
    } else if hour > 23 {
        return None;
    } else {
        time.seconds = hour * 3600 + minute * 60 + second;
    }
    if let Some(at) = tz_at {
        let sign = b[at];
        let offset = &b[at + 1..];
        time.offset_micros = Some(if sign == b'Z' {
            if !offset.is_empty() {
                return None;
            }
            0
        } else {
            let (h, m, s, us, used) = parse_clock(offset)?;
            if used != offset.len() || m > 59 || s > 59 {
                return None;
            }
            let magnitude = ((h * 60 + m) * 60 + s) * 1_000_000 + us;
            if magnitude >= 86_400_000_000 {
                return None;
            }
            if sign == b'-' { -magnitude } else { magnitude }
        });
    }
    Some(time)
}

/// Local wall-clock (days since epoch + seconds of day) → epoch seconds, via `mktime`.
fn local_to_epoch(days: i64, seconds: i64) -> Option<libc::time_t> {
    let (year, month, day) = civil_from_days(days);
    // SAFETY: `tm` is a plain C struct; all-zero is a valid initial value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = i32::try_from(year - 1900).ok()?;
    tm.tm_mon = i32::try_from(month - 1).ok()?;
    tm.tm_mday = i32::try_from(day).ok()?;
    tm.tm_hour = i32::try_from(seconds / 3600).ok()?;
    tm.tm_min = i32::try_from(seconds / 60 % 60).ok()?;
    tm.tm_sec = i32::try_from(seconds % 60).ok()?;
    tm.tm_isdst = -1;
    // SAFETY: `tm` is a valid, exclusively borrowed `struct tm`.
    let epoch = unsafe { libc::mktime(&mut tm) };
    (epoch != -1 || (tm.tm_year == 69 && tm.tm_mon == 11 && tm.tm_mday == 31)).then_some(epoch)
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

// ----- sha1 (hashlib.sha1(...).hexdigest()) ------------------------------------------------------

fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (slot, word) in w.iter_mut().zip(block.as_chunks::<4>().0) {
            *slot = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6u32),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        for (state, value) in h.iter_mut().zip([a, b, c, d, e]) {
            *state = state.wrapping_add(value);
        }
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TS: &str = "2026-01-02T03:04:05.123Z";

    fn turn(content: Value, extra: Value) -> Value {
        let mut obj = json!({
            "type": "user", "message": {"role": "user", "content": content},
            "userType": "external", "timestamp": TS, "uuid": "u1",
        });
        for (key, value) in extra.as_object().unwrap() {
            obj[key] = value.clone();
        }
        obj
    }

    fn parse(obj: &Value, name: &str, cmd: &str) -> Option<Value> {
        let recipient = Recipient {
            id: "rid",
            name,
            cmd,
            chat_id: "c1",
        };
        let roles = |sender: &str| match sender {
            "alice" => Some(SenderRole::Agent),
            "home" => Some(SenderRole::Shell),
            _ => None,
        };
        parse_message(obj, &recipient, &roles).map(|message| message.to_value())
    }

    /// Gates and timestamps, from CPython 3.14 (recipient_name "bob", recipient_cmd "claude").
    #[test]
    fn gates_and_timestamps_match_python() {
        let ts_of = |extra: Value| {
            parse(&turn(json!("q"), extra), "bob", "claude").map(|value| value["ts"].clone())
        };
        assert_eq!(ts_of(json!({"isMeta": true})), None);
        assert_eq!(ts_of(json!({"isSidechain": 1})), None);
        assert_eq!(ts_of(json!({"userType": "internal"})), None);
        assert_eq!(ts_of(json!({"isMeta": 0})), Some(json!(1767323045.123)));
        assert_eq!(
            ts_of(json!({"userType": null, "timestamp": "2026-01-02T03:04:05+00:00"})),
            Some(json!(1767323045.0))
        );
        let with_ts = |ts: Value| ts_of(json!({ "timestamp": ts }));
        assert_eq!(with_ts(json!("bad")), Some(json!(0.0)));
        assert_eq!(with_ts(json!(5)), Some(json!(0.0)));
        assert_eq!(
            with_ts(json!("2026-W01-1T00:00Z")),
            Some(json!(1766966400.0))
        );
        assert_eq!(
            with_ts(json!("2026-01-02T030405.25-0130")),
            Some(json!(1767328445.25))
        );
        assert_eq!(
            with_ts(json!("2026-01-02T24:00:00Z")),
            Some(json!(1767398400.0))
        );
        let bad_iso = parse(
            &turn(json!("q"), json!({"timestamp": "bad"})),
            "bob",
            "claude",
        );
        assert_eq!(bad_iso.unwrap()["iso"], json!("bad"));
        let no_iso = parse(&turn(json!("q"), json!({"timestamp": 5})), "bob", "claude");
        assert_eq!(no_iso.unwrap()["iso"], json!(""));
        assert_eq!(
            parse(&turn(json!([{"type": "text"}]), json!({})), "bob", "x"),
            None
        );
        assert_eq!(parse(&turn(json!([]), json!({})), "bob", "x"), None);
    }

    /// `datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp()` from CPython 3.14.
    #[test]
    fn fromisoformat_matches_python() {
        let cases: [(&str, Option<f64>); 22] = [
            ("2026-01-02T03:04:05.123Z", Some(1767323045.123)),
            ("2026-01-02T03:04:05.1+02:00", Some(1767315845.1)),
            ("20260102T030405Z", Some(1767323045.0)),
            ("2026-01-02T03:04:05.1234567Z", Some(1767323045.123456)),
            ("2026-01-02T03:04Z", Some(1767323040.0)),
            ("2026-01-02T03Z", Some(1767322800.0)),
            ("2026-01-02T03:04:05,5Z", Some(1767323045.5)),
            ("2026-01-02T03:04:05+0530", Some(1767303245.0)),
            ("2026-01-02T03:04:05-05", Some(1767341045.0)),
            ("2026-01-02T03:04:05+23:59:59.5", Some(1767236645.5)),
            ("2026-12-31T23:59:59.999999+00:00", Some(1798761599.999999)),
            ("1970-01-01T00:00:00Z", Some(0.0)),
            ("2026-01-02x03:04+00:00", Some(1767323040.0)),
            ("bad", None),
            ("2026-02-30T00:00:00Z", None),
            ("2026-01-02T25:00:00Z", None),
            ("2026-01-02T24:00:01Z", None),
            ("2026-01-02T", None),
            ("2026-01-02T03:0405Z", None),
            ("2026-01-02T03:04:05+24:00", None),
            ("0000-01-01", None),
            ("2026-01-02T03:04:05 ", None),
        ];
        for (input, expected) in cases {
            let got = fromisoformat_timestamp(&input.replace('Z', "+00:00"));
            assert_eq!(got, expected, "{input}");
        }
    }

    #[test]
    fn build_envelope_shapes() {
        assert_eq!(
            build_envelope("alice", "hi <b> & \"q\"", TAG_AGENT),
            "<from-agent session=\"alice\">hi <b> & \"q\"</from-agent>"
        );
        assert_eq!(
            build_envelope("viewer", "q?", TAG_USER),
            "<from-user session=\"viewer\">q?</from-user>"
        );
    }

    #[test]
    fn python_str_helpers() {
        assert_eq!(
            py_splitlines("a\r\nb\rc\u{2028}d\u{1c}e\n"),
            ["a", "b", "c", "d", "e"]
        );
        assert_eq!(py_splitlines("\n"), [""]);
        assert!(py_splitlines("").is_empty());
        assert_eq!(py_strip("\u{1f} x \u{3000}"), "x");
        assert!(is_priming(&format!(
            "{}as your first actions",
            "é".repeat(378)
        )));
        assert!(!is_priming(&format!(
            "{}as your first actions",
            "é".repeat(380)
        )));
    }

    #[test]
    fn sha1_matches_hashlib() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(&[b'a'; 1000]),
            "291e9a6c66994949b57ba5e650361e98fc36b1ba"
        );
    }
}
