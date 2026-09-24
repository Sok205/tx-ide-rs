//! Codex rollout reader (codex_rollout.py). Records are `{type, timestamp, payload}`; the first
//! line is always the `session_meta`.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::session::{py_repr, py_repr_str};

pub const SESSION_META_TYPE: &str = "session_meta";
/// A conversation turn (an OpenAI Responses item); other payload types are skipped.
pub const MESSAGE_TYPE: &str = "message";
pub const MESSAGE_ROLES: [&str; 3] = ["developer", "user", "assistant"];
/// user/developer turns hold text in input_text, assistant turns in output_text.
pub const TEXT_BLOCK_TYPES: [&str; 2] = ["input_text", "output_text"];
/// Codex injects the first user turn as an `<environment_context>` block.
pub const ENVIRONMENT_CONTEXT_TAG: &str = "<environment_context>";

/// A rollout could not be read. Texts follow the reference's `ValueError` / `KeyError`.
#[derive(Debug, thiserror::Error)]
pub enum RolloutError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{}: invalid JSON: {source}", path.display())]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{}: first record is {found}, expected {}", path.display(), py_repr_str(SESSION_META_TYPE))]
    NotSessionMeta { path: PathBuf, found: String },
    #[error("{}: empty rollout — no {SESSION_META_TYPE} record", path.display())]
    Empty { path: PathBuf },
    /// A record lacks a key (or is not an object where one is indexed).
    #[error("{}: malformed record: missing {}", path.display(), py_repr_str(.key))]
    Malformed { path: PathBuf, key: &'static str },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexMessage {
    pub role: String,
    pub text: String,
    pub is_environment_context: bool,
}

/// The first-line `session_meta` payload (session id, cwd, and on a fork the forked_from_id).
pub fn read_session_meta(path: &Path) -> Result<Value, RolloutError> {
    let Some(record) = records(path)?.into_iter().next() else {
        return Err(RolloutError::Empty {
            path: path.to_owned(),
        });
    };
    let kind = field(path, &record, "type")?;
    if kind.as_str() != Some(SESSION_META_TYPE) {
        return Err(RolloutError::NotSessionMeta {
            path: path.to_owned(),
            found: py_repr(kind),
        });
    }
    Ok(field(path, &record, "payload")?.clone())
}

/// The conversation turns in order; the injected first-user `<environment_context>` is flagged,
/// not dropped.
pub fn iter_messages(path: &Path) -> Result<Vec<CodexMessage>, RolloutError> {
    let mut messages = Vec::new();
    for record in records(path)? {
        let payload = field(path, &record, "payload")?;
        let Some(payload) = payload.as_object() else {
            return Err(malformed(path, "type"));
        };
        if payload.get("type").and_then(Value::as_str) != Some(MESSAGE_TYPE) {
            continue;
        }
        let role = payload.get("role").ok_or_else(|| malformed(path, "role"))?;
        let Some(role) = role.as_str().filter(|role| MESSAGE_ROLES.contains(role)) else {
            continue;
        };
        let content = payload
            .get("content")
            .ok_or_else(|| malformed(path, "content"))?;
        let text = message_text(path, content)?;
        let is_environment_context =
            role == "user" && py_lstrip(&text).starts_with(ENVIRONMENT_CONTEXT_TAG);
        messages.push(CodexMessage {
            role: role.to_owned(),
            text,
            is_environment_context,
        });
    }
    Ok(messages)
}

fn records(path: &Path) -> Result<Vec<Value>, RolloutError> {
    let text = std::fs::read_to_string(path).map_err(|source| RolloutError::Io {
        path: path.to_owned(),
        source,
    })?;
    text.split_inclusive('\n')
        .map(py_strip)
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_str(line).map_err(|source| RolloutError::Json {
                path: path.to_owned(),
                source,
            })
        })
        .collect()
}

fn field<'a>(path: &Path, record: &'a Value, key: &'static str) -> Result<&'a Value, RolloutError> {
    record.get(key).ok_or_else(|| malformed(path, key))
}

fn malformed(path: &Path, key: &'static str) -> RolloutError {
    RolloutError::Malformed {
        path: path.to_owned(),
        key,
    }
}

fn message_text(path: &Path, content: &Value) -> Result<String, RolloutError> {
    let blocks = content
        .as_array()
        .ok_or_else(|| malformed(path, "content"))?;
    let mut text = String::new();
    for block in blocks {
        let block: &Map<String, Value> =
            block.as_object().ok_or_else(|| malformed(path, "type"))?;
        let kind = block.get("type").ok_or_else(|| malformed(path, "type"))?;
        if kind
            .as_str()
            .is_some_and(|kind| TEXT_BLOCK_TYPES.contains(&kind))
        {
            let part = block.get("text").ok_or_else(|| malformed(path, "text"))?;
            text.push_str(part.as_str().ok_or_else(|| malformed(path, "text"))?);
        }
    }
    Ok(text)
}

/// `str.strip()` (Python whitespace).
fn py_strip(line: &str) -> &str {
    line.trim_matches(is_py_space)
}

fn py_lstrip(text: &str) -> &str {
    text.trim_start_matches(is_py_space)
}

fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rollout(text: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    const ROLLOUT: &str = concat!(
        r#"{"type":"session_meta","timestamp":"t","payload":{"id":"r1","cwd":"/w"}}"#,
        "\n\n",
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"  <environment_context>cwd</environment_context>"}]}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"message","role":"system","content":[]}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi "},{"type":"input_image","url":"x"},{"type":"input_text","text":"there"}]}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"<environment_context>"}]}}"#,
        "\n",
    );

    #[test]
    fn session_meta_is_the_first_record_payload() {
        let (_dir, path) = rollout(ROLLOUT);
        assert_eq!(
            read_session_meta(&path).unwrap(),
            serde_json::json!({"id": "r1", "cwd": "/w"})
        );
    }

    #[test]
    fn session_meta_errors_match_the_reference_text() {
        let (_dir, path) = rollout("{\"type\":\"x\",\"payload\":{}}\n");
        assert_eq!(
            read_session_meta(&path).unwrap_err().to_string(),
            format!(
                "{}: first record is 'x', expected 'session_meta'",
                path.display()
            )
        );
        let (_dir, path) = rollout("\n  \n");
        assert_eq!(
            read_session_meta(&path).unwrap_err().to_string(),
            format!("{}: empty rollout — no session_meta record", path.display())
        );
    }

    #[test]
    fn messages_keep_order_flag_environment_context_and_skip_non_turns() {
        let (_dir, path) = rollout(ROLLOUT);
        let messages = iter_messages(&path).unwrap();
        let summary: Vec<_> = messages
            .iter()
            .map(|m| (m.role.as_str(), m.text.as_str(), m.is_environment_context))
            .collect();
        assert_eq!(
            summary,
            [
                (
                    "user",
                    "  <environment_context>cwd</environment_context>",
                    true
                ),
                ("user", "hi there", false),
                ("assistant", "<environment_context>", false),
            ]
        );
    }

    #[test]
    fn a_message_without_role_is_malformed() {
        let (_dir, path) = rollout("{\"payload\":{\"type\":\"message\"}}\n");
        assert!(matches!(
            iter_messages(&path),
            Err(RolloutError::Malformed { key: "role", .. })
        ));
    }
}
