//! tx-core plugin `timeline`: swimlanes of session activity in the terminal (ratatui), the
//! resident popup behind prefix+T. Port of main's `plugins/timeline`. Data layer: [`model`]
//! over [`rows`] + [`cache`] (+ `tx-chat`'s message parser for the arrows); state and input:
//! [`app`]; drawing: [`ui`]; the terminal and the event loop: [`run`]; the verbs: [`verbs`].
//!
//! Working / idle spans come from the transcripts' own turn markers ([`turns`]), as in main;
//! the live state of a lane is the `hooks` plugin's `ext.hooks.state`. Nothing here needs a
//! core event: the view re-reads records and transcripts every tick.

pub mod app;
pub mod cache;
pub mod clock;
pub mod metadata;
pub mod model;
pub mod rows;
pub mod run;
pub mod turns;
pub mod ui;
pub mod verbs;

use tx_core::{Plugin, PluginError, Registry};

pub const PLUGIN_ID: &str = "timeline";

#[derive(Clone, Copy, Debug, Default)]
pub struct TimelinePlugin;

impl Plugin for TimelinePlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        for verb in verbs::verbs() {
            r.verb(verb);
        }
        r.resource("keys/timeline.tmux", include_bytes!("../tmux/keys.tmux"))
    }
}
