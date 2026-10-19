//! tx-core plugin `views`: the nested-tmux workspace layer. Views are tmux sessions on the
//! core's private server; `ui install` wires the IDE's tmux config into it.

pub mod install;

use std::sync::Arc;

use tx_core::{Plugin, PluginError, Registry};

pub const PLUGIN_ID: &str = "views";

#[derive(Clone, Copy, Debug, Default)]
pub struct ViewsPlugin;

impl Plugin for ViewsPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(install::UiVerb));
        Ok(())
    }
}
