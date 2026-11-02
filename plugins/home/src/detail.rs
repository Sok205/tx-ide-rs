//! What an unfolded agent row shows, read on demand: the transcript's model, effort and last
//! exchange, the artifacts the agent touched. Every failure degrades to "nothing known".

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde_json::Value;
use tx_chat::transcript::metadata::Metadata;
use tx_chat::transcript::rows::{Kind, cap, parse_entry};
use tx_core::{Agent, LlmEngineState, TxCore, VerbIo};

use crate::model::Detail;
use crate::project::ProjectCache;

const LINE_CAP: usize = 120;
const ARTIFACT_VERB: &str = "artifact";

#[derive(Default)]
struct Scan {
    last_user: Option<String>,
    last_agent: Option<String>,
    chats: usize,
    metadata: Metadata,
}

fn scan(transcript: &Path) -> Scan {
    let mut scan = Scan::default();
    let Ok(bytes) = std::fs::read(transcript) else {
        return scan;
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut rows = Vec::new();
    for line in text.lines() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        scan.metadata.update(&entry);
        parse_entry(&entry, &mut rows);
    }
    scan.chats = rows.iter().filter(|row| row.kind.is_message()).count();
    let newest = |kind: Kind| {
        rows.iter()
            .rev()
            .filter(|row| row.kind == kind)
            .find_map(|row| first_line(&row.text))
    };
    scan.last_user = newest(Kind::User);
    scan.last_agent = newest(Kind::Agent);
    scan
}

/// The first non-empty line, split at any line break, cut to [`LINE_CAP`].
fn first_line(text: &str) -> Option<String> {
    text.split(['\n', '\r', '\u{b}', '\u{c}', '\u{2028}', '\u{2029}'])
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| cap(line, LINE_CAP))
}

/// The newest thing the human typed and the newest line the agent said, each cut to its first
/// line; `(None, None)` for a missing or empty transcript.
pub fn last_exchange(transcript: &Path) -> (Option<String>, Option<String>) {
    let scan = scan(transcript);
    (scan.last_user, scan.last_agent)
}

/// `tx artifact ls --session <id>` run in-process, counted by its kind column (the fourth) on lines shaped like an artifact row;
/// empty when the verb is missing or fails.
pub fn artifact_counts(core: &TxCore, id: &str) -> Vec<(String, usize)> {
    let Some(verb) = core.verb(ARTIFACT_VERB) else {
        return Vec::new();
    };
    let mut stdin = io::empty();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let argv = ["ls", "--session", id].map(str::to_owned);
    let result = verb.run(
        core,
        &mut VerbIo {
            stdin: &mut stdin,
            stdout: &mut stdout,
            stderr: &mut stderr,
        },
        &argv,
    );
    if !matches!(result, Ok(0)) {
        return Vec::new();
    }
    count_kinds(&String::from_utf8_lossy(&stdout))
}

/// The `ls` listing is a header line, then `id  Nr  age  kind  name  authors` per artifact;
/// `ARTIFACTS\n  (none)` when empty.
fn count_kinds(listing: &str) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for line in listing.lines() {
        let mut columns = line.split_whitespace().skip(1);
        let is_row = columns.next().is_some_and(|revisions| {
            revisions
                .strip_suffix('r')
                .is_some_and(|count| !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
        });
        if let (true, Some(kind)) = (is_row, columns.nth(1)) {
            *counts.entry(kind.to_owned()).or_default() += 1;
        }
    }
    counts.into_iter().collect()
}

