//! Port of lib/tx/render.py — `tx ls`, the `_list` picker feed, `tx history`, `tx chat ls` and the
//! `tx artifact ls|show` tables.
//!
//! Pure formatting: every function takes the records, `now` (the caller's `time.time()`) and the
//! widths it needs; nothing reads the clock or the environment.

use std::collections::HashMap;

use serde_json::Number;

use crate::artifact::{Artifact, USER_ACTOR};
use crate::palette::{RESET_FG, WARN_ANSI, tag_ansi};
use crate::session::{Location, Session};

/// LOCATION column width, shared with the picker header and `tx ls`.
pub const LOCATION_W: usize = 25;
/// ROLE column width (the longest role, `shell` / `other`).
pub const ROLE_W: usize = 5;

const NAMEW_OVERHEAD: i64 = 65;
const NAMEW_FLOOR: i64 = 12;
const NAMEW_CEILING: i64 = 60;

/// A Python-truthy timestamp: `None` and `0` are both "unset".
fn truthy(n: Option<&Number>) -> Option<f64> {
    n.and_then(Number::as_f64).filter(|value| *value != 0.0)
}

/// `s[:end]` with Python slice semantics (code points; a negative `end` counts from the back).
fn py_prefix(s: &str, end: i64) -> String {
    let len = s.chars().count() as i64;
    let end = if end < 0 { (len + end).max(0) } else { end };
    s.chars().take(end as usize).collect()
}

/// Compact relative age (`-`, `5s`, `3m`, `2h`, `4d`); `-` for an unset or zero epoch.
pub fn reltime(epoch: Option<&Number>, now: f64) -> String {
    let Some(epoch) = truthy(epoch) else {
        return "-".into();
    };
    // `int()` truncates toward zero, as does `as`; the clamp keeps the divisions non-negative.
    let delta = ((now - epoch) as i64).max(0);
    match delta {
        d if d < 60 => format!("{d}s"),
        d if d < 3600 => format!("{}m", d / 60),
        d if d < 86400 => format!("{}h", d / 3600),
        d => format!("{}d", d / 86400),
    }
}

fn chips<S: AsRef<str>>(tags: &[S]) -> String {
    tags.iter()
        .map(|tag| format!(" [{}]", tag.as_ref()))
        .collect()
}

/// The LOCATION cell: primary `window[pane]` + ` +N` overflow, `—` when attached nowhere. Only the
/// window name is clipped (with `…`) so the cell fits [`LOCATION_W`].
pub fn location_text(locations: &[Location]) -> String {
    let Some(primary) = locations.first() else {
        return "—".into();
    };
    let suffix = if locations.len() > 1 {
        format!(" +{}", locations.len() - 1)
    } else {
        String::new()
    };
    let tail = format!("[{}]{suffix}", primary.pane_index);
    let tail_len = tail.chars().count();
    let mut name = primary.window_name.clone();
    if name.chars().count() + tail_len > LOCATION_W {
        name = py_prefix(&name, LOCATION_W as i64 - tail_len as i64 - 1) + "…";
    }
    format!("{name}{tail}")
}

/// Newest first; ties keep their input order (Python's stable `sorted(reverse=True)`).
fn sorted_desc<T>(items: &[T], key: impl Fn(&T) -> f64) -> Vec<&T> {
    let mut ordered: Vec<&T> = items.iter().collect();
    ordered.sort_by(|a, b| key(b).total_cmp(&key(a)));
    ordered
}

/// IDLE: the last-turn age for an llm session, `—` otherwise (no activity signal).
fn idle_cell(session: &Session, now: f64) -> String {
    match session.llm() {
        Some(llm) => reltime(llm.last_activity.as_ref(), now),
        None => "—".into(),
    }
}

/// `tx ls`: a single PROCESSES listing, newest activity first.
pub fn render_ls(sessions: &[Session], now: f64) -> String {
    let mut lines = vec!["PROCESSES".to_owned()];
    for session in sorted_desc(sessions, Session::activity_at) {
        lines.push(format!(
            "  {:<24} {:<8} {:<LOCATION_W$} {:<6}{}",
            session.name,
            session.state.as_str(),
            location_text(&session.attached_to),
            idle_cell(session, now),
            chips(&session.tags),
        ));
    }
    lines.join("\n")
}

/// The picker NAME column width: fit the longest name, capped so the other columns fit `cols`.
pub fn picker_namew(cols: i64, longest: i64) -> i64 {
    let ceiling = NAMEW_FLOOR.max(NAMEW_CEILING.min(cols - NAMEW_OVERHEAD));
    NAMEW_FLOOR.max(longest).min(ceiling)
}

/// Truncate to `width` code points with a trailing `…`.
fn trunc(text: &str, width: i64) -> String {
    if text.chars().count() as i64 > width {
        py_prefix(text, width - 1) + "…"
    } else {
        text.to_owned()
    }
}

/// One fzf row: `name \t plain_chips \t L \t tmux_target \t visual`.
fn picker_row(session: &Session, namew: i64, now: f64) -> String {
    let role = session.role().as_str();
    let name_disp = trunc(&session.name, namew);
    let pad = (namew - name_disp.chars().count() as i64).max(0) as usize;
    let colored_chips: String = session
        .tags
        .iter()
        .map(|tag| format!(" {}[{tag}]{RESET_FG}", tag_ansi(tag)))
        .collect();
    let started = reltime(session.created_at.as_ref(), now);
    let idle = idle_cell(session, now);
    let role_cell = format!("{}{role:<ROLE_W$}{RESET_FG}", tag_ansi(role));
    let visual = format!(
        "{name_disp}{}   {:<LOCATION_W$} {started:<7} {WARN_ANSI}{idle:<6}{RESET_FG} {role_cell}{colored_chips}",
        " ".repeat(pad),
        location_text(&session.attached_to),
    );
    [
        session.name.as_str(),
        &chips(&session.tags),
        "L",
        session.tmux_name(),
        &visual,
    ]
    .join("\t")
}

