//! The event → state table (port of tx-ide `hooks.rs`): which agent hook means what.

use serde_json::Value;
use tx_ui::render::HookState;

pub const PLUGIN_ID: &str = tx_ui::render::HOOKS_KEY;

/// `Notification` subtypes meaning "the agent yielded — needs you".
const WAITING_NOTIFICATION_TYPES: [&str; 3] =
    ["idle_prompt", "permission_prompt", "elicitation_dialog"];

/// `None` is "no transition": an unknown event or a notification that is not a yield.
pub fn state_for(event: &str, payload: &Value) -> Option<HookState> {
    match event {
        "prompt-submit" | "working" => Some(HookState::Working),
        "stop" => Some(HookState::Waiting),
        "session-end" => Some(HookState::Idle),
        "notification" => payload
            .get("notification_type")
            .and_then(Value::as_str)
            .filter(|kind| WAITING_NOTIFICATION_TYPES.contains(kind))
            .map(|_| HookState::Waiting),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_table() {
        let none = json!({});
        assert_eq!(state_for("prompt-submit", &none), Some(HookState::Working));
        assert_eq!(state_for("working", &none), Some(HookState::Working));
        assert_eq!(state_for("stop", &none), Some(HookState::Waiting));
        assert_eq!(state_for("session-end", &none), Some(HookState::Idle));
        for other in ["", "start", "ingest", "STOP"] {
            assert_eq!(state_for(other, &none), None, "{other:?}");
        }
    }

    #[test]
    fn notification_yields_only_on_known_subtypes() {
        for kind in ["idle_prompt", "permission_prompt", "elicitation_dialog"] {
            assert_eq!(
                state_for("notification", &json!({"notification_type": kind})),
                Some(HookState::Waiting)
            );
        }
        assert_eq!(
            state_for("notification", &json!({"notification_type": "other"})),
            None
        );
        assert_eq!(state_for("notification", &json!({})), None);
        assert_eq!(state_for("notification", &json!([1])), None);
    }
}