/// The detail of one agent. Records the agent's message count in `cache` for its row.
pub fn detail(core: &TxCore, cache: &mut ProjectCache, agent: &Agent) -> Detail {
    let transcript = core
        .conversation(&agent.id)
        .ok()
        .flatten()
        .map(|conversation| scan(&conversation.transcript_path))
        .unwrap_or_default();
    cache.record_chats(agent.id.as_str(), transcript.chats);
    let option = |key: &str| {
        agent
            .ext
            .get(agent.engine.as_str())?
            .get(key)?
            .as_str()
            .map(str::to_owned)
    };
    let metadata = transcript
        .metadata
        .with_launch_fallback(option("model").as_deref(), option("effort").as_deref());
    let ended_at = (agent.state == LlmEngineState::Ended)
        .then(|| agent.last_activity_at.unwrap_or(agent.last_modified_at));
    Detail {
        roles: agent.roles.clone(),
        model: metadata.model,
        effort: metadata.effort,
        parent_name: agent
            .parent_id
            .as_ref()
            .and_then(|parent| core.get_agent(parent).ok())
            .map(|parent| parent.name),
        last_user: transcript.last_user,
        last_agent: transcript.last_agent,
        artifacts: artifact_counts(core, agent.id.as_str()),
        started_at: agent.created_at,
        ended_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(lines: &[&str]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), lines.join("\n")).unwrap();
        file
    }

    #[test]
    fn last_exchange_reads_the_newest_user_and_agent_lines() {
        let file = transcript(&[
            r#"{"type":"user","timestamp":"2026-10-11T08:00:00Z","message":{"content":"first question"}}"#,
            r#"{"type":"assistant","timestamp":"2026-10-11T08:00:01Z","message":{"model":"claude-opus-4-7","content":[{"type":"text","text":"first answer"}]}}"#,
            r#"not json"#,
            r#"{"type":"user","timestamp":"2026-10-11T08:01:00Z","message":{"content":"second question\nwith a second line"}}"#,
            r#"{"type":"assistant","timestamp":"2026-10-11T08:01:01Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-10-11T08:01:02Z","message":{"content":[{"type":"text","text":"second answer\nmore"}]}}"#,
        ]);
        assert_eq!(
            last_exchange(file.path()),
            (
                Some("second question".to_owned()),
                Some("second answer".to_owned())
            )
        );
        let scan = scan(file.path());
        assert_eq!(scan.chats, 4);
        assert_eq!(scan.metadata.model.as_deref(), Some("claude-opus-4-7"));
    }

    #[test]
    fn a_long_line_is_capped() {
        let long = "x".repeat(300);
        let line = format!(
            r#"{{"type":"user","timestamp":"2026-10-11T08:00:00Z","message":{{"content":"{long}"}}}}"#
        );
        let (user, _) = last_exchange(transcript(&[&line]).path());
        assert_eq!(user.unwrap(), cap(&long, LINE_CAP));
    }

    #[test]
    fn artifact_kinds_are_counted_from_the_fourth_column() {
        let listing = "ARTIFACTS\n  1a2b3c4d    2r     3m  diff      Refactor the thing          [me]\n  \
                       5e6f7a8b    1r     1h  document  Plan                        \n  \
                       9c0d1e2f   12r     2d  diff      Another one  (archived)\n";
        assert_eq!(
            count_kinds(listing),
            [("diff".to_owned(), 2), ("document".to_owned(), 1)]
        );
        assert!(count_kinds("ARTIFACTS\n  (none)").is_empty());
        assert!(count_kinds("").is_empty());
    }

    #[test]
    fn a_screenshot_paste_is_one_message_with_the_typed_text_first() {
        let file = transcript(&[
            r#"{"type":"user","timestamp":"2026-10-11T08:00:00Z","message":{"content":"look at this"}}"#,
            r#"{"type":"user","timestamp":"2026-10-11T08:00:00Z","message":{"content":[{"type":"text","text":"[Image: source: x]"}]}}"#,
        ]);
        assert_eq!(
            last_exchange(file.path()).0.as_deref(),
            Some("look at this")
        );
        assert_eq!(scan(file.path()).chats, 1);
    }

    #[test]
    fn the_first_line_is_the_first_non_empty_one() {
        assert_eq!(first_line("\n\nsecond").as_deref(), Some("second"));
        assert_eq!(first_line("\n \n"), None);
    }

    #[test]
    fn a_line_with_an_invalid_byte_does_not_discard_the_transcript() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut bytes = br#"{"type":"user","timestamp":"2026-10-11T08:00:00Z","message":{"content":"question"}}"#.to_vec();
        bytes.extend(b"\n\xff\xfe broken\n");
        bytes.extend(br#"{"type":"assistant","timestamp":"2026-10-11T08:00:01Z","message":{"content":[{"type":"text","text":"answer"}]}}"#);
        std::fs::write(file.path(), bytes).unwrap();
        assert_eq!(
            last_exchange(file.path()),
            (Some("question".to_owned()), Some("answer".to_owned()))
        );
    }

    #[test]
    fn every_line_break_ends_the_first_line() {
        for separator in ["\r", "\u{2028}", "\u{2029}", "\x0b", "\x0c", "\r\n", "\n"] {
            assert_eq!(
                first_line(&format!("ab{separator}cd")).as_deref(),
                Some("ab"),
                "{separator:?}"
            );
        }
    }

    #[test]
    fn only_artifact_rows_are_counted() {
        let listing = "ARTIFACTS\n  1a2b3c4d    2r     3m  diff      A title with\na newline in it  [me]\n  \
                       5e6f7a8b    1r     1h  document  Plan\n\
                       error: could not read the store\n\
                       tx artifact: no such session, really\n";
        assert_eq!(
            count_kinds(listing),
            [("diff".to_owned(), 1), ("document".to_owned(), 1)]
        );
    }

    #[test]
    fn a_missing_transcript_gives_none_none() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            last_exchange(&directory.path().join("absent.jsonl")),
            (None, None)
        );
    }

    #[test]
    fn an_empty_transcript_gives_none_none() {
        assert_eq!(last_exchange(transcript(&[]).path()), (None, None));
    }
}
