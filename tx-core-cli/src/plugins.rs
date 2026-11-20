//! The plugin switchboard: what `tx plugins` lists, and the warning about shipped plugins that
//! are not this tx's. The `plugins:` section of `config.yaml` is read by `tx_plugin::config`.

use std::path::PathBuf;

use serde_json::{Value, json};
use tx_core::plugin::{Index, Manifest, ShippedStamp};
use tx_core::{Directories, PluginStatus, updates};
pub use tx_plugin::config::{Entry, RETIRED};
use tx_plugin::setup::Installation;

/// The built-in verbs; a plugin may not provide one of these.
pub const RESERVED: &[&str] = tx_plugin::RESERVED_VERBS;

/// Where a listed plugin comes from: the first plugin root, or any other root.
pub const SHIPPED: &str = "shipped";
pub const USER: &str = "user";

/// On stderr, once: the shipped plugins were laid out by another tx, or an install that has
/// data has none. They link the same core as `tx` and read the records directly, so they must
/// come from the same build. Never refuses. Nothing is said when the plugin roots were
/// replaced: the shipped plugins are then not what runs.
pub fn warn_if_shipped_differ(index: &Index, directories: &Directories) {
    match &index.shipped {
        None => {}
        Some(ShippedStamp::By(version)) if version == tx_plugin::VERSION => {}
        Some(ShippedStamp::By(version)) => {
            eprintln!("tx: shipped plugins are from tx {version}: run install --upgrade");
        }
        Some(ShippedStamp::Absent) => match updates::is_fresh(directories) {
            Ok(true) => {}
            Ok(false) => eprintln!("tx: no shipped plugins are installed: run install"),
            Err(error) => eprintln!("tx: {error}"),
        },
        Some(ShippedStamp::Unreadable(why)) => eprintln!("tx: {why}"),
    }
}

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

/// What `tx plugins` lists: the installed plugins in plugin order, then the entries of the
/// config that name none.
pub fn rows(installation: &Installation) -> Vec<Row> {
    let index = &installation.index;
    let activation = &installation.activation;
    activation
        .plugins
        .iter()
        .map(|state| {
            let installed = state.installed.map(|position| &index.plugins[position]);
            Row {
                id: state.id.clone(),
                status: state.status.clone(),
                detail: activation.detail(state),
                origin: installed.map(|installed| {
                    if index.roots.first() == Some(&installed.root) {
                        SHIPPED
                    } else {
                        USER
                    }
                }),
                directory: installed.map(|installed| installed.directory.clone()),
                manifest: installed.and_then(|installed| installed.manifest.as_ref().ok().cloned()),
            }
        })
        .collect()
}
