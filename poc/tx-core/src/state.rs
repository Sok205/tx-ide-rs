//! The agent's state: one vocabulary core owns, moved by the engine adapter. A hook the adapter
//! wired fires inside the session; [`crate::LlmEngine::on_hook`] turns it into a [`Change`];
//! core applies it to the record's [`Status`] under the session lock.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What the agent is doing. Fixed in core, so every consumer reads every engine the same way;
/// CLI-specific detail goes into [`Status::note`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
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

impl State {
    pub const ALL: [State; 5] = [
        State::Starting,
        State::Idle,
        State::Working,
        State::Waiting,
        State::Ended,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Idle => "idle",
            State::Working => "working",
            State::Waiting => "waiting",
            State::Ended => "ended",
        }
    }
}

/// The agent's state on the record, since when, and the adapter's note on it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub state: State,
    pub since: f64,
    /// Shown with the state: "approval: Bash", "error: rate_limit".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Status {
    pub fn new(state: State, since: f64) -> Self {
        Self {
            state,
            since,
            note: None,
        }
    }

    /// Applies an adapter's decision. No state: nothing happens (a note only comes with a
    /// state). The same state: only the note is replaced. A new state: `since = at` and the
    /// note is set or cleared. Returns the previous state when the state changed.
    pub fn apply(&mut self, change: Change, at: f64) -> Option<State> {
        let state = change.state?;
        self.note = change.note;
        if state == self.state {
            return None;
        }
        self.since = at;
        Some(std::mem::replace(&mut self.state, state))
    }
}

/// What the adapter decided about one hook. Every field optional: a hook may change nothing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub state: Option<State>,
    pub note: Option<String>,
}

impl Change {
    pub fn to(state: State) -> Self {
        Self {
            state: Some(state),
            note: None,
        }
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// One hook invocation, as the CLI delivered it.
#[derive(Clone, Debug, PartialEq)]
pub struct Hook {
    /// The CLI's own event name: "Stop", "PermissionRequest", …
    pub name: String,
    /// The CLI's JSON payload, untouched; always an object.
    pub payload: Value,
    pub at: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip_through_serde() {
        for state in State::ALL {
            let text = serde_json::to_string(&state).unwrap();
            assert_eq!(text, format!("\"{}\"", state.as_str()));
            assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
        }
    }

    #[test]
    fn no_state_changes_nothing_and_drops_the_note() {
        let mut status = Status {
            note: Some("kept".into()),
            ..Status::new(State::Idle, 1.0)
        };
        let change = Change {
            state: None,
            note: Some("ignored".into()),
        };
        assert_eq!(status.apply(change, 2.0), None);
        assert_eq!(status.note.as_deref(), Some("kept"));
        assert_eq!(status.since, 1.0);
    }

    #[test]
    fn same_state_replaces_only_the_note() {
        let mut status = Status::new(State::Waiting, 1.0);
        assert_eq!(
            status.apply(Change::to(State::Waiting).note("approval: Bash"), 2.0),
            None
        );
        assert_eq!(status.state, State::Waiting);
        assert_eq!(status.since, 1.0);
        assert_eq!(status.note.as_deref(), Some("approval: Bash"));
    }

    #[test]
    fn new_state_stamps_since_and_sets_or_clears_the_note() {
        let mut status = Status::new(State::Working, 1.0);
        assert_eq!(
            status.apply(Change::to(State::Idle).note("error: x"), 2.0),
            Some(State::Working)
        );
        assert_eq!(status.since, 2.0);
        assert_eq!(status.note.as_deref(), Some("error: x"));
        assert_eq!(
            status.apply(Change::to(State::Working), 3.0),
            Some(State::Idle)
        );
        assert_eq!(status.note, None);
    }

    #[test]
    fn status_without_a_note_omits_it() {
        let status = Status::new(State::Idle, 1.5);
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            serde_json::json!({"state": "idle", "since": 1.5})
        );
    }
}
