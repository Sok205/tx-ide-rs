//! tx-core plugin `board`: a GitHub issue board in the terminal (ratatui), the resident popup
//! behind prefix+b. Issues come from `gh`; the tx agents on each issue (tagged `issue-<n>`)
//! from the records. Data: [`github`] and [`model`]; state and input: [`app`]; drawing:
//! [`ui`]; the terminal and the event loop: [`run`]; the verbs: [`verbs`].

pub mod app;
pub mod github;
pub mod model;
pub mod run;
pub mod ui;
pub mod verbs;

use tx_core::{Plugin, PluginError, Registry};

pub const PLUGIN_ID: &str = "board";

#[derive(Clone, Copy, Debug, Default)]
pub struct BoardPlugin;

impl Plugin for BoardPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        let settings =
            verbs::Settings::from_value(r.settings()).map_err(|reason| PluginError::Register {
                plugin: PLUGIN_ID.to_owned(),
                reason,
            })?;
        for verb in verbs::verbs(settings) {
            r.verb(verb);
        }
        Ok(())
    }
}
