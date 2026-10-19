//! The `history` plugin's verbs: `_history-ingest` (run by the agent's own Stop hooks and
//! re-run detached), `history`, and `history-hooks`.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Map, Value};
use tx_core::events::now;
use tx_core::{
    CoreError, Lifecycle, ListFilter, Session, SessionId, TxCore, Verb, VerbError, VerbIo,
};
use tx_ui::render::{chips, reltime};

use crate::HISTORY_ID;
use crate::detach::detach;
use crate::engines::BUNDLE_TRANSCRIPT_NAME;
use crate::ext::{HistoryChat, HistoryExt, chat_ext, history_ext, owned_elsewhere};
use crate::install::{install, uninstall};
use crate::mirror::{HISTORY_ENTRIES, IngestMode, ingest_session};

pub(crate) const SESSION_ID_ENV: &str = "TX_SESSION_ID";

const INGEST_USAGE: &str = "usage: txc _history-ingest [--wait] [<session>]\n";
const HISTORY_USAGE: &str = "\
usage: txc history [--tag T] [--cwd S] [--since YYYY-MM-DD] [--until YYYY-MM-DD]
       txc history <session>
";
const HOOKS_USAGE: &str = "usage: txc history-hooks install [--dry-run] | uninstall\n";

pub fn verbs(history_dir: PathBuf) -> Vec<Arc<dyn Verb>> {
    vec![
        Arc::new(Ingest {
            history_dir: history_dir.clone(),
        }),
        Arc::new(History { history_dir }),
        Arc::new(HistoryHooks),
    ]
}

pub(crate) fn say(out: &mut dyn Write, text: &str) -> Result<(), VerbError> {
    out.write_all(text.as_bytes())
        .map_err(|e| VerbError::Failed(format!("write: {e}")))
}

/// The hook payload, tolerantly: `{}` for none / unreadable / bad JSON / non-object.
pub(crate) fn read_payload(stdin: &mut dyn Read) -> Value {
    let mut text = String::new();
    if stdin.read_to_string(&mut text).is_err() {
        return Value::Object(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::Object(Map::new()),
    }
}

pub(crate) fn home_dir() -> Result<PathBuf, VerbError> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| VerbError::Failed("HOME is not set".into()))
}

pub(crate) fn resolve(core: &TxCore, token: &str) -> Result<Option<SessionId>, VerbError> {
    match core.resolve(token) {
        Ok(id) => Ok(Some(id)),
        Err(CoreError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

struct Ingest {
    history_dir: PathBuf,
}

impl Verb for Ingest {
    fn name(&self) -> &'static str {
        "_history-ingest"
    }

    fn usage(&self) -> &'static str {
        "_history-ingest [--wait] [<session>]   mirror transcripts (agent hook)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let (mode, token) = match argv {
            [] => return self.hook(core, io),
            [token] if token != "--wait" => (IngestMode::Coalesce, token),
            [flag, token] if flag == "--wait" => (IngestMode::Wait, token),
            _ => {
                say(io.stderr, INGEST_USAGE)?;
                return Ok(2);
            }
        };
        if let Some(id) = resolve(core, token)? {
            ingest_session(core, &self.history_dir, &id, mode)?;
        }
        Ok(0)
    }
}

impl Ingest {
    /// Hook mode: record the chat the agent reports, then mirror it detached. "Not ours" and
    /// bad payloads are silent successes.
    fn hook(&self, core: &TxCore, io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
        let Some(id) = std::env::var(SESSION_ID_ENV)
            .ok()
            .and_then(|raw| raw.parse::<SessionId>().ok())
        else {
            return Ok(0);
        };
        let payload = read_payload(io.stdin);
        if let (Some(chat_id), Some(transcript)) = (
            payload.get("session_id").and_then(Value::as_str),
            payload.get("transcript_path").and_then(Value::as_str),
        ) && (listed_here(core, &id, chat_id) || !owned_elsewhere(core, &id, chat_id)?)
        {
            match record(core, &id, chat_id, transcript) {
                Ok(()) => {}
                Err(CoreError::NotFound(_)) => return Ok(0),
                Err(error) => return Err(error.into()),
            }
        }
        if core.get_session(&id).is_ok()
            && let Ok(exe) = std::env::current_exe()
        {
            let _ = detach(
                &exe,
                &core.config().state_dir,
                &["_history-ingest", &id.to_string()],
                &[],
            );
        }
        Ok(0)
    }
}

/// The session's own `ext.history` already lists the chat: ownership was settled then.
fn listed_here(core: &TxCore, id: &SessionId, chat_id: &str) -> bool {
    core.get_session(id)
        .ok()
        .and_then(|session| history_ext(&session).ok())
        .is_some_and(|ext| ext.chats.contains_key(chat_id))
}

