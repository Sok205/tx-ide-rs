//! `plugin.yaml`, api 1: what a plugin on disk declares. Every key not listed here is an error,
//! and a manifest written for an `api` this tx does not support is refused with that one
//! message, whatever else it holds. The same types are the JSON the index keeps.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::engine::Key;
use crate::events::EventType;
use crate::state::LlmEngineState;

/// The plugin API this tx implements: the manifest schema and how a plugin's programs are run.
pub const PLUGIN_API: u32 = 1;
/// In a plugin's directory: its manifest.
pub const MANIFEST_FILE: &str = "plugin.yaml";
/// In a plugin's directory: the programs its manifest names.
pub const BIN_DIR: &str = "bin";
/// What a resource name starts with: the directory under `<data>` it is published to.
pub const RESOURCE_KINDS: &[&str] = &["keys", "conf", "bin", "nvim", "agents", "definitions"];

/// A manifest that cannot be used, at `path` and, when the YAML parser gave one, a 1-based
/// `line`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}{}: {message}", path.display(), line.map(|line| format!(":{line}")).unwrap_or_default())]
pub struct ManifestError {
    pub path: PathBuf,
    pub line: Option<usize>,
    pub message: String,
}

impl ManifestError {
    /// Why, for a reader who already knows the file: the message, after `line N: ` when the
    /// line is known.
    pub fn reason(&self) -> String {
        match self.line {
            Some(line) => format!("line {line}: {}", self.message),
            None => self.message.clone(),
        }
    }
}

/// One `plugin.yaml`, read and checked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The plugin API the manifest is written for.
    pub api: u32,
    /// `[a-z0-9-]+`, the name of the plugin's directory; the key in `config.yaml` and in
    /// `Agent.ext`.
    pub id: String,
    /// One line.
    pub description: String,
    /// What a preset selects.
    #[serde(default)]
    pub tier: Tier,
    /// Plugin ids it is of no use without.
    #[serde(default)]
    pub requires: Vec<String>,
    /// Programs it runs, looked up on `PATH` by `tx check`.
    #[serde(default)]
    pub needs: Vec<String>,
    /// Its default settings, spelled out: only ever used to seed `config.yaml`. `Null` when
    /// absent.
    #[serde(default)]
    pub settings: Value,
    #[serde(default)]
    pub verbs: Vec<ManifestVerb>,
    #[serde(default)]
    pub engine: Option<ManifestEngine>,
    #[serde(default, rename = "on-event")]
    pub on_event: Option<ManifestOnEvent>,
    /// What `tx update` runs for the plugin, and once more, retiring, when it stops being
    /// active.
    #[serde(default)]
    pub update: Option<ManifestProgram>,
    /// The files it publishes, each named by its path inside the plugin.
    #[serde(default)]
    pub resources: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Core,
    Ide,
    #[default]
    Extra,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Core => "core",
            Tier::Ide => "ide",
            Tier::Extra => "extra",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestVerb {
    /// `[a-z0-9_-]+`; a leading `_` marks it internal.
    pub name: String,
    /// One line, shown by `tx help`.
    pub usage: String,
    /// A file name under `bin/`.
    pub bin: String,
    /// Words put before the caller's arguments.
    #[serde(default)]
    pub args: Vec<String>,
}

/// A program of the plugin and the words it is started with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestProgram {
    /// A file name under `bin/`.
    pub bin: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "EngineKeys")]
pub struct ManifestEngine {
    /// `[a-z0-9-]{1,31}`: what `--engine`, `config.yaml` and records name.
    pub kind: String,
    /// The `ext` key the engine owns; the kind when the manifest leaves it out.
    pub ext_key: String,
    /// The protocol server.
    pub bin: String,
    pub args: Vec<String>,
    /// Its own hook entry, instead of `txi hook`.
    pub hook: Option<ManifestProgram>,
    /// The optional methods it implements; core never asks it for another.
    pub methods: Vec<EngineMethod>,
    /// A fixed answer to `interrupt`, instead of the method.
    pub interrupt: Option<ManifestInterrupt>,
    /// Printed by `tx definitions` and `tx check`.
    pub notes: Vec<String>,
}

