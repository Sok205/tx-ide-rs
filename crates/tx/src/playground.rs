//! The playground companion: a real interactive zsh beside every worker, in the worker's
//! worktree, that replays each Bash command the agent runs — prompt line, output, trailer — as if
//! the agent had typed it there, and pushes the command into the shell's history so the operator
//! can recall, edit and rerun it. The agent's commands still execute inside the engine's own tool
//! (completion and output capture stay the engine's); the companion only *mirrors* them.
//!
//! The service spawns the companion right after the worker. `tx hook working` appends each Bash
//! hook payload to the worker's durable log and, while the companion lives, hands the entry to
//! its shell: NUL-separated records in a pending file + `SIGUSR1`, drained by the rc in
//! `playground/zshrc`. A worker without a log (spawned before the companion existed) records
//! nothing.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session::{Role, Session};
use crate::spawn::SpawnSpec;
use crate::storage::Home;

pub const COMPANION_SUFFIX: &str = "-playground";
/// Output lines kept per command; the rest is summarised in one line.
pub const OUTPUT_LINE_LIMIT: usize = 200;
/// The companion shell's env: where the hook leaves records for it.
pub const PENDING_ENV: &str = "TX_PLAYGROUND_PENDING";
pub const ZDOTDIR_ENV: &str = "ZDOTDIR";
const ZDOTDIR_NAME: &str = "zdotdir";
const RC_FILES: [(&str, &str); 2] = [
    (".zshenv", include_str!("playground/zshenv")),
    (".zshrc", include_str!("playground/zshrc")),
];
/// A pending record that is a history entry rather than text to print.
const HISTORY_MARK: char = '\x01';

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub fn log_path(home: &Home, worker_id: &str) -> PathBuf {
    home.playground_dir().join(format!("{worker_id}.log"))
}

pub fn pending_path(home: &Home, worker_id: &str) -> PathBuf {
    home.playground_dir().join(format!("{worker_id}.pending"))
}

pub fn companion_name(worker_name: &str) -> String {
    format!("{worker_name}{COMPANION_SUFFIX}")
}

/// The worker's live companion among `sessions`: named for it, spawned by it, not ended.
pub fn companion_of<'a>(worker: &Session, sessions: &'a [Session]) -> Option<&'a Session> {
    let name = companion_name(&worker.name);
    sessions.iter().find(|session| {
        session.name == name && session.parent.as_deref() == Some(&worker.id) && session.is_alive()
    })
}

/// Create the worker's log and pending file, materialise the shell rc, and build the companion's
/// spawn spec: an interactive zsh, same tags, cwd = the worker's worktree, parent = the worker.
pub fn prepare_companion(home: &Home, worker: &Session) -> io::Result<SpawnSpec> {
    let directory = home.playground_dir();
    let zdotdir = directory.join(ZDOTDIR_NAME);
    std::fs::create_dir_all(&zdotdir)?;
    for (name, contents) in RC_FILES {
        std::fs::write(zdotdir.join(name), contents)?;
    }
    std::fs::write(
        log_path(home, &worker.id),
        format!(
            "{DIM}# playground of {} — every Bash command the agent runs, mirrored{RESET}\n",
            worker.name
        ),
    )?;
    let pending = pending_path(home, &worker.id);
    std::fs::write(&pending, "")?;
    let mut spec = SpawnSpec::for_role(
        companion_name(&worker.name),
        Role::Shell,
        worker.tags.clone(),
        worker.cwd.clone(),
        "zsh",
    );
    spec.parent = Some(worker.id.clone());
    spec.env = vec![
        (
            ZDOTDIR_ENV.to_owned(),
            zdotdir.to_string_lossy().into_owned(),
        ),
        (
            PENDING_ENV.to_owned(),
            pending.to_string_lossy().into_owned(),
        ),
    ];
    Ok(spec)
}

