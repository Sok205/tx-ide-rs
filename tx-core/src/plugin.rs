//! The plugin host. One trait, [`Plugin`]; a [`Registry`] with five slots (engines, event
//! sinks, verbs, updaters, resources); a [`PluginConfig`] the front end fills from its own
//! config format.
//! Core never parses arguments or reads the environment: a [`Verb`] receives argv already split.
//! A resource is a named static blob the core stores and lists but never reads.
//!
//! A plugin reaches a core in one of two ways. Linked into the process, it is handed to
//! [`TxCore::plug`]. Installed on disk as a directory with a [`Manifest`], it runs as its own
//! program: the [`Index`] finds it, the [`Activation`] says whether it is on, and
//! [`TxCore::plug_external`] puts a proxy for each of its verbs into the core.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::sync::Arc;

use serde_json::Value;

use crate::config::CoreConfig;
use crate::core::TxCore;
use crate::engine::LlmEngine;
use crate::error::CoreError;
use crate::events::EventSink;
use crate::updates::Outcome;

pub mod external;
pub mod index;
pub mod manifest;

pub use external::{ExecContext, Launcher};
pub use index::{Activation, ActiveVerb, Entry, Index, Installed, PluginState, Problem};
pub use manifest::{
    EngineMethod, Events, Manifest, ManifestEngine, ManifestError, ManifestInterrupt,
    ManifestOnEvent, ManifestProgram, ManifestVerb, PLUGIN_API, Tier,
};

pub trait Plugin: Send + Sync {
    /// `[a-z0-9-]+`, unique across the manifest; the key in the front end's config and in
    /// `Agent.ext`.
    fn id(&self) -> &'static str;
    fn register(&self, r: &mut Registry) -> Result<(), PluginError>;
}

/// What one plugin contributes. Built per plugin by [`TxCore::plug`].
pub struct Registry {
    engines: Vec<Box<dyn LlmEngine>>,
    sinks: Vec<Box<dyn EventSink>>,
    verbs: Vec<Arc<dyn Verb>>,
    updaters: Vec<Arc<dyn Updater>>,
    resources: BTreeMap<&'static str, &'static [u8]>,
    settings: Value,
    config: CoreConfig,
}

impl Registry {
    /// An empty registry holding `settings` for the plugin about to fill it. For a caller that
    /// runs [`Plugin::register`] itself, to look at or serve what one plugin contributes without
    /// plugging it into a core.
    pub fn new(settings: Value, config: CoreConfig) -> Self {
        Self {
            engines: Vec::new(),
            sinks: Vec::new(),
            verbs: Vec::new(),
            updaters: Vec::new(),
            resources: BTreeMap::new(),
            settings,
            config,
        }
    }

    pub fn engine(&mut self, adapter: Box<dyn LlmEngine>) {
        self.engines.push(adapter);
    }

    pub fn sink(&mut self, sink: Box<dyn EventSink>) {
        self.sinks.push(sink);
    }

    pub fn verb(&mut self, verb: Arc<dyn Verb>) {
        self.verbs.push(verb);
    }

    /// What `tx update` runs for this plugin, after the core's own update steps.
    pub fn updater(&mut self, updater: Arc<dyn Updater>) {
        self.updaters.push(updater);
    }

    /// Publishes a named static blob; the core stores and lists it, never reads it. `name` is
    /// `[A-Za-z0-9_./-]+`, no leading `/`, no `..` segment, unique across plugins. Convention:
    /// `<kind>/<file>`, e.g. `keys/kill.tmux`.
    pub fn resource(
        &mut self,
        name: &'static str,
        bytes: &'static [u8],
    ) -> Result<(), PluginError> {
        validate_resource_name(name)?;
        if self.resources.insert(name, bytes).is_some() {
            return Err(PluginError::DuplicateResource(name.to_owned()));
        }
        Ok(())
    }

    /// This plugin's settings block; `Null` when the config has none.
    pub fn settings(&self) -> &Value {
        &self.settings
    }

    /// The core's config (state dir, socket), for plugins that open a second core in a sink.
    pub fn config(&self) -> &CoreConfig {
        &self.config
    }

