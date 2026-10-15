//! The playground companion: a shell session beside a worker that replays every Bash command the
//! agent runs, with its output, as if the agent had typed it there. The agent's commands still
//! execute inside the engine's own tool (so completion and output capture stay the engine's);
//! the companion only *mirrors* them, from the engine's tool hooks, into a per-session log that
//! the companion pane tails.
//!
//! Every worker gets one: the service spawns the companion right after the worker, and
//! `tx hook working` appends to the worker's log — a session without a log (spawned before the
//! companion existed) simply records nothing.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session::{Role, Session};
use crate::shlex::shlex_quote;
use crate::spawn::SpawnSpec;
use crate::storage::Home;

pub const COMPANION_SUFFIX: &str = "-playground";
/// Output lines kept per command in the log; the rest is summarised in one line.
pub const OUTPUT_LINE_LIMIT: usize = 200;

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

pub fn log_path(home: &Home, session_id: &str) -> PathBuf {
    home.playground_dir().join(format!("{session_id}.log"))
}

pub fn companion_name(worker_name: &str) -> String {
    format!("{worker_name}{COMPANION_SUFFIX}")
}

/// Create the worker's log (its header line) and the companion's spawn spec: a shell session
/// tailing the log, same tags, cwd = the worker's worktree, parent = the worker.
pub fn prepare_companion(home: &Home, worker: &Session) -> io::Result<SpawnSpec> {
    let log = log_path(home, &worker.id);
    let directory = home.playground_dir();
    std::fs::create_dir_all(&directory)?;
    std::fs::write(
        &log,
        format!(
            "{DIM}# playground of {} — every Bash command the agent runs, mirrored{RESET}\n",
            worker.name
        ),
    )?;
    let cmd = format!(
        "sh -c {}",
        shlex_quote(&format!(
            "exec tail -n +1 -F {}",
            shlex_quote(&log.to_string_lossy())
        ))
    );
    let mut spec = SpawnSpec::for_role(
        companion_name(&worker.name),
        Role::Shell,
        worker.tags.clone(),
        worker.cwd.clone(),
        cmd,
    );
    spec.parent = Some(worker.id.clone());
    Ok(spec)
}

/// Append the entry a tool-hook payload describes — nothing for a tool other than Bash or an
/// event other than the three Bash hooks carry. The log is never created here: a worker without
/// one has no companion to feed.
pub fn record(home: &Home, session_id: &str, payload: &Value) -> io::Result<()> {
    let Some(entry) = entry_for(payload) else {
        return Ok(());
    };
    let path = log_path(home, session_id);
    let mut file = OpenOptions::new().append(true).open(path)?;
    file.write_all(entry.as_bytes())
}

/// The log text for one payload: the prompt line on `PreToolUse`, the output plus a trailer on
/// `PostToolUse` / `PostToolUseFailure`.
pub fn entry_for(payload: &Value) -> Option<String> {
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

fn prompt_entry(payload: &Value) -> String {
    let mut entry = String::new();
    if let Some(description) = text(payload, &["tool_input", "description"]) {
        entry.push_str(&format!("{DIM}# {description}{RESET}\n"));
    }
    let directory = text(payload, &["cwd"])
        .map(Path::new)
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let command = text(payload, &["tool_input", "command"]).unwrap_or("");
    entry.push_str(&format!("{BOLD}{directory} $ {command}{RESET}\n"));
    entry
}

fn result_entry(output: &str, status: &str, duration: Option<u64>) -> String {
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
    entry
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
            format!("{DIM}# Run the tests{RESET}\n{BOLD}repo--feature $ cargo test{RESET}\n")
        );
    }

    #[test]
    fn success_shows_output_and_a_timed_trailer() {
        let entry = entry_for(&bash(
            "PostToolUse",
            json!({"tool_response": {"stdout": "a\nb", "interrupted": false}, "duration_ms": 347}),
        ))
        .unwrap();
        assert_eq!(entry, format!("a\nb\n{DIM}[ok · 347ms]{RESET}\n\n"));
    }

    #[test]
    fn failure_splits_the_exit_code_off_the_error_text() {
        let entry = entry_for(&bash(
            "PostToolUseFailure",
            json!({"error": "Exit code 3\nbefore\nls: no such file", "duration_ms": 1021}),
        ))
        .unwrap();
        assert_eq!(
            entry,
            format!("before\nls: no such file\n{DIM}[exit 3 · 1.0s]{RESET}\n\n")
        );
        let entry = entry_for(&bash("PostToolUseFailure", json!({"error": "boom"}))).unwrap();
        assert_eq!(entry, format!("boom\n{DIM}[failed]{RESET}\n\n"));
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
        assert!(entry.contains(&format!("\n{}\n{DIM}… 5 more lines not shown", OUTPUT_LINE_LIMIT - 1)));
        assert!(!entry.contains(&format!("\n{}\n", OUTPUT_LINE_LIMIT)));
        assert!(entry.ends_with(&format!("{DIM}[ok]{RESET}\n\n")));
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
