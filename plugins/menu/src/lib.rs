//! tx-core plugin `menu`: the tx-ide start menu. A full-screen popup with the shimmering `TX`
//! logo, every useful tx-ide action one key away, and the recent past sessions. It opens on
//! prefix+m and, once per server, on the first client attach. Items run other plugins' verbs
//! only through `txc` and tmux (D3); an item whose plugin is disabled is hidden.
//!
//! State and keys: [`model`]; what it shows: [`data`]; drawing: [`ui`]; the terminal: [`run`];
//! the verbs and the actions: [`verbs`].

pub mod data;
pub mod model;
pub mod run;
pub mod ui;
pub mod verbs;

use tx_core::{Plugin, PluginError, Registry};

pub const PLUGIN_ID: &str = "menu";

#[derive(Clone, Copy, Debug, Default)]
pub struct MenuPlugin;

impl Plugin for MenuPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        for verb in verbs::verbs() {
            r.verb(verb);
        }
        r.resource("keys/menu.tmux", include_bytes!("../tmux/keys.tmux"))
    }
}
