//! The event → state table (port of tx-ide `hooks.rs`): which agent hook means what.

use std::io::Read;

use serde_json::{Map, Value};

pub const PLUGIN_ID: &str = "hooks";
pub const SESSION_ID_ENV: &str = "TX_SESSION_ID";

/// `Notification` subtypes meaning "the agent yielded — needs you".
const WAITING_NOTIFICATION_TYPES: [&str; 3] =
    ["idle_prompt", "permission_prompt", "elicitation_dialog"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Working,
    Waiting,
    Idle,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Waiting => "waiting",
            State::Idle => "idle",
        }
    }
}

/// `None` is "no transition": an unknown event or a notification that is not a yield.
pub fn state_for(event: &str, payload: &Value) -> Option<State> {
    match event {
        "prompt-submit" | "working" => Some(State::Working),
        "stop" => Some(State::Waiting),
        "session-end" => Some(State::Idle),
        "notification" => payload
            .get("notification_type")
            .and_then(Value::as_str)
            .filter(|kind| WAITING_NOTIFICATION_TYPES.contains(kind))
            .map(|_| State::Waiting),
        _ => None,
    }
}

/// The hook payload, tolerantly: `{}` for none / unreadable / bad JSON / non-object. A hook
/// must never fail the agent over its own input.
pub fn read_payload(stdin: &mut dyn Read) -> Value {
    let mut text = String::new();
    if stdin.read_to_string(&mut text).is_err() {
        return Value::Object(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::Object(Map::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_table() {
        let none = json!({});
        assert_eq!(state_for("prompt-submit", &none), Some(State::Working));
        assert_eq!(state_for("working", &none), Some(State::Working));
        assert_eq!(state_for("stop", &none), Some(State::Waiting));
        assert_eq!(state_for("session-end", &none), Some(State::Idle));
        for other in ["", "start", "ingest", "STOP"] {
            assert_eq!(state_for(other, &none), None, "{other:?}");
        }
    }

    #[test]
    fn notification_yields_only_on_known_subtypes() {
        for kind in ["idle_prompt", "permission_prompt", "elicitation_dialog"] {
            assert_eq!(
                state_for("notification", &json!({"notification_type": kind})),
                Some(State::Waiting)
            );
        }
        assert_eq!(
            state_for("notification", &json!({"notification_type": "other"})),
            None
        );
        assert_eq!(state_for("notification", &json!({})), None);
        assert_eq!(state_for("notification", &json!([1])), None);
    }

    #[test]
    fn payload_degrades_to_empty_object() {
        for text in ["", "not json", "[1,2]", "\"str\"", "null"] {
            assert_eq!(read_payload(&mut text.as_bytes()), json!({}), "{text:?}");
        }
        assert_eq!(read_payload(&mut r#"{"a":1}"#.as_bytes()), json!({"a": 1}));
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("no"))
            }
        }
        assert_eq!(read_payload(&mut Broken), json!({}));
    }
}
