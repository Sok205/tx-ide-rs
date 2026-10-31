//! tx-core plugin `kill`: prefix+X confirms, then stops the session in the active pane: an
//! agent through `txc kill` so the record stays honest, a view through `txc ui view close`.
//! `txc kill <name>` also closes a view by name, under the views guard (never an agent's tmux
//! session behind core's back). Publishes `keys/kill.tmux` and its helper script.

use std::sync::Arc;

use tx_client::tmux::Ui;
use tx_client::views::{self, ViewError};
use tx_core::{AgentId, CoreError, Plugin, PluginError, Registry, TxCore, Verb, VerbError, VerbIo};

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
fn resolve_or_not_found(core: &TxCore, token: &str) -> Result<Option<AgentId>, VerbError> {
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
        "kill <name>   stop a session, or close a view (prefix+X)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let [token] = argv else {
            let _ = writeln!(io.stderr, "usage: txc kill <name>");
            return Ok(2);
        };
        let ui = Ui::from_core(core);
        let name = match resolve_or_not_found(core, token)? {
            Some(id) => core.stop_agent(&id)?.name,
            None => match views::close(&ui, token) {
                Ok(()) => token.clone(),
                Err(ViewError::NotFound(_) | ViewError::NotAView(_)) => {
                    let _ = writeln!(io.stderr, "session '{token}' not found");
                    return Ok(1);
                }
                Err(error) => return Err(VerbError::Failed(error.to_string())),
            },
        };
        write_out(io, &format!("Killed '{name}'\n"))?;
        Ok(0)
    }
}