    /// The engines registered so far.
    pub fn engines(&self) -> &[Box<dyn LlmEngine>] {
        &self.engines
    }

    /// The event sinks registered so far.
    pub fn sinks(&self) -> &[Box<dyn EventSink>] {
        &self.sinks
    }

    /// The verbs registered so far.
    pub fn verbs(&self) -> &[Arc<dyn Verb>] {
        &self.verbs
    }

    /// The updaters registered so far.
    pub fn updaters(&self) -> &[Arc<dyn Updater>] {
        &self.updaters
    }
}

/// A named static blob published by a plugin, listed by [`TxCore::resources`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resource {
    pub plugin: String,
    pub name: String,
    pub bytes: &'static [u8],
}

/// A command a front end can dispatch to. `argv` is everything after the verb name. The name
/// and the usage are borrowed from the verb: a linked plugin's are constants, a proxy's come
/// from a manifest.
pub trait Verb: Send + Sync {
    fn name(&self) -> &str;
    /// One usage line, shown under the front end's own verbs.
    fn usage(&self) -> &str;
    /// Returns the exit code. Usage mistakes: write to `io.stderr`, return `Ok(2)`.
    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError>;
}

/// A plugin's part of `tx update`: brings the files it owns up to this build (generated
/// config, materialised resources, its own one-shot steps). Runs on every `tx update`, so it
/// must be idempotent. What it did goes to `io.stdout`, one line per change.
pub trait Updater: Send + Sync {
    fn update(&self, core: &TxCore, io: &mut VerbIo<'_>) -> Result<Outcome, VerbError>;

    /// The plugin stopped being active (it was disabled, or left the installed set): take back
    /// what `update` generated outside the plugin's own data. Called with the plugin registered
    /// whether or not it is enabled, and possibly more than once, so it must be safe to repeat.
    /// The default has nothing to take back.
    fn retire(&self, _core: &TxCore, _io: &mut VerbIo<'_>) -> Result<Outcome, VerbError> {
        Ok(Outcome::default())
    }
}

pub struct VerbIo<'a> {
    pub stdin: &'a mut dyn Read,
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
}

#[derive(Debug, thiserror::Error)]
pub enum VerbError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("{0}")]
    Failed(String),
}

/// Which plugins to skip and what settings to hand each one. Ids must name plugins in the
/// manifest handed to [`TxCore::plug`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginConfig {
    pub disabled: BTreeSet<String>,
    pub settings: BTreeMap<String, Value>,
    /// Verb names the front end owns; a plugin registering one is refused.
    pub reserved: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    Active,
    /// The config says `enabled: false`.
    Disabled,
    /// Installed, and it cannot load: why.
    Error(String),
    /// An entry of the config that names no plugin.
    Missing,
}

impl PluginStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PluginStatus::Active => "active",
            PluginStatus::Disabled => "disabled",
            PluginStatus::Error(_) => "error",
            PluginStatus::Missing => "missing",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginError {
    #[error("invalid plugin id {0:?} (allowed: [a-z0-9-]+)")]
    InvalidId(String),
    #[error("plugin {0:?} is listed twice")]
    DuplicatePlugin(String),
    #[error("no plugin named {0:?}")]
    Unknown(String),
    #[error("verb {verb:?} is registered by both {first:?} and {second:?}")]
    DuplicateVerb {
        verb: String,
        first: String,
        second: String,
    },
    #[error("plugin {plugin:?} registers verb {verb:?}, which the front end owns")]
    ReservedVerb { verb: String, plugin: String },
    #[error("resource {0:?} is published twice")]
    DuplicateResource(String),
    #[error(
        "invalid resource name {0:?} (allowed: [A-Za-z0-9_./-]+, no leading '/', no '..' segment)"
    )]
    InvalidResourceName(String),
    #[error("plugin {plugin:?}: {source}")]
    Engine { plugin: String, source: CoreError },
    #[error("plugin {plugin:?}: {reason}")]
    Register { plugin: String, reason: String },
}