/// The fzf picker feed (`tx _list`), newest activity first.
pub fn picker_display_rows(sessions: &[Session], namew: i64, now: f64) -> String {
    sorted_desc(sessions, Session::activity_at)
        .into_iter()
        .map(|session| picker_row(session, namew, now))
        .collect::<Vec<_>>()
        .join("\n")
}

fn ended_or_activity(session: &Session) -> f64 {
    truthy(session.ended_at.as_ref()).unwrap_or_else(|| session.activity_at())
}

/// `tx history`: exited / archived records, newest-ended first.
pub fn render_history(sessions: &[Session], now: f64) -> String {
    if sessions.is_empty() {
        return "HISTORY\n  (no exited or archived sessions)".into();
    }
    let mut lines = vec!["HISTORY".to_owned()];
    for session in sorted_desc(sessions, ended_or_activity) {
        let when = Number::from_f64(ended_or_activity(session));
        lines.push(format!(
            "  {:<24} {:<8} {:>5}  {:>2}c {}  {}",
            trunc(&session.name, 24),
            session.state.as_str(),
            reltime(when.as_ref(), now),
            session.chats().len(),
            chips(&session.tags),
            session.cwd,
        ));
    }
    lines.join("\n")
}

/// `tx chat ls <session>`: one line per chat ref.
pub fn render_chats(session: &Session, now: f64) -> String {
    let chats = session.chats();
    let header = format!("{} — {} chat(s)", session.name, chats.len());
    if chats.is_empty() {
        return header + "\n  (none)";
    }
    let mut lines = vec![header];
    for chat in chats {
        let identifier = match chat.id.as_deref() {
            Some(id) if !id.is_empty() => py_prefix(id, 8),
            _ => "pending".into(),
        };
        let mut origin = chat.origin.how.clone();
        if let Some(parent) = chat.origin.chat_id.as_deref().filter(|id| !id.is_empty()) {
            origin.push('←');
            origin.push_str(&py_prefix(parent, 8));
        }
        let bundle = chat
            .bundle_path
            .as_deref()
            .filter(|path| !path.is_empty())
            .unwrap_or("—");
        lines.push(format!(
            "  {identifier:<8}  {:<9} {origin:<18} {:>4} ago   {bundle}",
            chat.role,
            reltime(chat.started_at.as_ref(), now),
        ));
    }
    lines.join("\n")
}

/// Distinct touch authors in first-touch order.
fn distinct_authors(artifact: &Artifact) -> Vec<&str> {
    let mut authors: Vec<&str> = Vec::new();
    for touch in artifact.history() {
        if !authors.contains(&touch.session_id.as_str()) {
            authors.push(&touch.session_id);
        }
    }
    authors
}

/// A touch author's display label: the live session name, `user` verbatim, else a short id.
pub fn actor_label(session_id: &str, names: &HashMap<String, String>) -> String {
    if session_id == USER_ACTOR {
        return USER_ACTOR.into();
    }
    match names.get(session_id) {
        Some(name) if !name.is_empty() => name.clone(),
        _ => py_prefix(session_id, 8),
    }
}

fn title_or_filename(artifact: &Artifact) -> &str {
    artifact
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .unwrap_or(&artifact.filename)
}

fn updated_at(artifact: &Artifact) -> f64 {
    artifact.updated_at().as_f64().unwrap_or(0.0)
}

/// `tx artifact ls`: newest-touched first; `names` is the caller-resolved id→name map.
pub fn render_artifacts(
    artifacts: &[Artifact],
    names: &HashMap<String, String>,
    now: f64,
) -> String {
    if artifacts.is_empty() {
        return "ARTIFACTS\n  (none)".into();
    }
    let mut lines = vec!["ARTIFACTS".to_owned()];
    for artifact in sorted_desc(artifacts, updated_at) {
        let labels: Vec<String> = distinct_authors(artifact)
            .into_iter()
            .map(|author| actor_label(author, names))
            .collect();
        lines.push(format!(
            "  {:<8}  {:>3}r  {:>5}  {:<28}{}",
            py_prefix(&artifact.id, 8),
            artifact.history().len(),
            reltime(Some(artifact.updated_at()), now),
            trunc(title_or_filename(artifact), 28),
            chips(&labels),
        ));
    }
    lines.join("\n")
}

