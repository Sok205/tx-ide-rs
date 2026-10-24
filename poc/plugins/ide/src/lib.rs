//! tx-core plugin `ide`: the nested-tmux workspace layer. Views are tmux sessions on the core's
//! private server; `ui install` composes the server's config from the resources every active
//! plugin publishes, and this plugin publishes the IDE's own: theme, base keys, helper scripts.

pub mod install;

use std::sync::Arc;

use tx_core::{Plugin, PluginError, Registry};

pub const PLUGIN_ID: &str = "ide";

#[derive(Clone, Copy, Debug, Default)]
pub struct IdePlugin;

impl Plugin for IdePlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(install::UiVerb));
        r.verb(Arc::new(install::ResourceVerb));
        r.resource("conf/tmux.conf", include_bytes!("../conf/tmux.conf"))?;
        r.resource("keys/ide.tmux", include_bytes!("../tmux/keys.tmux"))?;
        r.resource("bin/tmux-nav", include_bytes!("../bin/tmux-nav"))?;
        r.resource(
            "bin/tmux-system-resources",
            include_bytes!("../bin/tmux-system-resources"),
        )
    }
}