/// Mirror one tool-hook payload: append its entry to the worker's log and, when the companion
/// lives, hand it to the shell. Nothing for a tool other than Bash or an event other than the
/// three Bash hooks carry. Neither file is created here: no log means no companion to feed.
pub fn mirror(
    home: &Home,
    worker: &Session,
    companion: Option<&Session>,
    payload: &Value,
) -> io::Result<()> {
    let Some(entry) = entry_for(payload) else {
        return Ok(());
    };
    append(&log_path(home, &worker.id), entry.text.as_bytes())?;
    let Some(companion) = companion else {
        return Ok(());
    };
    let mut records = Vec::new();
    records.extend_from_slice(entry.text.as_bytes());
    records.push(0);
    if let Some(command) = entry.command {
        records.push(HISTORY_MARK as u8);
        records.extend_from_slice(command.as_bytes());
        records.push(0);
    }
    append(&pending_path(home, &worker.id), &records)?;
    if let Some(pid) = companion.pid.as_ref().and_then(serde_json::Number::as_i64) {
        // SAFETY: a plain signal send; a vanished pid (or a reused one) only yields an error or
        // a stray SIGUSR1 to a process that ignores it by default.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGUSR1);
        }
    }
    Ok(())
}

fn append(path: &Path, bytes: &[u8]) -> io::Result<()> {
    OpenOptions::new().append(true).open(path)?.write_all(bytes)
}

/// One log entry: the text, plus the command itself when the entry opens a command (for the
/// companion's history).
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    pub text: String,
    pub command: Option<String>,
}

/// The entry a payload describes: the prompt line on `PreToolUse`, the output plus a trailer on
/// `PostToolUse` / `PostToolUseFailure`.
pub fn entry_for(payload: &Value) -> Option<Entry> {
    if text(payload, &["tool_name"]) != Some("Bash") {
        return None;
    }
    let event = text(payload, &["hook_event_name"])?;
    match event {
        "PreToolUse" => Some(prompt_entry(payload)),
        "PostToolUse" => {
            let output = text(payload, &["tool_response", "stdout"]).unwrap_or("");
            let interrupted = payload
                .pointer("/tool_response/interrupted")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let status = if interrupted { "interrupted" } else { "ok" };
            Some(result_entry(output, status, duration(payload)))
        }
        "PostToolUseFailure" => {
            let error = text(payload, &["error"]).unwrap_or("");
            let (status, output) = split_failure(error);
            Some(result_entry(output, &status, duration(payload)))
        }
        _ => None,
    }
}

fn prompt_entry(payload: &Value) -> Entry {
    let mut line = String::new();
    if let Some(description) = text(payload, &["tool_input", "description"]) {
        line.push_str(&format!("{DIM}# {description}{RESET}\n"));
    }
    let directory = text(payload, &["cwd"])
        .map(Path::new)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let command = text(payload, &["tool_input", "command"]).unwrap_or("");
    line.push_str(&format!("{BOLD}{directory} $ {command}{RESET}\n"));
    Entry {
        text: line,
        command: Some(command.to_owned()),
    }
}

fn result_entry(output: &str, status: &str, duration: Option<u64>) -> Entry {
    let mut entry = String::new();
    let lines: Vec<&str> = output.lines().collect();
    for line in lines.iter().take(OUTPUT_LINE_LIMIT) {
        entry.push_str(line);
        entry.push('\n');
    }
    if lines.len() > OUTPUT_LINE_LIMIT {
        entry.push_str(&format!(
            "{DIM}… {} more lines not shown{RESET}\n",
            lines.len() - OUTPUT_LINE_LIMIT
        ));
    }
    let timing = duration.map(format_duration).unwrap_or_default();
    entry.push_str(&format!("{DIM}[{status}{timing}]{RESET}\n\n"));
    Entry {
        text: entry,
        command: None,
    }
}

