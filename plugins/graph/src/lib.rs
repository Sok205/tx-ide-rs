//! tx-core plugin `graph`: the agents as a graph, columns by lineage depth, edges for lineage
//! and for the messages they exchange. Data: [`model`] (records plus core's `log.jsonl`);
//! geometry: [`layout`]; state and input: [`app`]; drawing: [`ui`]; the terminal: [`run`];
//! the verb: [`verbs`].

pub mod app;
pub mod layout;
pub mod model;
pub mod run;
pub mod ui;
pub mod verbs;

use tx_core::{Plugin, PluginError, Registry};

pub const PLUGIN_ID: &str = "graph";

#[derive(Clone, Copy, Debug, Default)]
pub struct GraphPlugin;

impl Plugin for GraphPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, registry: &mut Registry) -> Result<(), PluginError> {
        for verb in verbs::verbs() {
            registry.verb(verb);
        }
        registry.resource("keys/graph.tmux", include_bytes!("../tmux/keys.tmux"))
    }
}