/// `tx artifact show`: metadata + the full touch log. `dirty` and `resolved_group` are computed by
/// the caller; a `None` resolved group prints as Python's `None`.
pub fn render_artifact_show(
    artifact: &Artifact,
    dirty: bool,
    names: &HashMap<String, String>,
    now: f64,
    resolved_group: Option<&str>,
) -> String {
    let working = if dirty {
        "dirty — un-snapshotted edits (close with `tx artifact modify`)"
    } else {
        "clean"
    };
    let group = match &artifact.group {
        Some(group) => group.clone(),
        None => format!("derived: {}", resolved_group.unwrap_or("None")),
    };
    let mut lines = vec![
        format!("{}  ({})", title_or_filename(artifact), artifact.id),
        format!("  filename:   {}", artifact.filename),
        format!(
            "  created:    {} ago",
            reltime(Some(&artifact.created_at), now)
        ),
        format!(
            "  updated:    {} ago",
            reltime(Some(artifact.updated_at()), now)
        ),
        format!("  group:      {group}"),
        format!("  revisions:  {}", artifact.history().len()),
        format!("  working:    {working}"),
        "  history:".into(),
    ];
    for touch in artifact.history() {
        let note = match touch.changes.as_deref() {
            Some(changes) if !changes.is_empty() => format!("  {changes}"),
            _ => String::new(),
        };
        lines.push(format!(
            "    rev {:<3} {:>5} ago  {:<20}{note}",
            touch.rev,
            reltime(Some(&touch.at), now),
            actor_label(&touch.session_id, names),
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::grouping::GroupResolver;

    const NOW: f64 = 1_000_000.0;

    /// Crafted records and the output of lib/tx/render.py + grouping.py over them (CPython 3.14,
    /// `now` = 1e6, names = id→name of every session, show `dirty` on even indexes, the third show
    /// with `resolved_group=None`).
    const CORPUS: &str = r#"{
 "sessions": [
  {
   "schema_version": 6,
   "id": "a1",
   "name": "assist",
   "role": "llm",
   "state": "idle",
   "cwd": "/r/a1",
   "cmd": "",
   "tags": [
    "docs",
    "é"
   ],
   "group": null,
   "env": {},
   "parent": null,
   "pid": null,
   "attached_to": [],
   "created_at": 100,
   "ended_at": null,
   "engine": "claude",
   "last_activity": 999910.0,
   "chats": [
    {
     "id": "abcdef0123456789",
     "role": "original",
     "cwd": "/r",
     "transcript_path": "/t",
     "origin": {
      "how": "spawn",
      "session_id": "s",
      "chat_id": null
     },
     "bundle_path": null,
     "started_at": 999100.0,
     "ended_at": null,
     "summary": "",
     "engine": "claude"
    },
    {
     "id": null,
     "role": "fork",
     "cwd": "/r",
     "transcript_path": "/t",
     "origin": {
      "how": "fork",
      "session_id": "s",
      "chat_id": "0123456789abcdef"
     },
     "bundle_path": "/h/b",
     "started_at": 999220.0,
     "ended_at": null,
     "summary": "",
     "engine": "claude"
    },
    {
     "id": "",
     "role": "original",
     "cwd": "/r",
     "transcript_path": "/t",
     "origin": {
      "how": "resume",
      "session_id": "s",
      "chat_id": ""
     },
     "bundle_path": "",
     "started_at": 0,
     "ended_at": null,
     "summary": "",
     "engine": "claude"
    }
   ],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "b1",
   "name": "worker",
   "role": "llm",
   "state": "idle",
   "cwd": "/r/b1",
   "cmd": "",
   "tags": [
    "feat-x",
    "extra"
   ],
   "group": null,
   "env": {},
   "parent": "a1",
   "pid": null,
   "attached_to": [
    {
     "host": "Views",
     "window_index": "1",
     "window_name": "work",
     "pane_id": "%1",
     "pane_index": "2"
    },
    {
     "host": "Views",
     "window_index": "1",
     "window_name": "x",
     "pane_id": "%1",
     "pane_index": "1"
    }
   ],
   "created_at": 200.5,
   "ended_at": null,
   "engine": "claude",
   "last_activity": 999910.0,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "c1",
   "name": "child-with-a-rather-long-name-日本",
   "role": "llm",
   "state": "exited",
   "cwd": "/r/c1",
   "cmd": "",
   "tags": [],
   "group": null,
   "env": {},
   "parent": "b1",
   "pid": null,
   "attached_to": [],
   "created_at": 300,
   "ended_at": 994600.0,
   "engine": "claude",
   "last_activity": null,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "h1",
   "name": "tx-assistant",
   "role": "llm",
   "state": "waiting",
   "cwd": "/r/h1",
   "cmd": "",
   "tags": [],
   "group": "hubgrp",
   "env": {},
   "parent": "x1",
   "pid": null,
   "attached_to": [],
   "created_at": 50,
   "ended_at": null,
   "engine": "claude",
   "last_activity": 0,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "x1",
   "name": "up",
   "role": "llm",
   "state": "archived",
   "cwd": "/r/x1",
   "cmd": "",
   "tags": [],
   "group": "upgrp",
   "env": {},
   "parent": null,
   "pid": null,
   "attached_to": [],
   "created_at": 10,
   "ended_at": 0,
   "engine": "claude",
   "last_activity": 1100000.0,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "v1",
   "name": "view",
   "role": "nvim",
   "state": "alive",
   "cwd": "/o/v1",
   "cmd": "",
   "tags": [
    "artifact"
   ],
   "group": null,
   "env": {},
   "parent": null,
   "pid": null,
   "attached_to": [
    {
     "host": "Views",
     "window_index": "1",
     "window_name": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
     "pane_id": "%1",
     "pane_index": "1"
    },
    {
     "host": "Views",
     "window_index": "1",
     "window_name": "b",
     "pane_id": "%1",
     "pane_index": "3"
    }
   ],
   "created_at": 740800.0,
   "ended_at": null,
   "artifact_id": "art1"
  },
  {
   "schema_version": 6,
   "id": "v2",
   "name": "loop",
   "role": "nvim",
   "state": "alive",
   "cwd": "/o/v2",
   "cmd": "",
   "tags": [],
   "group": null,
   "env": {},
   "parent": null,
   "pid": null,
   "attached_to": [
    {
     "host": "Views",
     "window_index": "1",
     "window_name": "aaaaaaaaaaaaaaaaaaaaaa",
     "pane_id": "%1",
     "pane_index": "12"
    }
   ],
   "created_at": 740800.0,
   "ended_at": null,
   "artifact_id": "art3"
  },
  {
   "schema_version": 6,
   "id": "s1",
   "name": "sh",
   "role": "shell",
   "state": "alive",
   "cwd": "/o/s1",
   "cmd": "",
   "tags": [
    "xxx"
   ],
   "group": null,
   "env": {},
   "parent": null,
   "pid": null,
   "attached_to": [],
   "created_at": 999940.1,
   "ended_at": null,
   "artifact_id": null
  },
  {
   "schema_version": 6,
   "id": "o1",
   "name": "misc",
   "role": "other",
   "state": "exited",
   "cwd": "/o/o1",
   "cmd": "",
   "tags": [],
   "group": null,
   "env": {},
   "parent": null,
   "pid": null,
   "attached_to": [],
   "created_at": null,
   "ended_at": 996400.0,
   "artifact_id": null
  },
  {
   "schema_version": 6,
   "id": "n1",
   "name": "dup",
   "role": "llm",
   "state": "exited",
   "cwd": "/r/n1",
   "cmd": "",
   "tags": [],
   "group": "old",
   "env": {},
   "parent": null,
   "pid": null,
   "attached_to": [],
   "created_at": 100,
   "ended_at": 150,
   "engine": "claude",
   "last_activity": null,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "n2",
   "name": "dup",
   "role": "llm",
   "state": "idle",
   "cwd": "/r/n2",
   "cmd": "",
   "tags": [],
   "group": "",
   "env": {},
   "parent": null,
   "pid": null,
   "attached_to": [],
   "created_at": 500,
   "ended_at": null,
   "engine": "claude",
   "last_activity": null,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "n3",
   "name": "dup",
   "role": "llm",
   "state": "idle",
   "cwd": "/r/n3",
   "cmd": "",
   "tags": [],
   "group": "same-new",
   "env": {},
   "parent": null,
   "pid": null,
   "attached_to": [],
   "created_at": 500,
   "ended_at": null,
   "engine": "claude",
   "last_activity": null,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "k1",
   "name": "K",
   "role": "llm",
   "state": "idle",
   "cwd": "/r/k1",
   "cmd": "",
   "tags": [],
   "group": null,
   "env": {},
   "parent": "dup",
   "pid": null,
   "attached_to": [],
   "created_at": 300,
   "ended_at": null,
   "engine": "claude",
   "last_activity": null,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "k2",
   "name": "K2",
   "role": "llm",
   "state": "idle",
   "cwd": "/r/k2",
   "cmd": "",
   "tags": [],
   "group": null,
   "env": {},
   "parent": "dup",
   "pid": null,
   "attached_to": [],
   "created_at": 600,
   "ended_at": null,
   "engine": "claude",
   "last_activity": null,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "k3",
   "name": "K3",
   "role": "llm",
   "state": "idle",
   "cwd": "/r/k3",
   "cmd": "",
   "tags": [],
   "group": null,
   "env": {},
   "parent": "dup",
   "pid": null,
   "attached_to": [],
   "created_at": null,
   "ended_at": null,
   "engine": "claude",
   "last_activity": null,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "p1",
   "name": "P",
   "role": "llm",
   "state": "idle",
   "cwd": "/r/p1",
   "cmd": "",
   "tags": [],
   "group": null,
   "env": {},
   "parent": "q1",
   "pid": null,
   "attached_to": [],
   "created_at": null,
   "ended_at": null,
   "engine": "claude",
   "last_activity": null,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "q1",
   "name": "Q",
   "role": "llm",
   "state": "idle",
   "cwd": "/r/q1",
   "cmd": "",
   "tags": [],
   "group": null,
   "env": {},
   "parent": "p1",
   "pid": null,
   "attached_to": [],
   "created_at": null,
   "ended_at": null,
   "engine": "claude",
   "last_activity": null,
   "chats": [],
   "turn_started_at": null
  },
  {
   "schema_version": 6,
   "id": "m1",
   "name": "",
   "role": "llm",
   "state": "idle",
   "cwd": "/r/m1",
   "cmd": "",
   "tags": [],
   "group": null,
   "env": {},
   "parent": null,
   "pid": null,
   "attached_to": [],
   "created_at": null,
   "ended_at": null,
   "engine": "claude",
   "last_activity": null,
   "chats": [],
   "turn_started_at": null
  }
 ],
 "artifacts": [
  {
   "artifact_schema_version": 2,
   "id": "art1",
   "title": "Rust port plan",
   "filename": "plan.md",
   "created_at": 999100.0,
   "group": null,
   "history": [
    {
     "session_id": "b1",
     "at": 999100.0,
     "rev": 0,
     "changes": null
    },
    {
     "session_id": "user",
     "at": 999700.0,
     "rev": 1,
     "changes": "typo fix"
    },
    {
     "session_id": "b1",
     "at": 999910.0,
     "rev": 2,
     "changes": null
    }
   ]
  },
  {
   "artifact_schema_version": 2,
   "id": "art2",
   "title": "",
   "filename": "a-very-long-file-name-that-overflows.txt",
   "created_at": 999100.0,
   "group": null,
   "history": [
    {
     "session_id": "user",
     "at": 999100.0,
     "rev": 0,
     "changes": null
    },
    {
     "session_id": "gone-1234-5678",
     "at": 999910.0,
     "rev": 1,
     "changes": null
    },
    {
     "session_id": "c1",
     "at": 999910.0,
     "rev": 2,
     "changes": null
    }
   ]
  },
  {
   "artifact_schema_version": 2,
   "id": "art3",
   "title": null,
   "filename": "f.md",
   "created_at": 1000,
   "group": null,
   "history": [
    {
     "session_id": "v2",
     "at": 1000,
     "rev": 0,
     "changes": null
    }
   ]
  },
  {
   "artifact_schema_version": 2,
   "id": "art4",
   "title": null,
   "filename": "f.md",
   "created_at": 1000,
   "group": "g",
   "history": [
    {
     "session_id": "user",
     "at": 1000,
     "rev": 0,
     "changes": null
    },
    {
     "session_id": "gone",
     "at": 1100,
     "rev": 1,
     "changes": ""
    }
   ]
  },
  {
   "artifact_schema_version": 2,
   "id": "art5",
   "title": "日本語タイトルはとても長いのでここで切り詰められるはずです",
   "filename": "f.md",
   "created_at": 999910.0,
   "group": null,
   "history": [
    {
     "session_id": "m1",
     "at": 999910.0,
     "rev": 0,
     "changes": null
    }
   ]
  }
 ],
 "out": {
  "ls": "PROCESSES\n  up                       archived —                         0s    \n  sh                       alive    —                         —      [xxx]\n  assist                   idle     —                         1m     [docs] [é]\n  worker                   idle     work[2] +1                1m     [feat-x] [extra]\n  view                     alive    aaaaaaaaaaaaaaaaaa…[1] +1 —      [artifact]\n  loop                     alive    aaaaaaaaaaaaaaaaaaaa…[12] —     \n  K2                       idle     —                         -     \n  dup                      idle     —                         -     \n  dup                      idle     —                         -     \n  child-with-a-rather-long-name-日本 exited   —                         -     \n  K                        idle     —                         -     \n  dup                      exited   —                         -     \n  tx-assistant             waiting  —                         -     \n  misc                     exited   —                         —     \n  K3                       idle     —                         -     \n  P                        idle     —                         -     \n  Q                        idle     —                         -     \n                           idle     —                         -     ",
  "list12": "up\t\tL\tx1\tup             —                         11d     \u001b[38;2;224;175;104m0s    \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nsh\t [xxx]\tL\ts1\tsh             —                         59s     \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;38mshell\u001b[39m \u001b[38;5;210m[xxx]\u001b[39m\nassist\t [docs] [é]\tL\ta1\tassist         —                         11d     \u001b[38;2;224;175;104m1m    \u001b[39m \u001b[38;5;167mllm  \u001b[39m \u001b[38;5;140m[docs]\u001b[39m \u001b[38;5;73m[é]\u001b[39m\nworker\t [feat-x] [extra]\tL\tb1\tworker         work[2] +1                11d     \u001b[38;2;224;175;104m1m    \u001b[39m \u001b[38;5;167mllm  \u001b[39m \u001b[38;5;198m[feat-x]\u001b[39m \u001b[38;5;38m[extra]\u001b[39m\nview\t [artifact]\tL\tv1\tview           aaaaaaaaaaaaaaaaaa…[1] +1 3d      \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;80mnvim \u001b[39m \u001b[38;5;73m[artifact]\u001b[39m\nloop\t\tL\tv2\tloop           aaaaaaaaaaaaaaaaaaaa…[12] 3d      \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;80mnvim \u001b[39m\nK2\t\tL\tk2\tK2             —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ndup\t\tL\tn2\tdup            —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ndup\t\tL\tn3\tdup            —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nchild-with-a-rather-long-name-日本\t\tL\tc1\tchild-with-…   —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nK\t\tL\tk1\tK              —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ndup\t\tL\tn1\tdup            —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ntx-assistant\t\tL\th1\ttx-assistant   —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nmisc\t\tL\to1\tmisc           —                         -       \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;210mother\u001b[39m\nK3\t\tL\tk3\tK3             —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nP\t\tL\tp1\tP              —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nQ\t\tL\tq1\tQ              —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\n\t\tL\tm1\t               —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m",
  "list0": "up\t\tL\tx1\tu…   —                         11d     \u001b[38;2;224;175;104m0s    \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nsh\t [xxx]\tL\ts1\ts…   —                         59s     \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;38mshell\u001b[39m \u001b[38;5;210m[xxx]\u001b[39m\nassist\t [docs] [é]\tL\ta1\tassis…   —                         11d     \u001b[38;2;224;175;104m1m    \u001b[39m \u001b[38;5;167mllm  \u001b[39m \u001b[38;5;140m[docs]\u001b[39m \u001b[38;5;73m[é]\u001b[39m\nworker\t [feat-x] [extra]\tL\tb1\tworke…   work[2] +1                11d     \u001b[38;2;224;175;104m1m    \u001b[39m \u001b[38;5;167mllm  \u001b[39m \u001b[38;5;198m[feat-x]\u001b[39m \u001b[38;5;38m[extra]\u001b[39m\nview\t [artifact]\tL\tv1\tvie…   aaaaaaaaaaaaaaaaaa…[1] +1 3d      \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;80mnvim \u001b[39m \u001b[38;5;73m[artifact]\u001b[39m\nloop\t\tL\tv2\tloo…   aaaaaaaaaaaaaaaaaaaa…[12] 3d      \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;80mnvim \u001b[39m\nK2\t\tL\tk2\tK…   —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ndup\t\tL\tn2\tdu…   —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ndup\t\tL\tn3\tdu…   —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nchild-with-a-rather-long-name-日本\t\tL\tc1\tchild-with-a-rather-long-name-日…   —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nK\t\tL\tk1\t…   —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ndup\t\tL\tn1\tdu…   —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ntx-assistant\t\tL\th1\ttx-assistan…   —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nmisc\t\tL\to1\tmis…   —                         -       \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;210mother\u001b[39m\nK3\t\tL\tk3\tK…   —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nP\t\tL\tp1\t…   —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nQ\t\tL\tq1\t…   —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\n\t\tL\tm1\t   —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m",
  "list40": "up\t\tL\tx1\tup                                         —                         11d     \u001b[38;2;224;175;104m0s    \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nsh\t [xxx]\tL\ts1\tsh                                         —                         59s     \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;38mshell\u001b[39m \u001b[38;5;210m[xxx]\u001b[39m\nassist\t [docs] [é]\tL\ta1\tassist                                     —                         11d     \u001b[38;2;224;175;104m1m    \u001b[39m \u001b[38;5;167mllm  \u001b[39m \u001b[38;5;140m[docs]\u001b[39m \u001b[38;5;73m[é]\u001b[39m\nworker\t [feat-x] [extra]\tL\tb1\tworker                                     work[2] +1                11d     \u001b[38;2;224;175;104m1m    \u001b[39m \u001b[38;5;167mllm  \u001b[39m \u001b[38;5;198m[feat-x]\u001b[39m \u001b[38;5;38m[extra]\u001b[39m\nview\t [artifact]\tL\tv1\tview                                       aaaaaaaaaaaaaaaaaa…[1] +1 3d      \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;80mnvim \u001b[39m \u001b[38;5;73m[artifact]\u001b[39m\nloop\t\tL\tv2\tloop                                       aaaaaaaaaaaaaaaaaaaa…[12] 3d      \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;80mnvim \u001b[39m\nK2\t\tL\tk2\tK2                                         —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ndup\t\tL\tn2\tdup                                        —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ndup\t\tL\tn3\tdup                                        —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nchild-with-a-rather-long-name-日本\t\tL\tc1\tchild-with-a-rather-long-name-日本           —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nK\t\tL\tk1\tK                                          —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ndup\t\tL\tn1\tdup                                        —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\ntx-assistant\t\tL\th1\ttx-assistant                               —                         11d     \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nmisc\t\tL\to1\tmisc                                       —                         -       \u001b[38;2;224;175;104m—     \u001b[39m \u001b[38;5;210mother\u001b[39m\nK3\t\tL\tk3\tK3                                         —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nP\t\tL\tp1\tP                                          —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\nQ\t\tL\tq1\tQ                                          —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m\n\t\tL\tm1\t                                           —                         -       \u001b[38;2;224;175;104m-     \u001b[39m \u001b[38;5;167mllm  \u001b[39m",
  "history": "HISTORY\n  up                       archived    0s   0c   /r/x1\n  sh                       alive      59s   0c  [xxx]  /o/s1\n  assist                   idle        1m   3c  [docs] [é]  /r/a1\n  worker                   idle        1m   0c  [feat-x] [extra]  /r/b1\n  misc                     exited      1h   0c   /o/o1\n  child-with-a-rather-lon… exited      1h   0c   /r/c1\n  view                     alive       3d   0c  [artifact]  /o/v1\n  loop                     alive       3d   0c   /o/v2\n  K2                       idle       11d   0c   /r/k2\n  dup                      idle       11d   0c   /r/n2\n  dup                      idle       11d   0c   /r/n3\n  K                        idle       11d   0c   /r/k1\n  dup                      exited     11d   0c   /r/n1\n  tx-assistant             waiting    11d   0c   /r/h1\n  K3                       idle         -   0c   /r/k3\n  P                        idle         -   0c   /r/p1\n  Q                        idle         -   0c   /r/q1\n                           idle         -   0c   /r/m1",
  "history_empty": "HISTORY\n  (no exited or archived sessions)",
  "chats": [
   "assist — 3 chat(s)\n  abcdef01  original  spawn               15m ago   —\n  pending   fork      fork←01234567       13m ago   /h/b\n  pending   original  resume                - ago   —",
   "worker — 0 chat(s)\n  (none)",
   "child-with-a-rather-long-name-日本 — 0 chat(s)\n  (none)",
   "tx-assistant — 0 chat(s)\n  (none)",
   "up — 0 chat(s)\n  (none)",
   "view — 0 chat(s)\n  (none)",
   "loop — 0 chat(s)\n  (none)",
   "sh — 0 chat(s)\n  (none)",
   "misc — 0 chat(s)\n  (none)",
   "dup — 0 chat(s)\n  (none)",
   "dup — 0 chat(s)\n  (none)",
   "dup — 0 chat(s)\n  (none)",
   "K — 0 chat(s)\n  (none)",
   "K2 — 0 chat(s)\n  (none)",
   "K3 — 0 chat(s)\n  (none)",
   "P — 0 chat(s)\n  (none)",
   "Q — 0 chat(s)\n  (none)",
   " — 0 chat(s)\n  (none)"
  ],
  "artifacts": "ARTIFACTS\n  art1        3r     1m  Rust port plan               [worker] [user]\n  art2        3r     1m  a-very-long-file-name-that-… [user] [gone-123] [child-with-a-rather-long-name-日本]\n  art5        1r     1m  日本語タイトルはとても長いのでここで切り詰められるはず… [m1]\n  art4        2r    11d  f.md                         [user] [gone]\n  art3        1r    11d  f.md                         [loop]",
  "artifacts_empty": "ARTIFACTS\n  (none)",
  "show": [
   "Rust port plan  (art1)\n  filename:   plan.md\n  created:    15m ago\n  updated:    1m ago\n  group:      derived: feat-x\n  revisions:  3\n  working:    dirty — un-snapshotted edits (close with `tx artifact modify`)\n  history:\n    rev 0     15m ago  worker              \n    rev 1      5m ago  user                  typo fix\n    rev 2      1m ago  worker              ",
   "a-very-long-file-name-that-overflows.txt  (art2)\n  filename:   a-very-long-file-name-that-overflows.txt\n  created:    15m ago\n  updated:    1m ago\n  group:      derived: child-with-a-rather-long-name-日本\n  revisions:  3\n  working:    clean\n  history:\n    rev 0     15m ago  user                \n    rev 1      1m ago  gone-123            \n    rev 2      1m ago  child-with-a-rather-long-name-日本",
   "f.md  (art3)\n  filename:   f.md\n  created:    11d ago\n  updated:    11d ago\n  group:      derived: None\n  revisions:  1\n  working:    dirty — un-snapshotted edits (close with `tx artifact modify`)\n  history:\n    rev 0     11d ago  loop                ",
   "f.md  (art4)\n  filename:   f.md\n  created:    11d ago\n  updated:    11d ago\n  group:      g\n  revisions:  2\n  working:    clean\n  history:\n    rev 0     11d ago  user                \n    rev 1     11d ago  gone                ",
   "日本語タイトルはとても長いのでここで切り詰められるはずです  (art5)\n  filename:   f.md\n  created:    1m ago\n  updated:    1m ago\n  group:      derived: \n  revisions:  1\n  working:    dirty — un-snapshotted edits (close with `tx artifact modify`)\n  history:\n    rev 0      1m ago  m1                  "
  ],
  "session_groups": [
   "docs",
   "feat-x",
   "child-with-a-rather-long-name-日本",
   "hubgrp",
   "upgrp",
   "feat-x",
   "loop",
   "xxx",
   "misc",
   "old",
   "dup",
   "same-new",
   "old",
   "K2",
   "K3",
   "P",
   "Q",
   ""
  ],
  "artifact_groups": [
   "feat-x",
   "child-with-a-rather-long-name-日本",
   "loop",
   "g",
   ""
  ],
  "namew": [
   [
    0,
    -1,
    12
   ],
   [
    0,
    5,
    12
   ],
   [
    0,
    12,
    12
   ],
   [
    0,
    53,
    12
   ],
   [
    0,
    59,
    12
   ],
   [
    0,
    60,
    12
   ],
   [
    0,
    80,
    12
   ],
   [
    60,
    -1,
    12
   ],
   [
    60,
    5,
    12
   ],
   [
    60,
    12,
    12
   ],
   [
    60,
    53,
    12
   ],
   [
    60,
    59,
    12
   ],
   [
    60,
    60,
    12
   ],
   [
    60,
    80,
    12
   ],
   [
    76,
    -1,
    12
   ],
   [
    76,
    5,
    12
   ],
   [
    76,
    12,
    12
   ],
   [
    76,
    53,
    12
   ],
   [
    76,
    59,
    12
   ],
   [
    76,
    60,
    12
   ],
   [
    76,
    80,
    12
   ],
   [
    77,
    -1,
    12
   ],
   [
    77,
    5,
    12
   ],
   [
    77,
    12,
    12
   ],
   [
    77,
    53,
    12
   ],
   [
    77,
    59,
    12
   ],
   [
    77,
    60,
    12
   ],
   [
    77,
    80,
    12
   ],
   [
    118,
    -1,
    12
   ],
   [
    118,
    5,
    12
   ],
   [
    118,
    12,
    12
   ],
   [
    118,
    53,
    53
   ],
   [
    118,
    59,
    53
   ],
   [
    118,
    60,
    53
   ],
   [
    118,
    80,
    53
   ],
   [
    125,
    -1,
    12
   ],
   [
    125,
    5,
    12
   ],
   [
    125,
    12,
    12
   ],
   [
    125,
    53,
    53
   ],
   [
    125,
    59,
    59
   ],
   [
    125,
    60,
    60
   ],
   [
    125,
    80,
    60
   ],
   [
    126,
    -1,
    12
   ],
   [
    126,
    5,
    12
   ],
   [
    126,
    12,
    12
   ],
   [
    126,
    53,
    53
   ],
   [
    126,
    59,
    59
   ],
   [
    126,
    60,
    60
   ],
   [
    126,
    80,
    60
   ],
   [
    200,
    -1,
    12
   ],
   [
    200,
    5,
    12
   ],
   [
    200,
    12,
    12
   ],
   [
    200,
    53,
    53
   ],
   [
    200,
    59,
    59
   ],
   [
    200,
    60,
    60
   ],
   [
    200,
    80,
    60
   ]
  ]
 }
}"#;

    struct Corpus {
        sessions: Vec<Session>,
        artifacts: Vec<Artifact>,
        out: Value,
    }

    fn corpus() -> Corpus {
        let data: Value = serde_json::from_str(CORPUS).unwrap();
        let sessions = data["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| Session::from_value(value).unwrap())
            .collect();
        let artifacts = data["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| Artifact::from_value(value).unwrap())
            .collect();
        Corpus {
            sessions,
            artifacts,
            out: data["out"].clone(),
        }
    }

    fn expected(out: &Value, key: &str) -> String {
        out[key].as_str().unwrap().to_owned()
    }

    #[test]
    fn session_tables_match_python_bytes() {
        let Corpus { sessions, out, .. } = corpus();
        assert_eq!(render_ls(&sessions, NOW), expected(&out, "ls"));
        for namew in [12, 0, 40] {
            assert_eq!(
                picker_display_rows(&sessions, namew, NOW),
                expected(&out, &format!("list{namew}")),
                "namew {namew}"
            );
        }
        assert_eq!(render_history(&sessions, NOW), expected(&out, "history"));
        assert_eq!(render_history(&[], NOW), expected(&out, "history_empty"));
        for (session, want) in sessions.iter().zip(out["chats"].as_array().unwrap()) {
            assert_eq!(render_chats(session, NOW), want.as_str().unwrap());
        }
    }

    #[test]
    fn artifact_tables_match_python_bytes() {
        let Corpus {
            sessions,
            artifacts,
            out,
        } = corpus();
        let names: HashMap<String, String> = sessions
            .iter()
            .map(|session| (session.id.clone(), session.name.clone()))
            .collect();
        let resolver = GroupResolver::new(&sessions, &artifacts);
        assert_eq!(
            render_artifacts(&artifacts, &names, NOW),
            expected(&out, "artifacts")
        );
        assert_eq!(
            render_artifacts(&[], &names, NOW),
            expected(&out, "artifacts_empty")
        );
        let shows = out["show"].as_array().unwrap();
        for (index, (artifact, want)) in artifacts.iter().zip(shows).enumerate() {
            let group = (index != 2).then(|| resolver.artifact_group(artifact));
            let got = render_artifact_show(artifact, index % 2 == 0, &names, NOW, group.as_deref());
            assert_eq!(got, want.as_str().unwrap());
        }
    }

    #[test]
    fn group_resolution_matches_python() {
        let Corpus {
            sessions,
            artifacts,
            out,
        } = corpus();
        let resolver = GroupResolver::new(&sessions, &artifacts);
        let got: Vec<String> = sessions.iter().map(|s| resolver.session_group(s)).collect();
        assert_eq!(serde_json::to_value(got).unwrap(), out["session_groups"]);
        let got: Vec<String> = artifacts
            .iter()
            .map(|a| resolver.artifact_group(a))
            .collect();
        assert_eq!(serde_json::to_value(got).unwrap(), out["artifact_groups"]);
    }

    #[test]
    fn picker_namew_matches_python() {
        let Corpus { out, .. } = corpus();
        for triple in out["namew"].as_array().unwrap() {
            let [cols, longest, want] = [0, 1, 2].map(|i| triple[i].as_i64().unwrap());
            assert_eq!(picker_namew(cols, longest), want, "{cols} {longest}");
        }
    }

    /// The committed port-tests goldens, when the reference checkout sits beside this repo.
    fn golden(name: &str) -> Option<String> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../tx-ide/port-tests/golden/render")
            .join(format!("{name}.txt"));
        match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(_) => {
                eprintln!("skipping golden {name}: {} not found", path.display());
                None
            }
        }
    }

    fn record(base: Value, extra: Value) -> Session {
        let mut record = base;
        for (key, value) in extra.as_object().unwrap() {
            record[key] = value.clone();
        }
        Session::from_value(&record).unwrap()
    }

    fn llm(id: &str, name: &str, extra: Value) -> Session {
        let base = serde_json::json!({
            "schema_version": 6, "id": id, "name": name, "role": "llm", "state": "idle",
            "cwd": "/r", "cmd": "", "tags": [], "group": null, "env": {}, "parent": null,
            "pid": null, "attached_to": [], "created_at": null, "ended_at": null,
            "engine": "claude", "last_activity": null, "chats": [], "turn_started_at": null,
        });
        record(base, extra)
    }

    fn other(id: &str, name: &str, extra: Value) -> Session {
        let base = serde_json::json!({
            "schema_version": 6, "id": id, "name": name, "role": "nvim", "state": "alive",
            "cwd": "/r", "cmd": "", "tags": [], "group": null, "env": {}, "parent": null,
            "pid": null, "attached_to": [], "created_at": null, "ended_at": null,
            "artifact_id": null,
        });
        record(base, extra)
    }

    #[test]
    fn committed_goldens_reproduce() {
        use serde_json::json;
        let now = NOW;
        let render03 = [
            llm(
                "w1",
                "worker-1",
                json!({"state": "waiting", "tags": ["docs"], "created_at": now - 5400.0, "last_activity": now - 90.0}),
            ),
            other("ed", "ed", json!({"created_at": now - 5400.0})),
        ];
        let cases: Vec<(&str, String)> = vec![
            ("03", render_ls(&render03, now)),
            (
                "06",
                picker_display_rows(&render03[..1], 12, now).replace("\tw1\t", "\t0f3a-id\t"),
            ),
            (
                "07",
                picker_display_rows(
                    &[other(
                        "a-very-long-id",
                        "a-very-long-session-name",
                        json!({"created_at": now - 90.0}),
                    )],
                    12,
                    now,
                ),
            ),
        ];
        let chat =
            |id: Value, role: &str, how: &str, parent: Value, bundle: Value, started: f64| {
                json!({"id": id, "role": role, "cwd": "/r", "transcript_path": "/t",
                   "origin": {"how": how, "session_id": "3", "chat_id": parent},
                   "bundle_path": bundle, "started_at": started, "ended_at": null,
                   "summary": "", "engine": null})
            };
        let history = [
            llm(
                "ex",
                &"x".repeat(30),
                json!({
                    "state": "exited", "tags": ["a", "b"], "created_at": now - 2000.0,
                    "ended_at": now - 600.0, "last_activity": now - 700.0,
                    "chats": [
                        chat(json!("abcdef0123456789"), "original", "spawn", Value::Null, Value::Null, now - 900.0),
                        chat(Value::Null, "fork", "fork", json!("0123456789abcdef"), json!("/h/b"), now - 780.0),
                    ],
                }),
            ),
            other(
                "ar",
                "ed",
                json!({"state": "archived", "cwd": "/repo", "created_at": now - 90.0}),
            ),
        ];
        let mut cases = cases;
        cases.push(("09", render_history(&history, now)));
        cases.push(("10", render_chats(&history[0], now)));

        let touch = |sid: &str, at: f64, rev: u64, changes: Value| json!({"session_id": sid, "at": at, "rev": rev, "changes": changes});
        let artifacts = [
            Artifact::from_value(&json!({
                "artifact_schema_version": 2, "id": "abcdef01-2345-4678-8abc-000000000001",
                "title": "Rust port plan", "filename": "plan.md", "created_at": now - 900.0,
                "group": null, "history": [
                    touch("sess-aaaaaaaa-1", now - 900.0, 0, Value::Null),
                    touch("user", now - 300.0, 1, json!("typo fix")),
                    touch("sess-aaaaaaaa-1", now - 90.0, 2, Value::Null),
                ],
            }))
            .unwrap(),
            Artifact::from_value(&json!({
                "artifact_schema_version": 2, "id": "ffffffff-0000-4000-8000-000000000002",
                "title": null, "filename": "a-very-long-file-name-that-overflows.txt",
                "created_at": now - 900.0, "group": "g1",
                "history": [touch("gone-1234-5678", now - 900.0, 0, Value::Null)],
            }))
            .unwrap(),
        ];
        let names = HashMap::from([("sess-aaaaaaaa-1".to_owned(), "worker-1".to_owned())]);
        cases.push(("12", render_artifacts(&artifacts, &names, now)));
        cases.push((
            "13",
            render_artifact_show(&artifacts[0], true, &names, now, Some("derived-g")),
        ));
        for (name, got) in cases {
            if let Some(want) = golden(name) {
                assert_eq!(got + "\n", want, "golden render/{name}");
            }
        }
    }

    fn location(window: &str, pane: &str) -> Location {
        Location {
            host: "Views".into(),
            window_index: "1".into(),
            window_name: window.into(),
            pane_id: "%1".into(),
            pane_index: pane.into(),
        }
    }

    #[test]
    fn location_text_cells() {
        // T-RENDER-02's cells.
        let a = |n: usize| "a".repeat(n);
        assert_eq!(location_text(&[]), "—");
        assert_eq!(location_text(&[location("work", "1")]), "work[1]");
        let three = [
            location("work", "2"),
            location("x", "1"),
            location("y", "1"),
        ];
        assert_eq!(location_text(&three), "work[2] +2");
        assert_eq!(location_text(&[location(&a(22), "1")]), a(22) + "[1]");
        assert_eq!(location_text(&[location(&a(23), "1")]), a(21) + "…[1]");
        let two = [location(&a(30), "1"), location("b", "1")];
        assert_eq!(location_text(&two), a(18) + "…[1] +1");
        // A tail wider than the cell: Python's negative slice end.
        let pane = "9".repeat(30);
        assert_eq!(
            location_text(&[location("abcdef", &pane)]),
            format!("…[{pane}]")
        );
        assert_eq!(
            location_text(&[location("abcdefghijkl", &pane)]),
            format!("abcd…[{pane}]")
        );
    }

    #[test]
    fn reltime_boundaries() {
        let at = |epoch: f64| reltime(Number::from_f64(epoch).as_ref(), NOW);
        assert_eq!(reltime(None, NOW), "-");
        assert_eq!(reltime(Some(&Number::from(0)), NOW), "-");
        assert_eq!(at(NOW + 5.0), "0s");
        assert_eq!(at(NOW - 59.9), "59s");
        assert_eq!(at(NOW - 60.0), "1m");
        assert_eq!(at(NOW - 3599.0), "59m");
        assert_eq!(at(NOW - 3600.0), "1h");
        assert_eq!(at(NOW - 86399.0), "23h");
        assert_eq!(at(NOW - 86400.0), "1d");
    }
}
