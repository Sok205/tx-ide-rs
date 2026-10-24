//! tx-core plugin `hooks`: the agent's own hook system runs `txc hook <event>` inside the
//! session; the plugin records `ext["hooks"] = {"state", "since"}` on the record. Port of
//! tx-ide's `hooks.rs` without history ingest and chat-id capture.

pub mod install;
pub mod state;

use std::sync::Arc;

use serde_json::{Value, json};
use tx_core::events::now;
use tx_core::{
    CoreConfig, Event, EventSink, EventType, Lifecycle, ListFilter, Plugin, PluginError, Registry,
    SessionId, TxCore, Verb, VerbError, VerbIo,
};

pub use state::{PLUGIN_ID, SESSION_ID_ENV, State, read_payload, state_for};

pub const DEFAULT_STUCK_AFTER_SECONDS: u64 = 600;

const USAGE: &str = "\
usage: txc hook <event>               record the agent's state (run by the agent's hooks)
       txc hook status                name, state, since per running session
       txc hook install [--dry-run]   register the hooks with claude / codex
       txc hook uninstall
";

#[derive(Clone, Copy, Debug, Default)]
pub struct HooksPlugin;

impl Plugin for HooksPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        let stuck_after = match r.settings().get("stuck_after_seconds") {
            None => DEFAULT_STUCK_AFTER_SECONDS,
            Some(value) => value.as_u64().ok_or_else(|| PluginError::Register {
                plugin: PLUGIN_ID.into(),
                reason: format!(
                    "settings.stuck_after_seconds must be a non-negative integer, got {value}"
                ),
            })?,
        };
        r.sink(Box::new(Seed {
            config: r.config().clone(),
        }));
        r.verb(Arc::new(Hook { stuck_after }));
        Ok(())
    }
}

/// Seeds `idle` on create. Sinks get no core handle, so this opens one on the same state dir
/// (cheap: no tmux call).
struct Seed {
    config: CoreConfig,
}

impl EventSink for Seed {
    fn on_event(&self, event: &Event) {
        if event.type_ != EventType::Created {
            return;
        }
        let Ok(core) = TxCore::open(self.config.clone()) else {
            return;
        };
        let _ = core.update_ext(&event.id, PLUGIN_ID, |v| *v = record(State::Idle));
    }
}

fn record(state: State) -> Value {
    json!({"state": state.as_str(), "since": now()})
}

struct Hook {
    stuck_after: u64,
}

impl Verb for Hook {
    fn name(&self) -> &'static str {
        "hook"
    }

    fn usage(&self) -> &'static str {
        "hook <event> | status | install [--dry-run] | uninstall   agent hooks → session state"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv.first().map(String::as_str) {
            None | Some("-h" | "--help") => {
                let _ = write!(io.stderr, "{USAGE}");
                Ok(2)
            }
            Some("status") if argv.len() == 1 => self.status(core, io),
            Some("install") if argv.len() <= 2 && argv.get(1).is_none_or(|a| a == "--dry-run") => {
                let dry_run = argv.len() == 2;
                let exe = std::env::current_exe()
                    .map_err(|e| VerbError::Failed(format!("current_exe: {e}")))?;
                install::install(&home_dir()?, &exe, dry_run, io.stdout)
                    .map_err(|e| VerbError::Failed(e.to_string()))?;
                Ok(0)
            }
            Some("uninstall") if argv.len() == 1 => {
                install::uninstall(&home_dir()?, io.stdout)
                    .map_err(|e| VerbError::Failed(e.to_string()))?;
                Ok(0)
            }
            Some("status" | "install" | "uninstall") => {
                let _ = write!(io.stderr, "{USAGE}");
                Ok(2)
            }
            // An event: extra arguments are ignored rather than refused, because a hook must
            // never block the agent (exit 2) over its own command line.
            Some(event) => {
                self.apply(core, io, event)?;
                Ok(0)
            }
        }
    }
}

impl Hook {
    /// "Not ours" (no env var, malformed id, no record, unknown event) is a silent success.
    fn apply(&self, core: &TxCore, io: &mut VerbIo<'_>, event: &str) -> Result<(), VerbError> {
        let Some(id) = std::env::var(SESSION_ID_ENV)
            .ok()
            .and_then(|raw| raw.parse::<SessionId>().ok())
        else {
            return Ok(());
        };
        let payload = read_payload(io.stdin);
        let Some(state) = state_for(event, &payload) else {
            return Ok(());
        };
        match core.update_ext(&id, PLUGIN_ID, |v| {
            if v.get("state").and_then(Value::as_str) != Some(state.as_str()) {
                *v = record(state);
            }
        }) {
            Ok(_) | Err(tx_core::CoreError::NotFound(_)) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn status(&self, core: &TxCore, io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
        let filter = ListFilter::new().lifecycle(Lifecycle::Running);
        let mut rows = Vec::new();
        for id in core.list_sessions(&filter)? {
            let session = core.get_session(&id)?;
            let (state, since) = match session.ext.get(PLUGIN_ID) {
                Some(v) => (
                    v.get("state")
                        .and_then(Value::as_str)
                        .unwrap_or("-")
                        .to_owned(),
                    v.get("since").and_then(Value::as_f64),
                ),
                None => ("-".to_owned(), None),
            };
            let shown = match (state.as_str(), since) {
                ("working", Some(since)) if now() - since >= self.stuck_after as f64 => {
                    "stuck".to_owned()
                }
                _ => state,
            };
            rows.push((session.name, shown, since));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (name, state, since) in rows {
            let since = since
                .map(|s| format!("{s:.0}"))
                .unwrap_or_else(|| "-".into());
            let _ = writeln!(io.stdout, "{name}\t{state}\t{since}");
        }
        Ok(0)
    }
}

fn home_dir() -> Result<std::path::PathBuf, VerbError> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| VerbError::Failed("HOME is not set".into()))
}
