//! The agent's state: one vocabulary core owns, moved by the engine adapter. A hook the adapter
//! wired fires inside the session; [`crate::LlmEngine::on_hook`] answers with an
//! [`LlmEngineChange`]: the new state, if any, and the engine's own data, if it changed; core
//! writes both to the record under the session lock.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What the agent is doing. Fixed in core, so every consumer reads every engine the same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmEngineState {
    /// Launched; the CLI has not reported yet.
    Starting,
    /// The operator's move: a finished turn, or a turn stopped on an approval or a question.
    Idle,
    Working,
    /// The CLI is exiting or the process is gone.
    Ended,
}

impl LlmEngineState {
    pub const ALL: [LlmEngineState; 4] = [
        LlmEngineState::Starting,
        LlmEngineState::Idle,
        LlmEngineState::Working,
        LlmEngineState::Ended,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LlmEngineState::Starting => "starting",
            LlmEngineState::Idle => "idle",
            LlmEngineState::Working => "working",
            LlmEngineState::Ended => "ended",
        }
    }
}

/// One hook firing, as the CLI delivered it.
#[derive(Clone, Debug, PartialEq)]
pub struct LlmEngineHook {
    /// The CLI's own event name: "Stop", "PermissionRequest", …
    pub name: String,
    /// The CLI's JSON payload, untouched; always an object.
    pub payload: Value,
    pub at: f64,
}

/// What the engine decided about one hook. Both fields optional: a hook may change nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LlmEngineChange {
    pub state: Option<LlmEngineState>,
    /// The complete new value of `ext[engine.ext_key()]`; `None` leaves it as it is.
    pub engine_data: Option<Value>,
}

impl LlmEngineChange {
    /// Nothing changes.
    pub const NONE: LlmEngineChange = LlmEngineChange {
        state: None,
        engine_data: None,
    };

    /// Only the state moves.
    pub fn to(state: LlmEngineState) -> Self {
        Self {
            state: Some(state),
            engine_data: None,
        }
    }

    pub fn engine_data(mut self, engine_data: Value) -> Self {
        self.engine_data = Some(engine_data);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_through_serde() {
        for state in LlmEngineState::ALL {
            let text = serde_json::to_string(&state).unwrap();
            assert_eq!(text, format!("\"{}\"", state.as_str()));
            assert_eq!(
                serde_json::from_str::<LlmEngineState>(&text).unwrap(),
                state
            );
        }
    }
}
