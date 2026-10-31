//! The tx client: the code shared by the key plugins (the socket-bound tmux helper, the picker
//! renderer and palette, the modal editor, the `views` store) and the plugin `ide`, the
//! nested-tmux workspace layer. Views are the non-LLM tmux sessions on the core's private server
//! (`views`); `ui install` composes the server's config from the resources every active plugin
//! publishes, and `ide` publishes the IDE's own: theme, base keys, helper scripts, and the
//! nvim runtime (`nvim/`) that every nvim view gets on its runtimepath.

pub mod editor;
pub mod install;
pub mod prompt;
pub mod render;
pub mod tmux;
pub mod view_verb;
pub mod views;

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
            "bin/tmux-agents-status",
            include_bytes!("../bin/tmux-agents-status"),
        )?;
        r.resource(
            "bin/tmux-system-resources",
            include_bytes!("../bin/tmux-system-resources"),
        )?;
        r.resource(
            "nvim/lua/tx-tour.lua",
            include_bytes!("../nvim/lua/tx-tour.lua"),
        )?;
        r.resource(
            "nvim/lua/tx-diffview-return.lua",
            include_bytes!("../nvim/lua/tx-diffview-return.lua"),
        )?;
        r.resource(
            "nvim/lua/tx-ask.lua",
            include_bytes!("../nvim/lua/tx-ask.lua"),
        )?;
        r.resource(
            "nvim/plugin/tx.lua",
            include_bytes!("../nvim/plugin/tx.lua"),
        )
    }
}