/// New entry or changed transcript: write `{transcript, bundle: <kept>, at: now}`. A malformed
/// `ext.history` is left alone.
fn record(core: &TxCore, id: &SessionId, chat_id: &str, transcript: &str) -> Result<(), CoreError> {
    core.update_ext(id, HISTORY_ID, |value| {
        let Ok(mut ext) = (match &*value {
            Value::Null => Ok(HistoryExt::default()),
            other => serde_json::from_value::<HistoryExt>(other.clone()),
        }) else {
            return;
        };
        let kept = ext.chats.get(chat_id);
        if kept.is_some_and(|chat| chat.transcript == transcript) {
            return;
        }
        let entry = HistoryChat {
            transcript: transcript.to_owned(),
            bundle: kept.and_then(|chat| chat.bundle.clone()),
            at: now(),
            rest: kept.map(|chat| chat.rest.clone()).unwrap_or_default(),
        };
        ext.chats.insert(chat_id.to_owned(), entry);
        if let Ok(updated) = serde_json::to_value(&ext) {
            *value = updated;
        }
    })
    .map(drop)
}

struct History {
    history_dir: PathBuf,
}

impl Verb for History {
    fn name(&self) -> &'static str {
        "history"
    }

    fn usage(&self) -> &'static str {
        "history [--tag T] [--cwd S] [--since D] [--until D] | <session>   past sessions, mirrored chats"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv {
            [token] if !token.starts_with('-') => self.bundles(core, io, token),
            _ => self.list(core, io, argv),
        }
    }
}

#[derive(Default)]
struct Filters {
    tag: Option<String>,
    cwd: Option<String>,
    since: Option<String>,
    until: Option<String>,
}

fn parse_filters(argv: &[String]) -> Option<Filters> {
    let mut filters = Filters::default();
    let mut args = argv.iter();
    while let Some(flag) = args.next() {
        let slot = match flag.as_str() {
            "--tag" => &mut filters.tag,
            "--cwd" => &mut filters.cwd,
            "--since" => &mut filters.since,
            "--until" => &mut filters.until,
            _ => return None,
        };
        *slot = Some(args.next()?.clone());
    }
    Some(filters)
}

fn date_bound(raw: Option<&str>, flag: &str, end_of_day: bool) -> Result<Option<f64>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let Some((year, month, day)) = parse_ymd(raw) else {
        return Err(format!("history: {flag} expects YYYY-MM-DD, got '{raw}'"));
    };
    let (h, m, s) = if end_of_day { (23, 59, 59) } else { (0, 0, 0) };
    Ok(Some(local_epoch(year, month, day, h, m, s)))
}

fn ended_or_created(session: &Session) -> f64 {
    session
        .ended_at
        .filter(|ended| *ended != 0.0)
        .unwrap_or(session.created_at)
}

fn trunc(text: &str, width: usize) -> String {
    if text.chars().count() > width {
        text.chars().take(width - 1).collect::<String>() + "…"
    } else {
        text.to_owned()
    }
}

impl History {
    fn list(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let Some(filters) = parse_filters(argv) else {
            say(io.stderr, HISTORY_USAGE)?;
            return Ok(2);
        };
        let bounds = date_bound(filters.since.as_deref(), "--since", false).and_then(|since| {
            date_bound(filters.until.as_deref(), "--until", true).map(|until| (since, until))
        });
        let (since, until) = match bounds {
            Ok(bounds) => bounds,
            Err(message) => {
                say(io.stderr, &format!("{message}\n"))?;
                return Ok(2);
            }
        };
        let mut shown = Vec::new();
        for lifecycle in [Lifecycle::Exited, Lifecycle::Stopped] {
            for id in core.list_sessions(&ListFilter::new().lifecycle(lifecycle))? {
                let session = match core.get_session(&id) {
                    Ok(session) => session,
                    Err(CoreError::NotFound(_)) => continue,
                    Err(error) => return Err(error.into()),
                };
                let ended = ended_or_created(&session);
                if filters
                    .tag
                    .as_ref()
                    .is_none_or(|t| session.tags.contains(t))
                    && filters
                        .cwd
                        .as_ref()
                        .is_none_or(|c| session.cwd.to_string_lossy().contains(c.as_str()))
                    && since.is_none_or(|s| ended >= s)
                    && until.is_none_or(|u| ended <= u)
                {
                    shown.push(session);
                }
            }
        }
        shown.sort_by(|a, b| ended_or_created(b).total_cmp(&ended_or_created(a)));
        let text = render(&shown, now());
        say(io.stdout, &format!("{text}\n"))?;
        Ok(0)
    }

    fn bundles(&self, core: &TxCore, io: &mut VerbIo<'_>, token: &str) -> Result<i32, VerbError> {
        let Some(id) = resolve(core, token)? else {
            say(
                io.stderr,
                &format!("history: session '{token}' not found\n"),
            )?;
            return Ok(1);
        };
        let root = self.history_dir.join(id.to_string());
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        dirs.sort();
        if dirs.is_empty() {
            say(io.stdout, "(no mirrored chats)\n")?;
            return Ok(0);
        }
        for dir in dirs {
            let bytes = std::fs::metadata(dir.join(BUNDLE_TRANSCRIPT_NAME))
                .map(|meta| meta.len())
                .unwrap_or(0);
            let chat_id = dir.file_name().unwrap_or_default().to_string_lossy();
            say(
                io.stdout,
                &format!("{chat_id}  {bytes}  {}\n", dir.display()),
            )?;
        }
        Ok(0)
    }
}

