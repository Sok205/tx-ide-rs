//! tx-core plugin `mockup`: agents write JSON screen descriptions, a ratatui viewer renders
//! them in a per-agent `mockup-<agent>` view, and the operator's terminal is switched there
//! and back. The `model` module is the pure description format.

use tx_core::{Plugin, PluginError, Registry};

pub mod model;

pub const PLUGIN_ID: &str = "mockup";

#[derive(Clone, Copy, Debug, Default)]
pub struct MockupPlugin;

impl Plugin for MockupPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.resource(
            "agents/COMMON.local.md",
            include_bytes!("../../../../agents/COMMON.mockup.local.md"),
        )?;
        r.resource(
            "agents/skills/tx-mockup/SKILL.md",
            include_bytes!("../../../../agents/skills/tx-mockup/SKILL.md"),
        )
    }
}