/// The `engine` section as it is written: `ext_key` may be left out.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EngineKeys {
    kind: String,
    ext_key: Option<String>,
    bin: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    hook: Option<ManifestProgram>,
    #[serde(default)]
    methods: Vec<EngineMethod>,
    #[serde(default)]
    interrupt: Option<ManifestInterrupt>,
    #[serde(default)]
    notes: Vec<String>,
}

impl From<EngineKeys> for ManifestEngine {
    fn from(keys: EngineKeys) -> Self {
        Self {
            ext_key: keys.ext_key.unwrap_or_else(|| keys.kind.clone()),
            kind: keys.kind,
            bin: keys.bin,
            args: keys.args,
            hook: keys.hook,
            methods: keys.methods,
            interrupt: keys.interrupt,
            notes: keys.notes,
        }
    }
}

/// An engine method core calls only when the manifest lists it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineMethod {
    Prepare,
    ReceiveMessage,
    Interrupt,
    Archive,
    Restore,
    Warnings,
    Check,
    Tools,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestInterrupt {
    /// The key that aborts the running turn.
    #[serde(with = "key_name")]
    pub key: Key,
    /// The state the agent is in once the key landed; `None`: a hook will say.
    #[serde(default)]
    pub state: Option<LlmEngineState>,
}

/// A [`Key`] by the name tmux gives it.
mod key_name {
    use serde::de::Error;

    use super::{Deserialize, Deserializer, Key, Serializer};

    const KEYS: [Key; 4] = [Key::Escape, Key::Enter, Key::ControlC, Key::ControlD];

    pub fn serialize<S: Serializer>(key: &Key, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(key.tmux_name())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Key, D::Error> {
        let name = String::deserialize(deserializer)?;
        KEYS.into_iter()
            .find(|key| key.tmux_name() == name)
            .ok_or_else(|| {
                let known: Vec<&str> = KEYS.iter().map(|key| key.tmux_name()).collect();
                D::Error::custom(format!(
                    "unknown key {name:?} (known: {})",
                    known.join(", ")
                ))
            })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestOnEvent {
    pub events: Events,
    /// The program run once per subscribed event.
    pub bin: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Seconds the program may take, 1 to 30.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

pub const DEFAULT_EVENT_TIMEOUT: u64 = 5;

fn default_timeout() -> u64 {
    DEFAULT_EVENT_TIMEOUT
}

/// The events an `on-event` program is run for: `all`, or a list of event type names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Events {
    All,
    Only(Vec<EventType>),
}

impl Events {
    pub fn includes(&self, type_: EventType) -> bool {
        match self {
            Events::All => true,
            Events::Only(types) => types.contains(&type_),
        }
    }
}

const ALL_EVENTS: &str = "all";

fn event_type(name: &str) -> Result<EventType, String> {
    EventType::ALL
        .iter()
        .copied()
        .find(|type_| type_.as_str() == name)
        .ok_or_else(|| {
            let known: Vec<&str> = EventType::ALL.iter().map(|type_| type_.as_str()).collect();
            format!("unknown event type {name:?} (known: {})", known.join(", "))
        })
}

impl Serialize for Events {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Events::All => serializer.serialize_str(ALL_EVENTS),
            Events::Only(types) => serializer.collect_seq(types.iter().map(|type_| type_.as_str())),
        }
    }
}

impl<'de> Deserialize<'de> for Events {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Events;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a list of event types, or `all`")
            }

            fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Events, E> {
                if text == ALL_EVENTS {
                    Ok(Events::All)
                } else {
                    Err(E::custom(format!(
                        "events is a list of event types or `all`, got {text:?}"
                    )))
                }
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Events, A::Error> {
                let mut types = Vec::new();
                while let Some(name) = sequence.next_element::<String>()? {
                    types.push(event_type(&name).map_err(serde::de::Error::custom)?);
                }
                Ok(Events::Only(types))
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

/// Only the `api` of a manifest, read before anything else is looked at.
#[derive(Deserialize)]
struct ApiProbe {
    api: Option<i64>,
}

/// serde_norway ends its messages with ` at line L column C`; the line is reported apart.
fn strip_location(message: &str) -> String {
    match message.rfind(" at line ") {
        Some(index) => message[..index].to_owned(),
        None => message.to_owned(),
    }
}

fn one_line(text: &str) -> bool {
    !text.contains(['\n', '\r'])
}

/// `bin` names a file directly under `bin/`.
fn check_bin(owner: &str, bin: &str) -> Result<(), String> {
    if bin.is_empty() || bin.contains('/') || bin == "." || bin == ".." {
        return Err(format!(
            "{owner}: bin {bin:?} must be the name of a file under {BIN_DIR}/"
        ));
    }
    Ok(())
}

/// `<kind>/<path>`, where a key fragment is `keys/<name>.tmux`.
fn check_resource(name: &str) -> Result<(), String> {
    super::validate_resource_name(name).map_err(|error| error.to_string())?;
    let (kind, path) = name.split_once('/').unwrap_or((name, ""));
    if !RESOURCE_KINDS.contains(&kind) || path.is_empty() {
        return Err(format!(
            "resource {name:?} must be a file under one of {}",
            RESOURCE_KINDS.join(", ")
        ));
    }
    if kind == "keys"
        && (path.contains('/') || path.len() <= ".tmux".len() || !path.ends_with(".tmux"))
    {
        return Err(format!(
            "resource {name:?}: a key fragment is keys/<name>.tmux"
        ));
    }
    Ok(())
}

impl Manifest {
    /// Reads `<plugin directory>/plugin.yaml` and checks every rule of api 1. An `api` this tx
    /// does not support is the error whatever else the file holds.
    pub fn read(path: &Path) -> Result<Manifest, ManifestError> {
        let error = |line: Option<usize>, message: String| ManifestError {
            path: path.to_owned(),
            line,
            message,
        };
        let text =
            std::fs::read_to_string(path).map_err(|source| error(None, source.to_string()))?;
        if let Ok(ApiProbe { api: Some(api) }) = serde_norway::from_str(&text)
            && api != i64::from(PLUGIN_API)
        {
            return Err(error(
                None,
                format!("api {api} is not supported (this tx: {PLUGIN_API})"),
            ));
        }
        let manifest: Manifest = serde_norway::from_str(&text).map_err(|source| {
            error(
                source.location().map(|location| location.line()),
                strip_location(&source.to_string()),
            )
        })?;
        let directory = path
            .parent()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        manifest
            .check(&directory)
            .map_err(|message| error(None, message))?;
        Ok(manifest)
    }

    /// The rules the types do not hold, for a manifest in the directory named `directory`; the
    /// first one broken.
    fn check(&self, directory: &str) -> Result<(), String> {
        super::validate_id(&self.id).map_err(|error| error.to_string())?;
        if self.id == crate::roles::PLUGIN_ID {
            return Err(format!("the id {:?} is core's own", self.id));
        }
        if self.id != directory {
            return Err(format!(
                "id {:?} is not the name of its directory, {directory:?}",
                self.id
            ));
        }
        if !one_line(&self.description) {
            return Err("description must be one line".to_owned());
        }
        for verb in &self.verbs {
            let name = &verb.name;
            let named = !name.is_empty()
                && name.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                });
            if !named {
                return Err(format!("verb name {name:?} must match [a-z0-9_-]+"));
            }
            if !one_line(&verb.usage) {
                return Err(format!("verb {name:?}: usage must be one line"));
            }
            check_bin(&format!("verb {name:?}"), &verb.bin)?;
        }
        if let Some(engine) = &self.engine {
            let kind = &engine.kind;
            if super::validate_id(kind).is_err() || kind.len() > 31 {
                return Err(format!("engine kind {kind:?} must match [a-z0-9-]{{1,31}}"));
            }
            if super::validate_id(&engine.ext_key).is_err() {
                return Err(format!(
                    "engine ext_key {:?} must match [a-z0-9-]+",
                    engine.ext_key
                ));
            }
            check_bin("engine", &engine.bin)?;
            if let Some(hook) = &engine.hook {
                check_bin("engine hook", &hook.bin)?;
            }
            if engine.interrupt.is_some() && engine.methods.contains(&EngineMethod::Interrupt) {
                return Err(
                    "engine: interrupt is either a method or a fixed answer, not both".to_owned(),
                );
            }
        }
        if let Some(on_event) = &self.on_event {
            check_bin("on-event", &on_event.bin)?;
            if !(1..=30).contains(&on_event.timeout) {
                return Err(format!(
                    "on-event: timeout must be 1 to 30 seconds, got {}",
                    on_event.timeout
                ));
            }
        }
        if let Some(update) = &self.update {
            check_bin("update", &update.bin)?;
        }
        for resource in &self.resources {
            check_resource(resource)?;
        }
        Ok(())
    }

    /// Every program name the manifest uses, each once, in the order of its sections.
    pub fn bins(&self) -> Vec<&str> {
        let mut bins: Vec<&str> = Vec::new();
        let engine = self.engine.iter().flat_map(|engine| {
            std::iter::once(engine.bin.as_str())
                .chain(engine.hook.iter().map(|hook| hook.bin.as_str()))
        });
        let names = self
            .verbs
            .iter()
            .map(|verb| verb.bin.as_str())
            .chain(engine)
            .chain(self.on_event.iter().map(|on_event| on_event.bin.as_str()))
            .chain(self.update.iter().map(|update| update.bin.as_str()));
        for name in names {
            if !bins.contains(&name) {
                bins.push(name);
            }
        }
        bins
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes `text` as the manifest of a plugin directory named `directory`; the manifest's
    /// path.
    fn write(root: &Path, directory: &str, text: &str) -> PathBuf {
        let directory = root.join(directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(MANIFEST_FILE);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn read(directory: &str, text: &str) -> Result<Manifest, ManifestError> {
        let root = tempfile::tempdir().unwrap();
        Manifest::read(&write(root.path(), directory, text))
    }

    /// The message of the error reading `text` as the manifest of `hello`.
    fn refused(text: &str) -> String {
        read("hello", text).unwrap_err().message
    }

    const MINIMAL: &str = "api: 1\nid: hello\ndescription: Says hello.\n";

    const FULL: &str = "\
api: 1
id: kill
description: Stop the agent or close the view in the active pane, after a y/n prompt.
tier: core
requires: [ide]
needs: [fzf]
settings: { editor: nvim }
verbs:
  - name: kill
    usage: \"kill <name>   stop a session, or close a view (prefix+X)\"
    bin: tx-kill
    args: [kill]
  - name: _kill-pane
    usage: \"_kill-pane PANE   (internal)\"
    bin: tx-kill
engine:
  kind: claude
  bin: tx-engine-claude
  args: [--tx-engine]
  hook: { bin: tx-engine-claude, args: [--tx-hook] }
  methods: [prepare, archive, restore, receive_message]
  interrupt: { key: Escape, state: idle }
  notes: [\"claude adds to every agent's tools: Skill, ToolSearch\"]
on-event:
  events: [state-changed, message-sent]
  bin: notify
  timeout: 7
update:
  bin: tx-ide
  args: [--tx-update]
resources:
  - keys/kill.tmux
  - bin/tmux-kill-tx-session
  - definitions/skills/tx-kill/SKILL.md
";

    #[test]
    fn a_minimal_manifest_takes_every_default() {
        let manifest = read("hello", MINIMAL).unwrap();
        assert_eq!(manifest.api, PLUGIN_API);
        assert_eq!(manifest.id, "hello");
        assert_eq!(manifest.description, "Says hello.");
        assert_eq!(manifest.tier, Tier::Extra);
        assert_eq!(manifest.settings, Value::Null);
        assert!(manifest.requires.is_empty() && manifest.needs.is_empty());
        assert!(manifest.verbs.is_empty() && manifest.resources.is_empty());
        assert_eq!(manifest.engine, None);
        assert_eq!(manifest.on_event, None);
        assert_eq!(manifest.update, None);
        assert!(manifest.bins().is_empty());
    }

    #[test]
    fn every_section_is_read() {
        let manifest = read("kill", FULL).unwrap();
        assert_eq!(manifest.tier, Tier::Core);
        assert_eq!(manifest.requires, ["ide"]);
        assert_eq!(manifest.needs, ["fzf"]);
        assert_eq!(manifest.settings, serde_json::json!({"editor": "nvim"}));
        assert_eq!(
            manifest.verbs[0],
            ManifestVerb {
                name: "kill".into(),
                usage: "kill <name>   stop a session, or close a view (prefix+X)".into(),
                bin: "tx-kill".into(),
                args: vec!["kill".into()],
            }
        );
        assert!(manifest.verbs[1].args.is_empty());
        let engine = manifest.engine.as_ref().unwrap();
        assert_eq!(engine.kind, "claude");
        assert_eq!(engine.ext_key, "claude", "the kind, when left out");
        assert_eq!(engine.args, ["--tx-engine"]);
        assert_eq!(
            engine.hook,
            Some(ManifestProgram {
                bin: "tx-engine-claude".into(),
                args: vec!["--tx-hook".into()],
            })
        );
        assert_eq!(
            engine.methods,
            [
                EngineMethod::Prepare,
                EngineMethod::Archive,
                EngineMethod::Restore,
                EngineMethod::ReceiveMessage,
            ]
        );
        assert_eq!(
            engine.interrupt,
            Some(ManifestInterrupt {
                key: Key::Escape,
                state: Some(LlmEngineState::Idle),
            })
        );
        assert_eq!(engine.notes.len(), 1);
        let on_event = manifest.on_event.as_ref().unwrap();
        assert_eq!(
            on_event.events,
            Events::Only(vec![EventType::StateChanged, EventType::MessageSent])
        );
        assert!(on_event.events.includes(EventType::MessageSent));
        assert!(!on_event.events.includes(EventType::Created));
        assert_eq!(on_event.timeout, 7);
        assert_eq!(
            manifest.update,
            Some(ManifestProgram {
                bin: "tx-ide".into(),
                args: vec!["--tx-update".into()],
            })
        );
        assert_eq!(manifest.resources.len(), 3);
        assert_eq!(
            manifest.bins(),
            ["tx-kill", "tx-engine-claude", "notify", "tx-ide"]
        );
    }

    #[test]
    fn on_event_defaults_and_all() {
        let manifest = read(
            "hello",
            &format!("{MINIMAL}on-event:\n  events: all\n  bin: sink\n"),
        )
        .unwrap();
        let on_event = manifest.on_event.unwrap();
        assert_eq!(on_event.events, Events::All);
        assert!(on_event.events.includes(EventType::ArchiveFailed));
        assert_eq!(on_event.timeout, DEFAULT_EVENT_TIMEOUT);
        assert!(on_event.args.is_empty());
    }

    #[test]
    fn an_engine_may_name_its_ext_key_and_leave_the_rest_out() {
        let manifest = read(
            "hello",
            &format!("{MINIMAL}engine:\n  kind: fake\n  ext_key: fake-data\n  bin: engine\n"),
        )
        .unwrap();
        let engine = manifest.engine.unwrap();
        assert_eq!(engine.ext_key, "fake-data");
        assert!(engine.args.is_empty() && engine.methods.is_empty() && engine.notes.is_empty());
        assert_eq!(engine.hook, None);
        assert_eq!(engine.interrupt, None);
    }

    #[test]
    fn the_json_of_the_index_reads_back_the_same() {
        for (directory, text) in [("hello", MINIMAL), ("kill", FULL)] {
            let manifest = read(directory, text).unwrap();
            let json = serde_json::to_string(&manifest).unwrap();
            assert_eq!(
                serde_json::from_str::<Manifest>(&json).unwrap(),
                manifest,
                "{json}"
            );
        }
        let all = read(
            "hello",
            &format!("{MINIMAL}on-event:\n  events: all\n  bin: sink\n"),
        )
        .unwrap();
        let json = serde_json::to_string(&all).unwrap();
        assert!(json.contains(r#""on-event":{"events":"all""#), "{json}");
        assert_eq!(serde_json::from_str::<Manifest>(&json).unwrap(), all);
    }

    #[test]
    fn an_unsupported_api_is_the_error_whatever_else_is_wrong() {
        for text in [
            "api: 2\nid: hello\ndescription: Says hello.\n",
            "api: 2\nid: Not An Id\ncolour: red\n",
            "api: 0\n",
        ] {
            let error = read("hello", text).unwrap_err();
            let api = text.as_bytes()[5] as char;
            assert_eq!(
                error.message,
                format!("api {api} is not supported (this tx: 1)"),
                "{text:?}"
            );
            assert_eq!(error.line, None);
            assert_eq!(error.reason(), error.message);
        }
    }

    #[test]
    fn an_error_names_the_file_and_the_line_the_parser_gave() {
        let root = tempfile::tempdir().unwrap();
        let path = write(root.path(), "hello", &format!("{MINIMAL}colour: red\n"));
        let error = Manifest::read(&path).unwrap_err();
        assert_eq!(error.line, Some(4));
        assert!(
            error.message.starts_with("unknown field `colour`"),
            "{error}"
        );
        assert_eq!(
            error.to_string(),
            format!("{}:4: {}", path.display(), error.message)
        );
        assert_eq!(error.reason(), format!("line 4: {}", error.message));

        let path = write(root.path(), "other", MINIMAL);
        let error = Manifest::read(&path).unwrap_err();
        assert_eq!(error.line, None);
        assert_eq!(
            error.to_string(),
            format!(
                "{}: id \"hello\" is not the name of its directory, \"other\"",
                path.display()
            )
        );

        let missing = root.path().join("gone").join(MANIFEST_FILE);
        let error = Manifest::read(&missing).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with(&format!("{}: ", missing.display())),
            "{error}"
        );
    }

    #[test]
    fn unknown_keys_are_refused_in_every_section() {
        for (text, key) in [
            (format!("{MINIMAL}colour: red\n"), "colour"),
            (
                format!(
                    "{MINIMAL}verbs:\n  - name: hello\n    usage: hello\n    bin: hello\n    alias: hi\n"
                ),
                "alias",
            ),
            (
                format!("{MINIMAL}engine:\n  kind: fake\n  bin: engine\n  model: x\n"),
                "model",
            ),
            (
                format!(
                    "{MINIMAL}engine:\n  kind: fake\n  bin: engine\n  hook: {{ bin: engine, when: now }}\n"
                ),
                "when",
            ),
            (
                format!(
                    "{MINIMAL}engine:\n  kind: fake\n  bin: engine\n  interrupt: {{ key: Escape, then: idle }}\n"
                ),
                "then",
            ),
            (
                format!("{MINIMAL}on-event:\n  events: all\n  bin: sink\n  retry: 2\n"),
                "retry",
            ),
            (
                format!("{MINIMAL}update:\n  bin: update\n  when: always\n"),
                "when",
            ),
        ] {
            let message = refused(&text);
            assert!(
                message.contains(&format!("unknown field `{key}`")),
                "{text:?}: {message}"
            );
        }
    }

    #[test]
    fn required_keys_and_value_types_are_checked() {
        for (text, needle) in [
            ("", "missing field `api`"),
            ("api: 1\ndescription: x\n", "missing field `id`"),
            ("api: 1\nid: hello\n", "missing field `description`"),
            ("api: one\nid: hello\ndescription: x\n", "invalid type"),
            ("- api\n", "invalid type"),
            ("api: 1\nid: [1, 2\n", "invalid type"),
        ] {
            let message = refused(text);
            assert!(message.contains(needle), "{text:?}: {message}");
        }
        for (addition, needle) in [
            ("tier: gold\n", "unknown variant `gold`"),
            (
                "verbs:\n  - name: hello\n    bin: hello\n",
                "missing field `usage`",
            ),
            (
                "engine:\n  kind: fake\n  bin: engine\n  methods: [start]\n",
                "unknown variant `start`",
            ),
            (
                "engine:\n  kind: fake\n  bin: engine\n  interrupt: { key: F1 }\n",
                "unknown key \"F1\" (known: Escape, Enter, C-c, C-d)",
            ),
            (
                "engine:\n  kind: fake\n  bin: engine\n  interrupt: { key: Escape, state: asleep }\n",
                "unknown variant `asleep`",
            ),
            (
                "on-event:\n  events: [created, vanished]\n  bin: sink\n",
                "unknown event type \"vanished\" (known: created, forked,",
            ),
            (
                "on-event:\n  events: some\n  bin: sink\n",
                "events is a list of event types or `all`, got \"some\"",
            ),
        ] {
            let message = refused(&format!("{MINIMAL}{addition}"));
            assert!(message.contains(needle), "{addition:?}: {message}");
        }
    }

    #[test]
    fn the_id_is_a_plugin_id_the_directory_name_and_not_cores() {
        for id in ["Hello", "hello world", "hello_2", "\"\""] {
            let error = read("hello", &format!("api: 1\nid: {id}\ndescription: x\n")).unwrap_err();
            assert!(error.message.contains("invalid plugin id"), "{id}: {error}");
        }
        assert_eq!(
            read("tx-core", "api: 1\nid: tx-core\ndescription: x\n")
                .unwrap_err()
                .message,
            "the id \"tx-core\" is core's own"
        );
        assert_eq!(
            read("hello2", MINIMAL).unwrap_err().message,
            "id \"hello\" is not the name of its directory, \"hello2\""
        );
    }

    #[test]
    fn the_rules_of_each_section_are_checked() {
        let verb = |name: &str, usage: &str, bin: &str| {
            format!("verbs:\n  - name: {name}\n    usage: {usage}\n    bin: {bin}\n")
        };
        let engine = |keys: &str| format!("engine:\n  kind: fake\n  bin: engine\n{keys}");
        for (addition, message) in [
            (
                "description: \"two\\nlines\"\n".to_owned(),
                "description must be one line",
            ),
            (
                verb("Hello", "x", "hello"),
                "verb name \"Hello\" must match [a-z0-9_-]+",
            ),
            (
                verb("\"\"", "x", "hello"),
                "verb name \"\" must match [a-z0-9_-]+",
            ),
            (
                verb("hello", "\"a\\nb\"", "hello"),
                "verb \"hello\": usage must be one line",
            ),
            (
                verb("hello", "x", "bin/hello"),
                "verb \"hello\": bin \"bin/hello\" must be the name of a file under bin/",
            ),
            (
                verb("hello", "x", ".."),
                "verb \"hello\": bin \"..\" must be the name of a file under bin/",
            ),
            (
                "engine:\n  kind: Fake\n  bin: engine\n".to_owned(),
                "engine kind \"Fake\" must match [a-z0-9-]{1,31}",
            ),
            (
                format!("engine:\n  kind: {}\n  bin: engine\n", "k".repeat(32)),
                "must match [a-z0-9-]{1,31}",
            ),
            (
                engine("  ext_key: Fake Data\n"),
                "engine ext_key \"Fake Data\" must match [a-z0-9-]+",
            ),
            (
                "engine:\n  kind: fake\n  bin: /usr/bin/engine\n".to_owned(),
                "engine: bin \"/usr/bin/engine\" must be the name of a file under bin/",
            ),
            (
                engine("  hook: { bin: a/b }\n"),
                "engine hook: bin \"a/b\" must be the name of a file under bin/",
            ),
            (
                engine("  methods: [interrupt]\n  interrupt: { key: Escape }\n"),
                "engine: interrupt is either a method or a fixed answer, not both",
            ),
            (
                "on-event:\n  events: all\n  bin: \"\"\n".to_owned(),
                "on-event: bin \"\" must be the name of a file under bin/",
            ),
            (
                "on-event:\n  events: all\n  bin: sink\n  timeout: 0\n".to_owned(),
                "on-event: timeout must be 1 to 30 seconds, got 0",
            ),
            (
                "on-event:\n  events: all\n  bin: sink\n  timeout: 31\n".to_owned(),
                "on-event: timeout must be 1 to 30 seconds, got 31",
            ),
            (
                "update:\n  bin: ./update\n".to_owned(),
                "update: bin \"./update\" must be the name of a file under bin/",
            ),
        ] {
            // The addition comes last: a key written twice is the YAML parser's to refuse, so
            // the description under test replaces the minimal one.
            let text = if addition.starts_with("description") {
                format!("api: 1\nid: hello\n{addition}")
            } else {
                format!("{MINIMAL}{addition}")
            };
            let error = read("hello", &text).unwrap_err();
            assert!(error.message.contains(message), "{addition:?}: {error}");
            assert_eq!(error.line, None, "{addition:?}");
        }
        // The limits themselves are fine.
        read(
            "hello",
            &format!(
                "{MINIMAL}engine:\n  kind: {}\n  bin: engine\n  methods: [interrupt]\n\
                 on-event:\n  events: []\n  bin: sink\n  timeout: 30\n",
                "k".repeat(31)
            ),
        )
        .unwrap();
        read(
            "hello",
            &format!("{MINIMAL}on-event:\n  events: all\n  bin: sink\n  timeout: 1\n"),
        )
        .unwrap();
    }

    #[test]
    fn resource_names_are_a_kind_and_a_path() {
        for name in [
            "keys/kill.tmux",
            "conf/tmux.conf",
            "bin/tmux-nav",
            "nvim/lua/tx/init.lua",
            "agents/COMMON.md",
            "definitions/skills/tx-kill/SKILL.md",
        ] {
            read("hello", &format!("{MINIMAL}resources: [{name}]\n"))
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        for (name, needle) in [
            ("/keys/kill.tmux", "invalid resource name"),
            ("keys/../kill.tmux", "invalid resource name"),
            ("\"keys/kill tmux\"", "invalid resource name"),
            ("\"\"", "invalid resource name"),
            (
                "kill.tmux",
                "must be a file under one of keys, conf, bin, nvim, agents, definitions",
            ),
            (
                "lib/kill.tmux",
                "must be a file under one of keys, conf, bin, nvim, agents, definitions",
            ),
            ("keys/", "must be a file under one of"),
            ("keys/kill.conf", "a key fragment is keys/<name>.tmux"),
            ("keys/sub/kill.tmux", "a key fragment is keys/<name>.tmux"),
            ("keys/.tmux", "a key fragment is keys/<name>.tmux"),
        ] {
            let message = refused(&format!("{MINIMAL}resources: [{name}]\n"));
            assert!(message.contains(needle), "{name}: {message}");
        }
    }

    /// A built-in's name is not the manifest's to refuse: the plugin stays usable and only that
    /// verb is dropped, when the manifest meets the front end and the other plugins.
    #[test]
    fn a_verb_named_like_a_built_in_still_reads() {
        let manifest = read(
            "hello",
            &format!("{MINIMAL}verbs:\n  - name: create\n    usage: create\n    bin: hello\n"),
        )
        .unwrap();
        assert_eq!(manifest.verbs[0].name, "create");
    }
}
