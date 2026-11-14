//! tx-core plugin `graph`: the agents as a graph, columns by lineage depth, edges for lineage
//! and for the messages they exchange. Data: [`model`] (records plus core's `log.jsonl`);
//! geometry: [`layout`].

pub mod layout;
pub mod model;

pub const PLUGIN_ID: &str = "graph";
