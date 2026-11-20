//! tx-core plugin `ide`: the nested-tmux workspace layer. Its updater (run by `tx update`)
//! composes the server's config from the key fragments every active plugin publishes; `ui attach`
//! enters the IDE's server and `ui view` manages its views. It publishes the IDE's own resources:
//! theme, base keys, helper scripts, and the nvim runtime (`nvim/`) that every nvim view gets on
//! its runtimepath. The tmux and views code it runs on is the library `tx-client`.

pub mod install;
pub mod show_verb;
pub mod view_verb;

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
        r.updater(Arc::new(install::IdeUpdater));
        r.verb(Arc::new(install::ResourceVerb));
        r.verb(Arc::new(show_verb::ShowVerb));
        r.verb(Arc::new(show_verb::AttachIntoVerb));
        Ok(())
    }
}
