//! tx-core plugin `mockup`: agents write JSON screen descriptions, a ratatui viewer renders
//! them in a per-agent `mockup-<agent>` view, and the operator's terminal is switched there
//! and back. The `model` module is the pure description format.

use tx_core::{Plugin, PluginError, Registry};

pub mod model;
pub mod render;

pub const PLUGIN_ID: &str = "mockup";

#[derive(Clone, Copy, Debug, Default)]
pub struct MockupPlugin;

impl Plugin for MockupPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, _r: &mut Registry) -> Result<(), PluginError> {
        Ok(())
    }
}
