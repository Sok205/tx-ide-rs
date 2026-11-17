//! The plugin switchboard: the compiled-in manifest, and what `tx plugins` lists. The `plugins:`
//! section of `config.yaml` is read by `tx_plugin::config`.

use std::path::PathBuf;

use serde_json::{Value, json};
use tx_core::plugin::Manifest;
use tx_core::{Plugin, PluginStatus};
pub use tx_plugin::config::{Entry, RETIRED, plugin_config};
use tx_plugin::setup::Installation;

/// Every plugin this binary links, in listing order. One that is also installed in a plugin
/// root is left to the installed plugin.
pub const MANIFEST: &[fn() -> Box<dyn Plugin>] = &[
    || Box::new(tx_engine_claude::ClaudePlugin),
    || Box::new(tx_engine_codex::CodexPlugin),
    || Box::new(tx_chat::ChatPlugin),
    || Box::new(tx_client::IdePlugin),
    || Box::new(tx_labels::LabelsPlugin),
    || Box::new(tx_kill::KillPlugin),
    || Box::new(tx_picker::PickerPlugin),
    || Box::new(tx_edit::EditPlugin),
    || Box::new(tx_spawn::SpawnPlugin),
    || Box::new(tx_assistant::AssistantPlugin),
    || Box::new(tx_mockup::MockupPlugin),
    || Box::new(tx_tutor::TutorPlugin),
    || Box::new(tx_artifacts::ArtifactsPlugin),
    || Box::new(tx_tour::TourPlugin),
    || Box::new(tx_timeline::TimelinePlugin),
    || Box::new(tx_graph::GraphPlugin),
    || Box::new(tx_home::HomePlugin),
    || Box::new(tx_home_themes::HomeThemesPlugin),
    || Box::new(tx_layouts::LayoutsPlugin),
];

/// The built-in verbs; a plugin may not provide one of these.
pub const RESERVED: &[&str] = tx_plugin::RESERVED_VERBS;

pub fn manifest() -> Vec<Box<dyn Plugin>> {
    MANIFEST.iter().map(|make| make()).collect()
}

/// The manifest's ids.
pub fn known_ids() -> Vec<&'static str> {
    MANIFEST.iter().map(|make| make().id()).collect()
}

/// Where a listed plugin comes from: the first plugin root, any other root, or this binary.
pub const SHIPPED: &str = "shipped";
pub const USER: &str = "user";
pub const BUILT_IN: &str = "built-in";

/// One row of `tx plugins`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub status: PluginStatus,
    /// Why it is in error, or what an active plugin declares and does not get.
    pub detail: Option<String>,
    /// `None` for an entry of the config that names no plugin.
    pub origin: Option<&'static str>,
    pub directory: Option<PathBuf>,
    /// Of an installed plugin whose manifest loaded.
    pub manifest: Option<Manifest>,
}

impl Row {
    /// `<id>\t<status>[\t<detail>]`.
    pub fn line(&self) -> String {
        let mut line = format!("{}\t{}", self.id, self.status.as_str());
        if let Some(detail) = &self.detail {
            line.push('\t');
            line.push_str(detail);
        }
        line
    }

    /// `{id, status, detail, origin, directory, description, tier, requires, needs, verbs,
    /// engine}`: what only a manifest says is `null`, or an empty list, without one.
    pub fn json(&self) -> Value {
        let manifest = self.manifest.as_ref();
        let verbs: Vec<&str> = manifest
            .iter()
            .flat_map(|manifest| &manifest.verbs)
            .map(|verb| verb.name.as_str())
            .collect();
        json!({
            "id": self.id,
            "status": self.status.as_str(),
            "detail": self.detail,
            "origin": self.origin,
            "directory": self.directory.as_ref().map(|directory| directory.to_string_lossy()),
            "description": manifest.map(|manifest| &manifest.description),
            "tier": manifest.map(|manifest| manifest.tier.as_str()),
            "requires": manifest.map_or(&[][..], |manifest| &manifest.requires),
            "needs": manifest.map_or(&[][..], |manifest| &manifest.needs),
            "verbs": verbs,
            "engine": manifest
                .and_then(|manifest| manifest.engine.as_ref())
                .map(|engine| &engine.kind),
        })
    }
}

/// What `tx plugins` lists: the installed plugins in plugin order, then the compiled-in ones no
/// installed plugin replaces in `MANIFEST` order, then the entries of the config that name
/// neither.
pub fn rows(installation: &Installation) -> Vec<Row> {
    let index = &installation.index;
    let activation = &installation.activation;
    let mut rows = Vec::new();
    for state in &activation.plugins {
        let Some(position) = state.installed else {
            continue;
        };
        let installed = &index.plugins[position];
        let shipped = index.roots.first() == Some(&installed.root);
        rows.push(Row {
            id: state.id.clone(),
            status: state.status.clone(),
            detail: activation.detail(state),
            origin: Some(if shipped { SHIPPED } else { USER }),
            directory: Some(installed.directory.clone()),
            manifest: installed.manifest.as_ref().ok().cloned(),
        });
    }
    let linked = known_ids();
    for id in &linked {
        if index.contains(id) {
            continue;
        }
        let enabled = installation
            .entries
            .get(*id)
            .is_none_or(|entry| entry.enabled);
        rows.push(Row {
            id: (*id).to_owned(),
            status: if enabled {
                PluginStatus::Active
            } else {
                PluginStatus::Disabled
            },
            detail: None,
            origin: Some(BUILT_IN),
            directory: None,
            manifest: None,
        });
    }
    for state in &activation.plugins {
        if state.status == PluginStatus::Missing && !linked.contains(&state.id.as_str()) {
            rows.push(Row {
                id: state.id.clone(),
                status: PluginStatus::Missing,
                detail: None,
                origin: None,
                directory: None,
                manifest: None,
            });
        }
    }
    rows
}
