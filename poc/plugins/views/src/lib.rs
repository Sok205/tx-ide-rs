//! tx-core plugin `views`: the nested-tmux workspace layer. Views are tmux sessions on the
//! core's private server whose panes nest-attach to agent sessions; `ext["views"]` records
//! which kind of view a session is and which group it belongs to.

pub mod picker;
pub mod render;
pub mod tmux;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tx_core::{Plugin, PluginError, Registry};

pub const PLUGIN_ID: &str = "views";

#[derive(Clone, Copy, Debug, Default)]
pub struct ViewsPlugin;

impl Plugin for ViewsPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(picker::List));
        r.verb(Arc::new(picker::Pick));
        Ok(())
    }
}

/// The `ext["views"]` shape on a session record.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ext {
    pub kind: Option<String>,
    pub group: Option<String>,
}
