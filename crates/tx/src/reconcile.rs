//! `Reconciler` (reconcile.py): the no-daemon liveness sweep every read runs.
//!
//! - C1: one `tmux list-sessions` is the sole liveness signal, keyed by `@tx_id` (never the name,
//!   never the pid).
//! - C4: terminal records are skipped and only real transitions are saved + logged.
//! - C5: a stuck `WORKING` llm (turn older than the threshold, pane no longer the agent) is
//!   demoted to `IDLE`. The threshold comes from `config.json`, read only when a live `WORKING`
//!   llm needs it (the reference reads it every pass; only its crash on a malformed file is
//!   observable, and that becomes an error here).
//! - Q32: reconcile never revives — a terminal record stays terminal (`SessionService::revive`
//!   is the explicit way back).
//! - Launch scripts of sessions gone from tmux are swept after a grace period.

use std::collections::HashMap;
use std::io;
use std::path::Path;

use serde_json::Number;

use crate::engines::EngineRegistry;
use crate::events::{self, EventLog};
use crate::read_only::READ_ONLY_WRAPPER_BINARIES;
use crate::session::{Session, State};
use crate::storage::{Config, ConfigError, Home};
use crate::store::{SessionStore, StoreError};
use crate::tmux::Tmux;

/// Never sweep a launch script younger than this: its session may not be listed yet.
pub const LAUNCH_SCRIPT_GRACE_SECONDS: f64 = 60.0;

/// One `list-sessions` row: the match key is `@tx_id` (names are reusable, D7).
const LIST_FORMAT: &str = "#{@tx_id}\t#{session_name}\t#{pane_current_command}";

#[derive(Debug, thiserror::Error)]
pub enum ReconcileError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error("cannot append to {}: {source}", path.display())]
    Log {
        path: std::path::PathBuf,
        source: io::Error,
    },
}

/// The collaborators of one sweep, borrowed from the caller (the service).
pub struct Reconciler<'a> {
    pub store: &'a SessionStore,
    pub tmux: &'a Tmux,
    pub events: &'a EventLog,
    pub engines: &'a EngineRegistry,
    pub home: &'a Home,
    /// The actor stamped on every log line (`$TX_SESSION_ID`, else empty).
    pub actor: &'a str,
}

impl Reconciler<'_> {
    /// Drive every stored record to ground truth; returns only the records that changed, in
    /// store order.
    pub fn reconcile(&self) -> Result<Vec<Session>, ReconcileError> {
        let live = self.live_by_id();
        self.sweep_launch_scripts(&live);
        let mut threshold: Option<f64> = None;
        let mut changed = Vec::new();
        for mut session in self.store.all() {
            if session.state.is_terminal() {
                continue;
            }
            let dirty = match live.get(&session.id) {
                None => self.mark_exited(&mut session)?,
                Some(command) => self.demote_if_stuck(&mut session, command, &mut threshold)?,
            };
            if dirty {
                changed.push(session);
            }
        }
        Ok(changed)
    }

    /// `@tx_id` → `pane_current_command` over the single server scan. Sessions without an
    /// `@tx_id` are not ours and are ignored.
    fn live_by_id(&self) -> HashMap<String, String> {
        self.tmux
            .list_sessions(LIST_FORMAT)
            .into_iter()
            .filter_map(|row| {
                let mut fields = row.splitn(3, '\t');
                let tx_id = fields.next()?;
                let _name = fields.next()?;
                let command = fields.next()?;
                (!tx_id.is_empty()).then(|| (tx_id.to_owned(), command.to_owned()))
            })
            .collect()
    }

    /// GC `launch/*.sh` whose session is gone and whose mtime is past the grace period. Races
    /// with a concurrent sweep (and any other I/O failure) are tolerated.
    fn sweep_launch_scripts(&self, live: &HashMap<String, String>) {
        let Ok(entries) = std::fs::read_dir(self.home.launch_dir()) else {
            return;
        };
        let cutoff = events::now() - LAUNCH_SCRIPT_GRACE_SECONDS;
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            // `Path.stem`: a bare `.sh` is its own stem.
            let Some(stem) = name.to_str().and_then(|name| {
                name.strip_suffix(".sh")
                    .filter(|stem| !stem.is_empty())
                    .or((name == ".sh").then_some(name))
            }) else {
                continue;
            };
            if live.contains_key(stem) {
                continue;
            }
            if modified_secs(&entry.path()).is_some_and(|mtime| mtime < cutoff) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn mark_exited(&self, session: &mut Session) -> Result<bool, ReconcileError> {
        if !session.transition_to(State::Exited) {
            return Ok(false);
        }
        session.ended_at = Number::from_f64(events::now());
        session.attached_to.clear();
        self.store.save(session)?;
        self.log(&format!("{} → exited (vanished)", session.name))?;
        Ok(true)
    }

    /// C5: only an llm record in `WORKING` with an armed turn clock is ever demoted.
    fn demote_if_stuck(
        &self,
        session: &mut Session,
        command: &str,
        threshold: &mut Option<f64>,
    ) -> Result<bool, ReconcileError> {
        let Some(turn_started_at) = session
            .llm()
            .filter(|_| session.state == State::Working)
            .and_then(|llm| llm.turn_started_at.as_ref())
            .and_then(Number::as_f64)
        else {
            return Ok(false);
        };
        let threshold = match *threshold {
            Some(value) => value,
            None => *threshold
                .insert(Config::load(&self.home.config_path())?.stuck_working_threshold_seconds()?),
        };
        if events::now() - turn_started_at < threshold || self.is_agent_command(command) {
            return Ok(false);
        }
        if !session.transition_to(State::Idle) {
            return Ok(false);
        }
        self.store.save(session)?;
        self.log(&format!("{} working → idle (stuck)", session.name))?;
        Ok(true)
    }

    /// Whether a pane's `pane_current_command` is an agent still up: a read-only wrapper, any
    /// registered engine's binary, or a dotted version string (an agent TUI while loading).
    fn is_agent_command(&self, command: &str) -> bool {
        READ_ONLY_WRAPPER_BINARIES.contains(&command)
            || self.engines.engine_for_command(command).is_some()
            || is_version_command(command)
    }

    fn log(&self, msg: &str) -> Result<(), ReconcileError> {
        self.events
            .append("reconcile", msg, self.actor)
            .map_err(|source| ReconcileError::Log {
                path: self.events.path().to_owned(),
                source,
            })
    }
}

/// `re.match(r"^\d+\.\d+", command)`.
fn is_version_command(command: &str) -> bool {
    let rest = command.trim_start_matches(|c: char| c.is_ascii_digit());
    rest.len() < command.len()
        && rest
            .strip_prefix('.')
            .is_some_and(|tail| tail.starts_with(|c: char| c.is_ascii_digit()))
}

fn modified_secs(path: &Path) -> Option<f64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(since.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_commands() {
        // python: bool(re.compile(r"^\d+\.\d+").match(c))
        for command in ["2.1.138", "1.2", "10.0.0-beta", "1.2abc"] {
            assert!(is_version_command(command), "{command}");
        }
        for command in ["bash", "v2.1", "2", ".5.1", "2.", "", "1.x"] {
            assert!(!is_version_command(command), "{command}");
        }
    }
}