fn validate_id(id: &str) -> Result<(), PluginError> {
    let ok = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(())
    } else {
        Err(PluginError::InvalidId(id.to_owned()))
    }
}

fn validate_resource_name(name: &str) -> Result<(), PluginError> {
    let ok = !name.is_empty()
        && !name.starts_with('/')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'/' | b'-'))
        && name.split('/').all(|segment| segment != "..");
    if ok {
        Ok(())
    } else {
        Err(PluginError::InvalidResourceName(name.to_owned()))
    }
}

/// The host side, kept in `TxCore`.
#[derive(Default)]
pub(crate) struct Host {
    /// name → (plugin id, verb)
    verbs: BTreeMap<String, (String, Arc<dyn Verb>)>,
    /// (plugin id, updater), manifest order.
    updaters: Vec<(String, Arc<dyn Updater>)>,
    /// Manifest order, then name order within a plugin.
    resources: Vec<Resource>,
    status: Vec<(String, PluginStatus)>,
}

impl Host {
    /// A host that already lists the core's shipped roles (`crate::roles::SHIPPED`), ahead of
    /// whatever the plugins register.
    pub(crate) fn with_shipped_roles() -> Self {
        let mut host = Host::default();
        for &(name, bytes) in crate::roles::SHIPPED {
            host.resources.push(Resource {
                plugin: crate::roles::PLUGIN_ID.to_owned(),
                name: name.to_owned(),
                bytes,
            });
        }
        host
    }
}

/// One plugin after `register`: `None` when the config disabled it.
struct Loaded {
    id: String,
    registry: Option<Registry>,
}

/// Validates ids and config, runs every enabled plugin's `register` against a fresh
/// [`Registry`]. Touches nothing outside memory.
fn load(
    plugins: &[Box<dyn Plugin>],
    cfg: &PluginConfig,
    config: &CoreConfig,
) -> Result<Vec<Loaded>, PluginError> {
    let ids: BTreeSet<&str> = plugins.iter().map(|p| p.id()).collect();
    for id in cfg.disabled.iter().chain(cfg.settings.keys()) {
        if !ids.contains(id.as_str()) {
            return Err(PluginError::Unknown(id.clone()));
        }
    }
    let mut seen = BTreeSet::new();
    let mut loaded = Vec::with_capacity(plugins.len());
    for plugin in plugins {
        let id = plugin.id();
        validate_id(id)?;
        if !seen.insert(id) {
            return Err(PluginError::DuplicatePlugin(id.to_owned()));
        }
        if cfg.disabled.contains(id) {
            loaded.push(Loaded {
                id: id.to_owned(),
                registry: None,
            });
            continue;
        }
        let mut registry = Registry::new(
            cfg.settings.get(id).cloned().unwrap_or(Value::Null),
            config.clone(),
        );
        plugin.register(&mut registry)?;
        loaded.push(Loaded {
            id: id.to_owned(),
            registry: Some(registry),
        });
    }
    Ok(loaded)
}

type VerbTable = BTreeMap<String, (String, Arc<dyn Verb>)>;

fn insert_verb(
    table: &mut VerbTable,
    cfg: &PluginConfig,
    plugin: &str,
    verb: Arc<dyn Verb>,
) -> Result<(), PluginError> {
    let name = verb.name().to_owned();
    if cfg.reserved.contains(&name) {
        return Err(PluginError::ReservedVerb {
            verb: name,
            plugin: plugin.to_owned(),
        });
    }
    if let Some((first, _)) = table.get(&name) {
        return Err(PluginError::DuplicateVerb {
            verb: name,
            first: first.clone(),
            second: plugin.to_owned(),
        });
    }
    table.insert(name, (plugin.to_owned(), verb));
    Ok(())
}

/// The verbs [`TxCore::plug`] would expose for this manifest and config, without a core: for
/// usage text and "is this verb known?" checks that must not create the state dir. Runs every
/// enabled plugin's `register`; engines and sinks are dropped.
pub fn preview_verbs(
    plugins: &[Box<dyn Plugin>],
    cfg: &PluginConfig,
    config: &CoreConfig,
) -> Result<Vec<Arc<dyn Verb>>, PluginError> {
    let mut table = VerbTable::new();
    for loaded in load(plugins, cfg, config)? {
        if let Some(registry) = loaded.registry {
            for verb in registry.verbs {
                insert_verb(&mut table, cfg, &loaded.id, verb)?;
            }
        }
    }
    Ok(table.into_values().map(|(_, verb)| verb).collect())
}

