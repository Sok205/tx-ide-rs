//! tx-core plugin `assistant`: the `tx-assistant` agent session. `assistant warm` starts it
//! unless it is running and prints its name; `txc ui attach` calls it to land the operator in
//! the assistant. Ships the TX-ASSISTANT role as an `agents/` resource.

use std::sync::Arc;

use tx_core::{
    CoreError, CreateOptions, Effort, LaunchOptions, Lifecycle, Plugin, PluginError, Registry,
    SessionId, TxCore, Verb, VerbError, VerbIo,
};

pub const PLUGIN_ID: &str = "assistant";
pub const SESSION_NAME: &str = "tx-assistant";
pub const ROLE: &str = "TX-ASSISTANT";
pub const COMMON_ROLE: &str = "COMMON";
pub const TAG: &str = "tx-system";
pub const ENGINE: &str = "claude";
pub const MODEL: &str = "opus";
pub const EFFORT: Effort = Effort::Medium;

#[derive(Clone, Copy, Debug, Default)]
pub struct AssistantPlugin;

impl Plugin for AssistantPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(Assistant));
        r.resource(
            "agents/TX-ASSISTANT.md",
            include_bytes!("../../../../agents/TX-ASSISTANT.md"),
        )
    }
}

/// The assistant session: COMMON + TX-ASSISTANT on claude, tagged `tx-system`, in the state
/// dir (so the engine's skill links land there, not in `$HOME`).
pub fn create_options(core: &TxCore) -> CreateOptions {
    let launch = LaunchOptions::agent(ENGINE)
        .profile([COMMON_ROLE, ROLE])
        .model(MODEL)
        .effort(EFFORT);
    CreateOptions::new(SESSION_NAME, core.config().state_dir.clone(), launch).tags([TAG])
}

/// The running assistant's id, after a liveness check; `None` when there is none.
pub fn running(core: &TxCore) -> Result<Option<SessionId>, CoreError> {
    let id = match core.resolve(SESSION_NAME) {
        Ok(id) => id,
        Err(CoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let session = core.refresh_session(&id)?.session().clone();
    Ok((session.lifecycle == Lifecycle::Running).then_some(id))
}

/// The running assistant, started when there is none.
pub fn warm(core: &TxCore) -> Result<SessionId, CoreError> {
    match running(core)? {
        Some(id) => Ok(id),
        None => core.create_session(create_options(core)),
    }
}

pub struct Assistant;

impl Verb for Assistant {
    fn name(&self) -> &'static str {
        "assistant"
    }

    fn usage(&self) -> &'static str {
        "assistant warm   start the tx-assistant session unless it is running; prints its name"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv {
            [sub] if sub == "warm" => {
                warm(core)?;
                writeln!(io.stdout, "{SESSION_NAME}")
                    .map_err(|error| VerbError::Failed(format!("write: {error}")))?;
                Ok(0)
            }
            _ => {
                let _ = writeln!(io.stderr, "usage: txc assistant warm");
                Ok(2)
            }
        }
    }
}
