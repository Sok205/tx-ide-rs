//! tx-core plugin `artifacts`: durable versioned deliverables (`txc artifact …`), effort groups
//! (`txc group`), and prefix+a (`_artifact-menu`, `_artifact-pick`, `_artifact-go`).
//!
//! The store lives under `<data>/plugins/artifacts/`: `<id>.json` records,
//! `<id>/current<ext>` working copies, `<id>/revs/<n><ext>` snapshots, plus the plugin's own
//! `.log.jsonl` and `.locks/`. Per-session state is `ext["artifacts"] = {"group"}` (records
//! written before tx-client views may also carry a legacy `bound`, read until PR 11). An
//! artifact's nvim view is a tx-client view of kind `editor` bound to it (`@tx-view-bound`).
//! The touching session is `$TX_SESSION_ID`, else `user`.
//!
//! Settings (`plugins.yaml`): `editor` (default `nvim`), the program an artifact's view runs.

use std::path::PathBuf;
use std::sync::Arc;

use tx_core::{CoreConfig, Plugin, PluginError, Registry, SessionId, TxCore};

pub mod artifact;
pub mod difflib;
pub mod grouping;
pub mod menu;
pub mod picker;
pub mod render;
pub mod service;
pub mod store;
pub mod tmux;
pub mod verbs;
pub mod views;

pub const PLUGIN_ID: &str = "artifacts";
pub const DEFAULT_EDITOR: &str = "nvim";

/// `<data>/plugins/artifacts/`, the plugin's own store. Created on first write.
pub fn data_dir(config: &CoreConfig) -> PathBuf {
    config.plugin_data_dir(PLUGIN_ID)
}

/// The session a touch is recorded for: `$TX_SESSION_ID` when it is a valid id, else `user`.
pub fn actor() -> String {
    std::env::var("TX_SESSION_ID")
        .ok()
        .and_then(|id| id.parse::<SessionId>().ok())
        .map_or_else(|| artifact::USER_ACTOR.to_owned(), |id| id.to_string())
}

pub fn service(core: &TxCore) -> service::ArtifactService {
    service::ArtifactService::new(data_dir(core.config()), &actor())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ArtifactsPlugin;

impl Plugin for ArtifactsPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        let editor: Arc<str> = match r.settings().get("editor") {
            None => DEFAULT_EDITOR.into(),
            Some(serde_json::Value::String(editor)) if !editor.is_empty() => editor.as_str().into(),
            Some(other) => {
                return Err(PluginError::Register {
                    plugin: PLUGIN_ID.to_owned(),
                    reason: format!("settings.editor must be a program name, got {other}"),
                });
            }
        };
        r.verb(Arc::new(verbs::ArtifactVerb {
            editor: Arc::clone(&editor),
        }));
        r.verb(Arc::new(verbs::GroupVerb));
        r.verb(Arc::new(verbs::MenuVerb {
            editor: Arc::clone(&editor),
        }));
        r.verb(Arc::new(verbs::PickVerb {
            editor: Arc::clone(&editor),
        }));
        r.verb(Arc::new(verbs::GoVerb { editor }));
        r.resource("keys/artifacts.tmux", include_bytes!("../tmux/keys.tmux"))?;
        r.resource(
            "bin/tmux-artifact-menu",
            include_bytes!("../bin/tmux-artifact-menu"),
        )
    }
}