impl TxCore {
    /// Registers every plugin not in `cfg.disabled`, in order. Fails on the first problem; the
    /// core may then hold a partial registration and the caller should exit.
    pub fn plug(
        &mut self,
        plugins: Vec<Box<dyn Plugin>>,
        cfg: &PluginConfig,
    ) -> Result<(), PluginError> {
        for loaded in load(&plugins, cfg, self.config())? {
            let Some(registry) = loaded.registry else {
                self.host.status.push((loaded.id, PluginStatus::Disabled));
                continue;
            };
            for adapter in registry.engines {
                self.register_engine(adapter)
                    .map_err(|source| PluginError::Engine {
                        plugin: loaded.id.clone(),
                        source,
                    })?;
            }
            for sink in registry.sinks {
                self.subscribe(sink);
            }
            for verb in registry.verbs {
                insert_verb(&mut self.host.verbs, cfg, &loaded.id, verb)?;
            }
            for updater in registry.updaters {
                self.host.updaters.push((loaded.id.clone(), updater));
            }
            for (name, bytes) in registry.resources {
                if self.host.resources.iter().any(|r| r.name == name) {
                    return Err(PluginError::DuplicateResource(name.to_owned()));
                }
                self.host.resources.push(Resource {
                    plugin: loaded.id.clone(),
                    name: name.to_owned(),
                    bytes,
                });
            }
            self.host.status.push((loaded.id, PluginStatus::Active));
        }
        Ok(())
    }

    /// Brings in the plugins that run as their own programs, after [`Self::plug`] registered
    /// the ones linked into this process (`hosted_ids`, which are left alone here). Each step
    /// lives with the proxies it creates: the installed plugins join [`Self::plugin_status`],
    /// then every verb of an active plugin becomes a [`Verb`] that starts the plugin's program.
    /// Where such a verb and one registered in-process have the same name, the activation's
    /// owner is the one [`Self::verb`] answers, as the front end would run it.
    pub fn plug_external(
        &mut self,
        index: &Index,
        activation: &Activation,
        context: ExecContext,
        hosted_ids: &[&str],
    ) -> Result<(), PluginError> {
        let context = Arc::new(context);
        external::list(self, activation, hosted_ids);
        external::plug_verbs(self, index, activation, &context, hosted_ids);
        Ok(())
    }

    pub fn verb(&self, name: &str) -> Option<Arc<dyn Verb>> {
        self.host.verbs.get(name).map(|(_, verb)| Arc::clone(verb))
    }

    /// Every plugin verb, by name.
    pub fn verbs(&self) -> Vec<Arc<dyn Verb>> {
        self.host
            .verbs
            .values()
            .map(|(_, verb)| Arc::clone(verb))
            .collect()
    }

    /// The active plugins' updaters with their plugin id, in manifest order.
    pub fn updaters(&self) -> Vec<(String, Arc<dyn Updater>)> {
        self.host.updaters.clone()
    }

    /// Resources of active plugins whose name starts with `prefix`, in manifest order then name
    /// order.
    pub fn resources(&self, prefix: &str) -> Vec<Resource> {
        self.host
            .resources
            .iter()
            .filter(|r| r.name.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// [`Self::resources`] without the core's own shipped roles: what the plugins registered.
    pub fn plugin_resources(&self, prefix: &str) -> Vec<Resource> {
        self.resources(prefix)
            .into_iter()
            .filter(|r| r.plugin != crate::roles::PLUGIN_ID)
            .collect()
    }

    /// The plugins linked into this process in manifest order, then the installed ones that
    /// run as their own programs in plugin order.
    pub fn plugin_status(&self) -> &[(String, PluginStatus)] {
        &self.host.status
    }
}
