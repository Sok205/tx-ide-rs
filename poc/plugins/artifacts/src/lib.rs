//! tx-core plugin `artifacts`: durable deliverables of three kinds (documents, diffs, tours;
//! `txc artifact …`), effort groups (`txc group`), and prefix+a (`_artifact-menu`,
//! `_artifact-pick`, `_artifact-go`).
//!
//! The store lives under `<state_dir>/plugins/artifacts/`: `<id>.json` records, `<id>/` content
//! directories holding the kind's file (and `.cleanup.json`, pending close steps), plus the
//! plugin's own `.log.jsonl` and `.scratch/` (a diff's temp worktrees, the tour runtime).
//! Per-session state is `ext["artifacts"] = {"bound", "group"}`. The owner of a new artifact is
//! `$TX_SESSION_ID`, else `user`. When a bound view exits or is stopped, the close step its open
//! left behind runs.
//!
//! Settings (`plugins.yaml`): `editor` (default `nvim`), the program an artifact's view runs; a
//! diff or tour passes it nvim arguments.

use std::sync::Arc;

use tx_core::{
    CoreConfig, Event, EventSink, EventType, Plugin, PluginError, Registry, SessionId, TxCore,
};

pub mod artifact;
pub mod grouping;
pub mod kinds;
pub mod menu;
pub mod migrate;
pub mod picker;
pub mod render;
pub mod service;
pub mod store;
pub mod verbs;
pub mod views;

pub const PLUGIN_ID: &str = "artifacts";
pub const DEFAULT_EDITOR: &str = "nvim";

/// The acting session: `$TX_SESSION_ID` when it is a valid id, else `user`.
pub fn actor() -> String {
    std::env::var("TX_SESSION_ID")
        .ok()
        .and_then(|id| id.parse::<SessionId>().ok())
        .map_or_else(|| artifact::USER_ACTOR.to_owned(), |id| id.to_string())
}

pub fn service(core: &TxCore) -> service::ArtifactService {
    service::ArtifactService::new(core.config().plugin_dir(PLUGIN_ID), &actor())
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
        r.sink(Box::new(CloseOnEnd {
            config: r.config().clone(),
        }));
        r.resource("keys/artifacts.tmux", include_bytes!("../tmux/keys.tmux"))?;
        r.resource(
            "bin/tmux-artifact-menu",
            include_bytes!("../bin/tmux-artifact-menu"),
        )
    }
}

/// Runs a bound view's pending close step when the view ends. Sinks get no core handle, so this
/// opens one on the same state dir (cheap: no tmux call). Failures are reported, never fatal.
struct CloseOnEnd {
    config: CoreConfig,
}

impl EventSink for CloseOnEnd {
    fn on_event(&self, event: &Event) {
        if !matches!(event.type_, EventType::Exited | EventType::Stopped) {
            return;
        }
        let Ok(core) = TxCore::open(self.config.clone()) else {
            return;
        };
        let Some(artifact_id) = core
            .get_session(&event.id)
            .ok()
            .and_then(|view| grouping::bound_artifact(&view))
        else {
            return;
        };
        let service = service::ArtifactService::new(self.config.plugin_dir(PLUGIN_ID), &actor());
        if let Err(error) = service.close(&artifact_id, event.id.as_str()) {
            eprintln!("txc artifact: close step of {}: {error}", event.name);
        }
    }
}
