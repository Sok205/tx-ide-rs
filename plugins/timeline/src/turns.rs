//! Turn spans: when the agent held the floor, from transcript turn markers rather than row
//! gaps. Claude marks the final message of a turn with `stop_reason: end_turn`; Codex writes
//! `task_started` / `task_complete` / `turn_aborted` events. A span opens at the first entry of
//! a turn and closes at its end marker; an unclosed span is the live turn (or a crash).

use serde_json::Value;

use super::rows::iso_timestamp;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Turns {
    /// Closed spans, epoch seconds, in transcript order.
    pub spans: Vec<(f64, f64)>,
    /// The turn in progress: when it opened.
    pub open: Option<f64>,
    /// The last timestamped entry seen — bounds how far an open span may reach.
    pub last_at: f64,
}

impl Turns {
    pub fn update(&mut self, entry: &Value) {
        if entry["isSidechain"].as_bool() == Some(true) {
            return;
        }
        let Some(at) = iso_timestamp(entry.get("timestamp")) else {
            return;
        };
        self.last_at = self.last_at.max(at);
        let kind = entry["type"].as_str().unwrap_or("");
        let holds = if entry.get("payload").is_some() {
            match (kind, entry["payload"]["type"].as_str()) {
                ("event_msg", Some("task_started")) => true,
                ("event_msg", Some("task_complete" | "turn_aborted")) => false,
                _ => return,
            }
        } else {
            match kind {
                "user" => !user_text(entry)
                    .trim_start()
                    .starts_with("[Request interrupted"),
                "assistant" => {
                    let message = &entry["message"];
                    message["stop_reason"].as_str() != Some("end_turn")
                        && message["model"].as_str() != Some("<synthetic>")
                }
                _ => return,
            }
        };
        match (holds, self.open) {
            (true, None) => self.open = Some(at),
            (false, Some(start)) => {
                self.spans.push((start, at.max(start)));
                self.open = None;
            }
            _ => {}
        }
    }
}

/// The typed text of a Claude `user` entry: a string, or its text blocks joined.
fn user_text(entry: &Value) -> String {
    match &entry["message"]["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn claude(kind: &str, second: u32, extra: Value) -> Value {
        let mut entry = json!({
            "type": kind,
            "timestamp": format!("2026-10-11T08:00:{second:02}Z"),
            "message": {"content": "hi"},
        });
        for (key, value) in extra["message"].as_object().into_iter().flatten() {
            entry["message"][key] = value.clone();
        }
        entry
    }

    #[test]
    fn claude_turns_close_on_end_turn() {
        let mut turns = Turns::default();
        turns.update(&claude("user", 0, json!({})));
        turns.update(&claude(
            "assistant",
            5,
            json!({"message": {"stop_reason": "tool_use"}}),
        ));
        turns.update(&claude("user", 9, json!({})));
        turns.update(&claude(
            "assistant",
            20,
            json!({"message": {"stop_reason": "end_turn"}}),
        ));
        turns.update(&claude("user", 40, json!({})));
        let start = iso_timestamp(Some(&json!("2026-10-11T08:00:00Z"))).unwrap();
        assert_eq!(turns.spans, vec![(start, start + 20.0)]);
        assert_eq!(turns.open, Some(start + 40.0));
        assert_eq!(turns.last_at, start + 40.0);
    }

    #[test]
    fn interrupt_and_synthetic_error_close() {
        let mut turns = Turns::default();
        turns.update(&claude("user", 0, json!({})));
        turns.update(&claude(
            "user",
            3,
            json!({"message": {"content": "[Request interrupted by user]"}}),
        ));
        assert_eq!(turns.spans.len(), 1);
        assert_eq!(turns.open, None);
        turns.update(&claude("user", 10, json!({})));
        turns.update(&claude(
            "assistant",
            12,
            json!({"message": {"model": "<synthetic>"}}),
        ));
        assert_eq!(turns.spans.len(), 2);
        assert_eq!(turns.open, None);
    }

    #[test]
    fn codex_turns_follow_task_events() {
        let codex = |second: u32, kind: &str| {
            json!({
                "type": "event_msg",
                "timestamp": format!("2026-10-11T08:00:{second:02}Z"),
                "payload": {"type": kind},
            })
        };
        let mut turns = Turns::default();
        turns.update(&codex(0, "task_started"));
        turns.update(&codex(4, "token_count"));
        turns.update(&codex(8, "task_complete"));
        turns.update(&codex(9, "task_started"));
        turns.update(&codex(11, "turn_aborted"));
        assert_eq!(turns.spans.len(), 2);
        assert_eq!(turns.open, None);
    }
}
