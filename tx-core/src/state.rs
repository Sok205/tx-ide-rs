//! The agent's state: one vocabulary core owns, moved by the engine adapter. A hook the adapter
//! wired fires inside the session; [`crate::LlmEngine::on_hook`] answers with an
//! [`LlmEngineChange`]: the new state, if any, why an `Idle` one is idle, and the engine's own
//! data, if it changed; core writes them to the record under the session lock.

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

/// Why an `Idle` agent is idle; core keeps it on the record only while the state is `Idle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdleReason {
    /// The turn ended: finished, failed, or the session just started.
    Finished,
    /// The turn stopped mid-way on the operator: an approval, a question, a plan to accept.
    Waiting,
}

impl IdleReason {
    pub const ALL: [IdleReason; 2] = [IdleReason::Finished, IdleReason::Waiting];

    pub fn as_str(self) -> &'static str {
        match self {
            IdleReason::Finished => "finished",
            IdleReason::Waiting => "waiting",
        }
    }
}

/// The state as people read it: `idle (waiting)` when the reason is known, else the state's
/// name. `--json` and the record keep the two apart.
pub fn state_label(state: LlmEngineState, idle_reason: Option<IdleReason>) -> String {
    match idle_reason {
        Some(reason) => format!("{} ({})", state.as_str(), reason.as_str()),
        None => state.as_str().to_owned(),
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

/// What the engine decided about one hook. Every field optional: a hook may change nothing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LlmEngineChange {
    pub state: Option<LlmEngineState>,
    /// Why the new state is `Idle`; read only when `state` is `Some(Idle)`.
    pub idle_reason: Option<IdleReason>,
    /// The complete new value of `ext[engine.ext_key()]`; `None` leaves it as it is.
    pub engine_data: Option<Value>,
}

impl LlmEngineChange {
    /// Nothing changes.
    pub const NONE: LlmEngineChange = LlmEngineChange {
        state: None,
        idle_reason: None,
        engine_data: None,
    };

    /// Only the state moves.
    pub fn to(state: LlmEngineState) -> Self {
        Self {
            state: Some(state),
            ..Self::NONE
        }
    }

    /// The state moves to `Idle`, for `reason`.
    pub fn idle(reason: IdleReason) -> Self {
        Self {
            state: Some(LlmEngineState::Idle),
            idle_reason: Some(reason),
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
        for reason in IdleReason::ALL {
            let text = serde_json::to_string(&reason).unwrap();
            assert_eq!(text, format!("\"{}\"", reason.as_str()));
            assert_eq!(serde_json::from_str::<IdleReason>(&text).unwrap(), reason);
        }
    }

    #[test]
    fn labels_carry_a_known_reason() {
        assert_eq!(state_label(LlmEngineState::Working, None), "working");
        assert_eq!(state_label(LlmEngineState::Idle, None), "idle");
        assert_eq!(
            state_label(LlmEngineState::Idle, Some(IdleReason::Waiting)),
            "idle (waiting)"
        );
    }
}
