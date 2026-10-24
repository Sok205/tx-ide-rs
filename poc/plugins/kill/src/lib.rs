//! tx-core plugin `kill`: prefix+X confirms, then stops the session in the active pane through
//! `txc kill` so the record stays honest. Publishes `keys/kill.tmux` and its helper script.

use std::sync::Arc;

use tx_core::{
    CoreError, Plugin, PluginError, Registry, SessionId, TxCore, Verb, VerbError, VerbIo,
};
use tx_ui::tmux::Ui;

pub const PLUGIN_ID: &str = "kill";

#[derive(Clone, Copy, Debug, Default)]
pub struct KillPlugin;

impl Plugin for KillPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(Kill));
        r.resource("keys/kill.tmux", include_bytes!("../tmux/keys.tmux"))?;
        r.resource(
            "bin/tmux-kill-tx-session",
            include_bytes!("../bin/tmux-kill-tx-session"),
        )
    }
}

fn write_out(io: &mut VerbIo<'_>, text: &str) -> Result<(), VerbError> {
    io.stdout
        .write_all(text.as_bytes())
        .map_err(|e| VerbError::Failed(format!("write: {e}")))
}

/// `None` when nothing has that name or id.
fn resolve_or_not_found(core: &TxCore, token: &str) -> Result<Option<SessionId>, VerbError> {
    match core.resolve(token) {
        Ok(id) => Ok(Some(id)),
        Err(CoreError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub struct Kill;

impl Verb for Kill {
    fn name(&self) -> &'static str {
        "kill"
    }

    fn usage(&self) -> &'static str {
        "kill <name>   stop a session (prefix+X)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [token] = argv else {
            let _ = writeln!(io.stderr, "usage: txc kill <name>");
            return Ok(2);
        };
        let ui = Ui::from_core(core);
        let name = match resolve_or_not_found(core, token)? {
            Some(id) => core.stop_session(&id)?.name,
            None if ui.has_session(token) && ui.is_view(token) && ui.kill_session(token) => {
                token.clone()
            }
            None => {
                let _ = writeln!(io.stderr, "session '{token}' not found");
                return Ok(1);
            }
        };
        write_out(io, &format!("Killed '{name}'\n"))?;
        Ok(0)
    }
}