fn render(sessions: &[Session], now: f64) -> String {
    if sessions.is_empty() {
        return "HISTORY\n  (no exited or stopped sessions)".into();
    }
    let mut lines = vec!["HISTORY".to_owned()];
    for session in sessions {
        let chats = chat_ext(session).map(|ext| ext.chats.len()).unwrap_or(0);
        lines.push(format!(
            "  {:<24} {:<8} {:>5}  {:>2}c {}  {}",
            trunc(&session.name, 24),
            session.lifecycle.as_str(),
            reltime(Some(ended_or_created(session)), now),
            chats,
            chips(&session.tags),
            session.cwd.display(),
        ));
    }
    lines.join("\n")
}

/// `datetime.strptime(raw, "%Y-%m-%d")` -> `(year, month, day)`, `None` where Python raises.
fn parse_ymd(raw: &str) -> Option<(i32, u32, u32)> {
    fn digits(part: &str, min: usize, max: usize) -> Option<u32> {
        (part.len() >= min && part.len() <= max && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())
            .flatten()
    }
    let mut parts = raw.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let year = digits(year, 4, 4)?;
    let month = digits(month, 1, 2).filter(|m| (1..=12).contains(m))?;
    // `%d` also takes a space-padded single digit (` 5`).
    let day = match day.strip_prefix(' ') {
        Some(rest) => digits(rest, 1, 1).filter(|d| *d >= 1)?,
        None => digits(day, 1, 2)?,
    };
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let days_in_month = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (year >= 1 && (1..=days_in_month).contains(&day)).then_some((year as i32, month, day))
}

/// Local-time epoch seconds (naive `datetime.timestamp()`).
fn local_epoch(year: i32, month: u32, day: u32, hour: i32, minute: i32, second: i32) -> f64 {
    // SAFETY: `tm` is a plain C struct we own and fully initialise before `mktime` reads it.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month as i32 - 1;
    tm.tm_mday = day as i32;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = second;
    tm.tm_isdst = -1;
    // SAFETY: see above; mktime only normalises the struct in place.
    unsafe { libc::mktime(&mut tm) as f64 }
}

struct HistoryHooks;

impl Verb for HistoryHooks {
    fn name(&self) -> &'static str {
        "history-hooks"
    }

    fn usage(&self) -> &'static str {
        "history-hooks install [--dry-run] | uninstall   mirror transcripts on the agent's Stop"
    }

    fn run(&self, _core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let fail = |e: crate::install::InstallError| VerbError::Failed(e.to_string());
        match argv {
            [cmd] if cmd == "install" => self.install(io, false),
            [cmd, flag] if cmd == "install" && flag == "--dry-run" => self.install(io, true),
            [cmd] if cmd == "uninstall" => {
                uninstall(&home_dir()?, &HISTORY_ENTRIES, io.stdout).map_err(fail)?;
                Ok(0)
            }
            _ => {
                say(io.stderr, HOOKS_USAGE)?;
                Ok(2)
            }
        }
    }
}

impl HistoryHooks {
    fn install(&self, io: &mut VerbIo<'_>, dry_run: bool) -> Result<i32, VerbError> {
        let exe =
            std::env::current_exe().map_err(|e| VerbError::Failed(format!("current_exe: {e}")))?;
        install(&home_dir()?, &exe, &HISTORY_ENTRIES, dry_run, io.stdout)
            .map_err(|e| VerbError::Failed(e.to_string()))?;
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ymd_follows_strptime() {
        assert_eq!(parse_ymd("2026-09-15"), Some((2026, 9, 15)));
        assert_eq!(parse_ymd("2026-9-5"), Some((2026, 9, 5)));
        assert_eq!(parse_ymd("2024-02-29"), Some((2024, 2, 29)));
        assert_eq!(parse_ymd("2026-09- 5"), Some((2026, 9, 5)));
        for bad in [
            "15-09-2026",
            "2026-13-01",
            "2026-02-29",
            "2026-00-10",
            "2026-09-31",
            "0000-01-01",
            "2026-09-15x",
            "2026-09",
            "",
            "26-09-15",
            "2026-009-15",
        ] {
            assert_eq!(parse_ymd(bad), None, "{bad}");
        }
    }

    #[test]
    fn local_epoch_end_of_day_is_one_second_before_midnight() {
        let start = local_epoch(2026, 9, 15, 0, 0, 0);
        let end = local_epoch(2026, 9, 15, 23, 59, 59);
        assert_eq!(end - start, 86399.0);
    }
}