/// `"Exit code 3\n<output>"` → (`"exit 3"`, output); anything else is the whole text as output.
fn split_failure(error: &str) -> (String, &str) {
    let (first, rest) = error.split_once('\n').unwrap_or((error, ""));
    match first.strip_prefix("Exit code ") {
        Some(code) => (format!("exit {code}"), rest),
        None => ("failed".to_owned(), error),
    }
}

fn duration(payload: &Value) -> Option<u64> {
    payload.get("duration_ms").and_then(Value::as_u64)
}

fn format_duration(milliseconds: u64) -> String {
    if milliseconds < 1000 {
        format!(" · {milliseconds}ms")
    } else {
        format!(" · {:.1}s", milliseconds as f64 / 1000.0)
    }
}

fn text<'a>(payload: &'a Value, path: &[&str]) -> Option<&'a str> {
    path.iter()
        .try_fold(payload, |value, key| value.get(key))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bash(event: &str, extra: Value) -> Value {
        let mut payload = json!({
            "hook_event_name": event,
            "tool_name": "Bash",
            "cwd": "/home/u/.tx-ide/worktrees/repo/repo--feature",
            "tool_input": {"command": "cargo test", "description": "Run the tests"},
        });
        for (key, value) in extra.as_object().unwrap() {
            payload[key] = value.clone();
        }
        payload
    }

    #[test]
    fn prompt_line_carries_description_directory_and_command() {
        let entry = entry_for(&bash("PreToolUse", json!({}))).unwrap();
        assert_eq!(
            entry,
            Entry {
                text: format!(
                    "{DIM}# Run the tests{RESET}\n{BOLD}repo--feature $ cargo test{RESET}\n"
                ),
                command: Some("cargo test".into()),
            }
        );
    }

    #[test]
    fn success_shows_output_and_a_timed_trailer() {
        let entry = entry_for(&bash(
            "PostToolUse",
            json!({"tool_response": {"stdout": "a\nb", "interrupted": false}, "duration_ms": 347}),
        ))
        .unwrap();
        assert_eq!(entry.text, format!("a\nb\n{DIM}[ok · 347ms]{RESET}\n\n"));
        assert_eq!(entry.command, None);
    }

    #[test]
    fn failure_splits_the_exit_code_off_the_error_text() {
        let entry = entry_for(&bash(
            "PostToolUseFailure",
            json!({"error": "Exit code 3\nbefore\nls: no such file", "duration_ms": 1021}),
        ))
        .unwrap();
        assert_eq!(
            entry.text,
            format!("before\nls: no such file\n{DIM}[exit 3 · 1.0s]{RESET}\n\n")
        );
        let entry = entry_for(&bash("PostToolUseFailure", json!({"error": "boom"}))).unwrap();
        assert_eq!(entry.text, format!("boom\n{DIM}[failed]{RESET}\n\n"));
    }

    #[test]
    fn long_output_is_capped_with_a_count() {
        let output = (0..OUTPUT_LINE_LIMIT + 5)
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let entry = entry_for(&bash(
            "PostToolUse",
            json!({"tool_response": {"stdout": output}}),
        ))
        .unwrap();
        assert!(entry.text.contains(&format!(
            "\n{}\n{DIM}… 5 more lines not shown",
            OUTPUT_LINE_LIMIT - 1
        )));
        assert!(!entry.text.contains(&format!("\n{}\n", OUTPUT_LINE_LIMIT)));
        assert!(entry.text.ends_with(&format!("{DIM}[ok]{RESET}\n\n")));
    }

    #[test]
    fn other_tools_and_events_are_ignored() {
        let mut payload = bash("PreToolUse", json!({}));
        payload["tool_name"] = json!("Read");
        assert_eq!(entry_for(&payload), None);
        assert_eq!(entry_for(&bash("SubagentStart", json!({}))), None);
        assert_eq!(entry_for(&json!({})), None);
    }

    #[test]
    fn companion_name_suffixes_the_worker() {
        assert_eq!(companion_name("auth-review"), "auth-review-playground");
    }
}
