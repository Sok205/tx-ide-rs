//! The agent's state: one vocabulary core owns, moved by the engine adapter. A hook the adapter
//! wired fires inside the session; [`crate::LlmEngine::on_hook`] turns it into an optional
//! [`LlmEngineStateTransition`]; core applies it to the record's [`LlmEngineStatus`] under the
//! session lock.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What the agent is doing. Fixed in core, so every consumer reads every engine the same way;
/// CLI-specific detail goes into [`LlmEngineStatus::note`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmEngineState {
    /// Launched; the CLI has not reported yet.
    Starting,
    /// The operator's turn.
    Idle,
    Working,
    /// Blocked on the operator mid-turn: an approval, a question.
    Waiting,
    /// The CLI is exiting or the process is gone.
    Ended,
}

impl LlmEngineState {
    pub const ALL: [LlmEngineState; 5] = [
        LlmEngineState::Starting,
        LlmEngineState::Idle,
        LlmEngineState::Working,
        LlmEngineState::Waiting,
        LlmEngineState::Ended,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            LlmEngineState::Starting => "starting",
            LlmEngineState::Idle => "idle",
            LlmEngineState::Working => "working",
            LlmEngineState::Waiting => "waiting",
            LlmEngineState::Ended => "ended",
        }
    }
}

/// The agent's state on the record (`session.status`), since when, and the adapter's note on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LlmEngineStatus {
    pub state: LlmEngineState,
    pub since: f64,
    /// Shown with the state: "approval: Bash", "error: rate_limit".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl LlmEngineStatus {
    pub fn new(state: LlmEngineState, since: f64) -> Self {
        Self {
            state,
            since,
            note: None,
        }
    }

    /// Applies an adapter's decision. To the current state: only the note is replaced. To a new
    /// state: `since = at` and the note is set or cleared. Returns the previous state when the
    /// state changed.
    pub fn apply(
        &mut self,
        transition: LlmEngineStateTransition,
        at: f64,
    ) -> Option<LlmEngineState> {
        self.note = transition.note;
        if transition.to == self.state {
            return None;
        }
        self.since = at;
        Some(std::mem::replace(&mut self.state, transition.to))
    }
}

/// What the adapter decided about one hook: move to `to`, with `note`. `on_hook` returns `None`
/// when the hook changes nothing, so a note never travels without a state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LlmEngineStateTransition {
    pub to: LlmEngineState,
    pub note: Option<String>,
}

impl LlmEngineStateTransition {
    pub fn to(state: LlmEngineState) -> Self {
        Self {
            to: state,
            note: None,
        }
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
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

#[cfg(test)]
mod tests {
    use super::*;
    use LlmEngineState as State;
    use LlmEngineStateTransition as Transition;

    #[test]
    fn names_round_trip_through_serde() {
        for state in State::ALL {
            let text = serde_json::to_string(&state).unwrap();
            assert_eq!(text, format!("\"{}\"", state.as_str()));
            assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
        }
    }

    #[test]
    fn same_state_replaces_only_the_note() {
        let mut status = LlmEngineStatus::new(State::Waiting, 1.0);
        assert_eq!(
            status.apply(Transition::to(State::Waiting).note("approval: Bash"), 2.0),
            None
        );
        assert_eq!(status.state, State::Waiting);
        assert_eq!(status.since, 1.0);
        assert_eq!(status.note.as_deref(), Some("approval: Bash"));
    }

    #[test]
    fn new_state_stamps_since_and_sets_or_clears_the_note() {
        let mut status = LlmEngineStatus::new(State::Working, 1.0);
        assert_eq!(
            status.apply(Transition::to(State::Idle).note("error: x"), 2.0),
            Some(State::Working)
        );
        assert_eq!(status.since, 2.0);
        assert_eq!(status.note.as_deref(), Some("error: x"));
        assert_eq!(
            status.apply(Transition::to(State::Working), 3.0),
            Some(State::Idle)
        );
        assert_eq!(status.note, None);
    }

    #[test]
    fn status_without_a_note_omits_it() {
        let status = LlmEngineStatus::new(State::Idle, 1.5);
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({"state": "idle", "since": 1.5})
        );
    }
}
