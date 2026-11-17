//! Opening the core for `tx` and `txi`: `tx_plugin::setup` with the plugins this binary links.

use std::sync::Arc;

use tx_core::{CoreConfig, PluginConfig, TxCore};
pub use tx_plugin::setup::*;

use crate::args::Global;
use crate::plugins;

/// The plugins this binary links. An installed plugin of the same id replaces the linked one.
pub fn linked() -> Linked {
    Linked {
        plugins: plugins::manifest(),
        reserved: plugins::RESERVED
            .iter()
            .map(|verb| (*verb).to_owned())
            .collect(),
    }
}

/// The `plugins:` section of the config file, as it concerns the manifest.
pub fn plugin_config(global: &Global, config: &CoreConfig) -> Result<PluginConfig, Failure> {
    tx_plugin::setup::plugin_config(global, config, &linked())
}

/// The verbs of the linked plugins this invocation would see, without opening the core (so
/// `help` and an unknown verb never create a directory).
pub fn preview_verbs(global: &Global) -> Result<Vec<Arc<dyn tx_core::Verb>>, Failure> {
    tx_plugin::setup::preview_verbs(global, &linked())
}

pub fn open(global: &Global) -> Result<TxCore, Failure> {
    tx_plugin::setup::open(global, linked())
}

/// Opens the core from an already resolved config, with the plugins this binary links and the
/// installed ones.
pub fn open_with(global: &Global, config: CoreConfig) -> Result<TxCore, Failure> {
    tx_plugin::setup::open_with(global, config, linked())
}
