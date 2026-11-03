//! tx-core plugin `home`: tx-ide's session manager, the landing screen. Projects, their agents
//! (running and ended) and the views that belong to them, with live state; Enter nests into one.
//! It is the floor of the `home` view's pane ([`floor`]: the TUI comes back whenever what runs
//! there ends) and opens in a popup on prefix+m. Actions run other plugins' verbs only through
//! `tx` and tmux (D3).
//!
//! State and keys: [`model`]; what it shows: [`data`], [`detail`], [`project`]; drawing: [`ui`];
//! the terminal: [`run`]; the verbs and the actions: [`verbs`].

pub mod backdrop;
pub mod data;
pub mod detail;
pub mod floor;
pub mod model;
pub mod project;
pub mod run;
pub mod ui;
pub mod verbs;

use tx_core::{Plugin, PluginError, Registry};

pub const PLUGIN_ID: &str = "home";

#[derive(Clone, Copy, Debug, Default)]
pub struct HomePlugin;

impl Plugin for HomePlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        for verb in verbs::verbs() {
            r.verb(verb);
        }
        r.resource("keys/home.tmux", include_bytes!("../tmux/keys.tmux"))
    }
}
