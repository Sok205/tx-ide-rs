# tx-core plugins Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `tx-core` the plugin host (engines, event sinks, verbs behind one `Plugin` trait, switched by `plugins.yaml`), move the two engine crates under `poc/plugins/`, and port tx-ide's engine hooks as the first feature plugin.

**Architecture:** `tx-core` gains `plugin.rs` (`Plugin`, `Registry`, `Verb`, `PluginConfig`, `TxCore::plug`) and `Session.ext` + `TxCore::update_ext` for plugin-owned record data. `txc` parses `<state_dir>/plugins.yaml` into a `PluginConfig`, plugs a `MANIFEST`, and falls back to `core.verb(name)` for unknown verbs. The `hooks` plugin is a verb (`hook <event>|install|uninstall|status`) plus a sink.

**Tech Stack:** Rust 2024 / 1.92, serde + serde_json (existing), `serde_norway` (maintained serde_yaml fork) in the CLI only, tmux-backed integration tests on private sockets.

**Spec:** `docs/superpowers/specs/2026-10-16-poc-plugins-design.md`

Everything below is relative to `poc/` unless the path starts with `docs/`. Run cargo from `poc/`.

## Global Constraints

- Edition 2024, `rust-version = "1.92"`, workspace lints `clippy::all = warn`; finish every task with `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `tx-core` keeps its dependency set (serde, serde_json, thiserror, uuid, tempfile): **no YAML crate in core**, core never parses argv or reads env.
- Plugin ids: `[a-z0-9-]+`; verb names must not shadow a built-in `txc` verb.
- `RECORD_VERSION` stays `1`; a record without `ext` must still load.
- Tests use a temp dir under `/tmp` (short socket path), a private tmux socket, and `require_tmux!`-style skipping; never the user's tmux server or `$HOME`.
- Exit codes: 0 ok, 1 core/plugin error (`txc: <error>` on stderr), 2 usage (message + usage on stderr).
- `hook <event>` exits 0 on every handled path ("not ours" included); only a state-dir I/O failure exits 1.

## Review Focus

1. `plugins.yaml` present but `plugins:` key missing or the file empty → treated as "everything enabled", not a parse error (Task 4 test `empty_yaml_enables_everything`).
2. `txc hook stop` run with `TX_SESSION_ID` set to a well-formed id that no longer has a record (forgotten) → exit 0, nothing written (Task 5 test `unknown_session_is_not_ours`).
3. `hook install` when `~/.claude/settings.json` does not exist yet → creates it with only the hooks block; a settings file that is not a JSON object → error, file untouched (Task 6 tests `install_creates_missing_settings`, `install_refuses_non_object`).
4. Two plugins registering the same engine id → `plug` fails naming the plugin, not a panic (Task 2 test `duplicate_engine_id_fails_with_plugin_name`).
5. `update_ext` whose closure leaves the value `Null` → the key is removed, the record does not grow a `"hooks": null` (Task 1 test `null_removes_the_key`).

---

### Task 1: `Session.ext` and `TxCore::update_ext`

**Files:**
- Modify: `tx-core/src/session.rs:190-201` (struct `Session`), `tx-core/tests/fuzz_session.rs:136-150` (reserved keys)
- Modify: `tx-core/src/core.rs` (after `set_session_tags`, ~line 660)
- Test: `tx-core/tests/ext.rs`

**Interfaces:**
- Produces: `Session.ext: BTreeMap<String, serde_json::Value>`; `TxCore::update_ext(&self, id: &SessionId, plugin: &str, f: impl FnOnce(&mut serde_json::Value)) -> Result<bool, CoreError>`.

- [ ] **Step 1: Write the failing tests**

```rust
// tx-core/tests/ext.rs
//! Plugin-owned record data: `Session.ext`, written under the session lock.

mod common;

use std::sync::Arc;
use std::thread;

use common::{Harness, process};
use serde_json::{Value, json};
use tx_core::{Session, SessionId};

#[test]
fn record_without_ext_loads_as_empty() {
    let text = r#"{"version":1,"id":"0f6b3b8a-1111-4222-8333-444444444444","name":"a","parent":null,
        "tags":[],"cwd":"/tmp","created_at":1.0,"ended_at":null,"lifecycle":"running",
        "launch":{"kind":"process","program":"sh","args":[]}}"#;
    let session = Session::from_json(text).unwrap();
    assert!(session.ext.is_empty());
    assert!(!session.to_json().unwrap().contains("\"ext\""), "empty ext is not written");
}

#[test]
fn ext_round_trips() {
    let text = r#"{"version":1,"id":"0f6b3b8a-1111-4222-8333-444444444444","name":"a","parent":null,
        "tags":[],"cwd":"/tmp","created_at":1.0,"ended_at":null,"lifecycle":"running",
        "launch":{"kind":"process","program":"sh","args":[]},"ext":{"hooks":{"state":"idle"}}}"#;
    let session = Session::from_json(text).unwrap();
    assert_eq!(session.ext["hooks"], json!({"state": "idle"}));
    let again = Session::from_json(&session.to_json().unwrap()).unwrap();
    assert_eq!(again, session);
}

#[test]
fn update_ext_writes_only_on_change() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id = core.create_session(process("a", &h.workdir("a"))).unwrap();
    assert!(core.update_ext(&id, "hooks", |v| *v = json!({"state": "working"})).unwrap());
    assert_eq!(core.get_session(&id).unwrap().ext["hooks"]["state"], "working");
    assert!(!core.update_ext(&id, "hooks", |v| { let _ = v; }).unwrap(), "untouched → false");
    assert!(core.update_ext(&id, "other", |v| *v = json!(1)).unwrap());
    let ext = core.get_session(&id).unwrap().ext;
    assert_eq!(ext.len(), 2, "other plugins' keys survive");
}

#[test]
fn null_removes_the_key() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id = core.create_session(process("a", &h.workdir("a"))).unwrap();
    core.update_ext(&id, "hooks", |v| *v = json!({"state": "idle"})).unwrap();
    assert!(core.update_ext(&id, "hooks", |v| *v = Value::Null).unwrap());
    assert!(core.get_session(&id).unwrap().ext.is_empty());
    assert!(!core.update_ext(&id, "hooks", |v| *v = Value::Null).unwrap(), "absent → Null is no change");
}

#[test]
fn update_ext_on_unknown_session_is_not_found() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id: SessionId = "0f6b3b8a-1111-4222-8333-444444444444".parse().unwrap();
    assert!(matches!(core.update_ext(&id, "hooks", |_| {}), Err(tx_core::CoreError::NotFound(_))));
}

#[test]
fn concurrent_updates_both_survive() {
    require_tmux!();
    let h = Harness::new();
    let core = Arc::new(h.open());
    let id = core.create_session(process("a", &h.workdir("a"))).unwrap();
    let handles: Vec<_> = ["x", "y"]
        .into_iter()
        .map(|key| {
            let core = Arc::clone(&core);
            let id = id.clone();
            thread::spawn(move || {
                for i in 0..50 {
                    core.update_ext(&id, key, |v| *v = json!(i)).unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let ext = core.get_session(&id).unwrap().ext;
    assert_eq!(ext["x"], json!(49));
    assert_eq!(ext["y"], json!(49));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p tx-core --test ext`
Expected: compile error — `no field ext on Session`, `no method update_ext`.

- [ ] **Step 3: Add the field**

In `tx-core/src/session.rs`, add `use std::collections::BTreeMap;` and extend `Session`:

```rust
    pub launch: Launch,
    /// Plugin-owned data keyed by plugin id; written through `TxCore::update_ext`. Core never
    /// reads it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ext: BTreeMap<String, serde_json::Value>,
```

Fix every `Session { … }` literal that now misses the field: `grep -rn 'Session {' tx-core/src tx-core/tests` — add `ext: BTreeMap::new(),` in `core.rs::create_locked` and in `tests/fuzz_session.rs::sample` / `arb_session_with`. In `fuzz_session.rs:144` add `"ext"` to the reserved-keys list.

- [ ] **Step 4: Add `update_ext`**

In `tx-core/src/core.rs`, after `set_session_tags`:

```rust
    /// Runs `f` on `ext[plugin]` (`Null` when absent) under the session lock and saves the
    /// record if the value changed; leaving it `Null` removes the key. Returns whether a write
    /// happened. `f` must not call back into the core (the lock is held).
    pub fn update_ext(
        &self,
        id: &SessionId,
        plugin: &str,
        f: impl FnOnce(&mut serde_json::Value),
    ) -> Result<bool, CoreError> {
        let _session = self.locks.session(id).map_err(CoreError::Lock)?;
        let mut session = self.get_session(id)?;
        let before = session.ext.get(plugin).cloned().unwrap_or(serde_json::Value::Null);
        let mut value = before.clone();
        f(&mut value);
        if value == before {
            return Ok(false);
        }
        if value.is_null() {
            session.ext.remove(plugin);
        } else {
            session.ext.insert(plugin.to_owned(), value);
        }
        self.store.save(&session)?;
        Ok(true)
    }
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p tx-core`
Expected: all pass, including `fuzz_session` (the `ext` key is reserved, so random extra keys never collide).

- [ ] **Step 6: Commit**

```bash
git add tx-core && git commit -m "feat(tx-core): Session.ext and update_ext for plugin-owned record data"
```

---

### Task 2: `tx-core/src/plugin.rs` — `Plugin`, `Registry`, `Verb`, `TxCore::plug`

**Files:**
- Create: `tx-core/src/plugin.rs`
- Modify: `tx-core/src/lib.rs:4-48` (module + re-exports), `tx-core/src/core.rs:251-260` (fields) and `open` (~line 340)
- Test: `tx-core/tests/plugins.rs`

**Interfaces:**
- Consumes: `EngineRegistry::register`, `TxCore::subscribe`, `CoreConfig` (Task 0 code).
- Produces (all re-exported from `tx_core`):

```rust
pub trait Plugin: Send + Sync {
    fn id(&self) -> &'static str;
    fn register(&self, r: &mut Registry) -> Result<(), PluginError>;
}
pub struct Registry { .. }
impl Registry {
    pub fn engine(&mut self, adapter: Box<dyn EngineAdapter>);
    pub fn sink(&mut self, sink: Box<dyn EventSink>);
    pub fn verb(&mut self, verb: Arc<dyn Verb>);
    pub fn settings(&self) -> &serde_json::Value;   // this plugin's block, Null when none
    pub fn config(&self) -> &CoreConfig;
}
pub trait Verb: Send + Sync {
    fn name(&self) -> &'static str;
    fn usage(&self) -> &'static str;
    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError>;
}
pub struct VerbIo<'a> { pub stdin: &'a mut dyn Read, pub stdout: &'a mut dyn Write, pub stderr: &'a mut dyn Write }
pub enum VerbError { Core(CoreError), Failed(String) }
#[derive(Default)] pub struct PluginConfig { pub disabled: BTreeSet<String>, pub settings: BTreeMap<String, serde_json::Value>, pub reserved: BTreeSet<String> }
pub enum PluginStatus { Active, Disabled }
pub enum PluginError { InvalidId(String), DuplicatePlugin(String), Unknown(String), DuplicateVerb { verb, first, second }, ReservedVerb { verb, plugin }, Engine { plugin, source: CoreError }, Register { plugin, reason } }
impl TxCore {
    pub fn plug(&mut self, plugins: Vec<Box<dyn Plugin>>, cfg: &PluginConfig) -> Result<(), PluginError>;
    pub fn verb(&self, name: &str) -> Option<Arc<dyn Verb>>;
    pub fn verbs(&self) -> Vec<Arc<dyn Verb>>;                    // name order
    pub fn plugin_status(&self) -> &[(String, PluginStatus)];     // manifest order
}
```

`Verb::run` takes `&TxCore` (every core operation except `plug`/`subscribe`/`register_engine` is `&self`), and verbs are `Arc` so the CLI can clone one out of the core and run it against the same core.

- [ ] **Step 1: Write the failing tests**

```rust
// tx-core/tests/plugins.rs
//! The plugin host: one trait, three slots, a config that switches plugins off.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use common::{Harness, ShellStub, process};
use serde_json::json;
use tx_core::{
    Event, EventSink, Plugin, PluginConfig, PluginError, PluginStatus, Registry, TxCore, Verb,
    VerbError, VerbIo,
};

struct Echo;
impl Verb for Echo {
    fn name(&self) -> &'static str { "echo" }
    fn usage(&self) -> &'static str { "echo WORD...   prints its arguments" }
    fn run(&self, _core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        writeln!(io.stdout, "{}", argv.join(" ")).unwrap();
        Ok(0)
    }
}
use std::io::Write;

#[derive(Default)]
struct Seen(Mutex<Vec<String>>);
impl EventSink for Seen {
    fn on_event(&self, event: &Event) { self.0.lock().unwrap().push(event.name.clone()); }
}

/// Fills every slot and remembers its settings.
struct Full { seen: Arc<Seen>, settings: Arc<Mutex<serde_json::Value>> }
impl Plugin for Full {
    fn id(&self) -> &'static str { "full" }
    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        *self.settings.lock().unwrap() = r.settings().clone();
        r.engine(Box::new(ShellStub));
        r.sink(Box::new(Arc::clone(&self.seen)));
        r.verb(Arc::new(Echo));
        Ok(())
    }
}

struct Named(&'static str, &'static str);  // (plugin id, verb name)
struct NamedVerb(&'static str);
impl Verb for NamedVerb {
    fn name(&self) -> &'static str { self.0 }
    fn usage(&self) -> &'static str { "" }
    fn run(&self, _: &TxCore, _: &mut VerbIo<'_>, _: &[String]) -> Result<i32, VerbError> { Ok(0) }
}
impl Plugin for Named {
    fn id(&self) -> &'static str { self.0 }
    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(NamedVerb(self.1)));
        Ok(())
    }
}

struct EngineOnly(&'static str);
impl Plugin for EngineOnly {
    fn id(&self) -> &'static str { self.0 }
    fn register(&self, r: &mut Registry) -> Result<(), PluginError> { r.engine(Box::new(ShellStub)); Ok(()) }
}

struct Broken;
impl Plugin for Broken {
    fn id(&self) -> &'static str { "broken" }
    fn register(&self, _: &mut Registry) -> Result<(), PluginError> {
        Err(PluginError::Register { plugin: "broken".into(), reason: "no".into() })
    }
}

fn full() -> (Box<dyn Plugin>, Arc<Seen>, Arc<Mutex<serde_json::Value>>) {
    let seen = Arc::new(Seen::default());
    let settings = Arc::new(Mutex::new(serde_json::Value::Null));
    (Box::new(Full { seen: Arc::clone(&seen), settings: Arc::clone(&settings) }), seen, settings)
}

#[test]
fn a_plugin_fills_all_three_slots() {
    require_tmux!();
    let h = Harness::new();
    let mut core = h.open();
    let (plugin, seen, settings) = full();
    let cfg = PluginConfig { settings: BTreeMap::from([("full".into(), json!({"k": 1}))]), ..Default::default() };
    core.plug(vec![plugin], &cfg).unwrap();
    assert_eq!(core.engine_ids(), vec![common::STUB_ID]);
    assert_eq!(*settings.lock().unwrap(), json!({"k": 1}));
    core.create_session(process("alice", &h.workdir("a"))).unwrap();
    assert_eq!(*seen.0.lock().unwrap(), vec!["alice".to_owned()]);
    let verb = core.verb("echo").expect("verb registered");
    let mut out = Vec::new();
    let (mut stdin, mut err) = (std::io::empty(), Vec::new());
    let mut io = VerbIo { stdin: &mut stdin, stdout: &mut out, stderr: &mut err };
    assert_eq!(verb.run(&core, &mut io, &["a".into(), "b".into()]).unwrap(), 0);
    assert_eq!(String::from_utf8(out).unwrap(), "a b\n");
    assert_eq!(core.plugin_status(), &[("full".to_owned(), PluginStatus::Active)]);
}

#[test]
fn disabled_plugin_registers_nothing() {
    let h = Harness::new();
    let mut core = h.open();
    let (plugin, _, settings) = full();
    let cfg = PluginConfig { disabled: BTreeSet::from(["full".to_owned()]), ..Default::default() };
    core.plug(vec![plugin], &cfg).unwrap();
    assert!(core.engine_ids().is_empty());
    assert!(core.verb("echo").is_none());
    assert!(settings.lock().unwrap().is_null(), "register never ran");
    assert_eq!(core.plugin_status(), &[("full".to_owned(), PluginStatus::Disabled)]);
}

#[test]
fn settings_default_to_null() {
    let h = Harness::new();
    let mut core = h.open();
    let (plugin, _, settings) = full();
    core.plug(vec![plugin], &PluginConfig::default()).unwrap();
    assert!(settings.lock().unwrap().is_null());
}

#[test]
fn unknown_ids_in_config_are_errors() {
    let h = Harness::new();
    for cfg in [
        PluginConfig { disabled: BTreeSet::from(["nope".to_owned()]), ..Default::default() },
        PluginConfig { settings: BTreeMap::from([("nope".to_owned(), json!(1))]), ..Default::default() },
    ] {
        let mut core = h.open();
        let error = core.plug(vec![Box::new(Named("a", "x"))], &cfg).unwrap_err();
        assert!(matches!(error, PluginError::Unknown(ref id) if id == "nope"), "{error}");
    }
}

#[test]
fn duplicate_verb_names_the_two_plugins() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core
        .plug(vec![Box::new(Named("a", "x")), Box::new(Named("b", "x"))], &PluginConfig::default())
        .unwrap_err();
    match error {
        PluginError::DuplicateVerb { verb, first, second } => {
            assert_eq!((verb.as_str(), first.as_str(), second.as_str()), ("x", "a", "b"));
        }
        other => panic!("{other}"),
    }
}

#[test]
fn reserved_verb_is_refused() {
    let h = Harness::new();
    let mut core = h.open();
    let cfg = PluginConfig { reserved: BTreeSet::from(["ls".to_owned()]), ..Default::default() };
    let error = core.plug(vec![Box::new(Named("a", "ls"))], &cfg).unwrap_err();
    assert!(matches!(error, PluginError::ReservedVerb { ref verb, ref plugin } if verb == "ls" && plugin == "a"));
}

#[test]
fn duplicate_engine_id_fails_with_plugin_name() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core
        .plug(vec![Box::new(EngineOnly("e1")), Box::new(EngineOnly("e2"))], &PluginConfig::default())
        .unwrap_err();
    assert!(matches!(error, PluginError::Engine { ref plugin, .. } if plugin == "e2"), "{error}");
}

#[test]
fn bad_and_duplicate_plugin_ids() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core.plug(vec![Box::new(Named("Bad_Id", "x"))], &PluginConfig::default()).unwrap_err();
    assert!(matches!(error, PluginError::InvalidId(_)), "{error}");
    let mut core = h.open();
    let error = core
        .plug(vec![Box::new(Named("a", "x")), Box::new(Named("a", "y"))], &PluginConfig::default())
        .unwrap_err();
    assert!(matches!(error, PluginError::DuplicatePlugin(ref id) if id == "a"), "{error}");
}

#[test]
fn a_plugins_own_error_surfaces() {
    let h = Harness::new();
    let mut core = h.open();
    let error = core.plug(vec![Box::new(Broken)], &PluginConfig::default()).unwrap_err();
    assert_eq!(error.to_string(), "plugin \"broken\": no");
}

#[test]
fn verbs_are_listed_in_name_order() {
    let h = Harness::new();
    let mut core = h.open();
    core.plug(vec![Box::new(Named("a", "zeta")), Box::new(Named("b", "alpha"))], &PluginConfig::default()).unwrap();
    let names: Vec<_> = core.verbs().iter().map(|v| v.name()).collect();
    assert_eq!(names, ["alpha", "zeta"]);
}
```

`common::STUB_ID` and `ShellStub` already exist in `tx-core/tests/common/mod.rs` (`pub const STUB_ID`, `pub struct ShellStub`); if `STUB_ID` is private, make it `pub`.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p tx-core --test plugins`
Expected: compile error — unresolved imports `tx_core::Plugin` etc.

- [ ] **Step 3: Write `plugin.rs`**

```rust
// tx-core/src/plugin.rs
//! The plugin host. One trait, [`Plugin`]; a [`Registry`] with three slots (engines, event
//! sinks, verbs); a [`PluginConfig`] the front end fills from its own config format. Core never
//! parses arguments or reads the environment: a [`Verb`] receives argv already split.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::sync::Arc;

use serde_json::Value;

use crate::config::CoreConfig;
use crate::core::TxCore;
use crate::engine::EngineAdapter;
use crate::error::CoreError;
use crate::events::EventSink;

pub trait Plugin: Send + Sync {
    /// `[a-z0-9-]+`, unique across the manifest; the key in the front end's config and in
    /// `Session.ext`.
    fn id(&self) -> &'static str;
    fn register(&self, r: &mut Registry) -> Result<(), PluginError>;
}

/// What one plugin contributes. Built per plugin by [`TxCore::plug`].
pub struct Registry {
    engines: Vec<Box<dyn EngineAdapter>>,
    sinks: Vec<Box<dyn EventSink>>,
    verbs: Vec<Arc<dyn Verb>>,
    settings: Value,
    config: CoreConfig,
}

impl Registry {
    pub fn engine(&mut self, adapter: Box<dyn EngineAdapter>) {
        self.engines.push(adapter);
    }

    pub fn sink(&mut self, sink: Box<dyn EventSink>) {
        self.sinks.push(sink);
    }

    pub fn verb(&mut self, verb: Arc<dyn Verb>) {
        self.verbs.push(verb);
    }

    /// This plugin's settings block; `Null` when the config has none.
    pub fn settings(&self) -> &Value {
        &self.settings
    }

    /// The core's config (state dir, socket), for plugins that open a second core in a sink.
    pub fn config(&self) -> &CoreConfig {
        &self.config
    }
}

/// A command a front end can dispatch to. `argv` is everything after the verb name.
pub trait Verb: Send + Sync {
    fn name(&self) -> &'static str;
    /// One usage line, shown under the front end's own verbs.
    fn usage(&self) -> &'static str;
    /// Returns the exit code. Usage mistakes: write to `io.stderr`, return `Ok(2)`.
    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError>;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginStatus {
    Active,
    Disabled,
}

impl PluginStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PluginStatus::Active => "active",
            PluginStatus::Disabled => "disabled",
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
    DuplicateVerb { verb: String, first: String, second: String },
    #[error("plugin {plugin:?} registers verb {verb:?}, which the front end owns")]
    ReservedVerb { verb: String, plugin: String },
    #[error("plugin {plugin:?}: {source}")]
    Engine { plugin: String, source: CoreError },
    #[error("plugin {plugin:?}: {reason}")]
    Register { plugin: String, reason: String },
}

pub(crate) fn validate_id(id: &str) -> Result<(), PluginError> {
    let ok = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else { Err(PluginError::InvalidId(id.to_owned())) }
}

/// The host side, kept in `TxCore`.
#[derive(Default)]
pub(crate) struct Host {
    pub(crate) verbs: BTreeMap<String, (String, Arc<dyn Verb>)>, // name → (plugin id, verb)
    pub(crate) status: Vec<(String, PluginStatus)>,
}

impl TxCore {
    /// Registers every plugin not in `cfg.disabled`, in order. Fails on the first problem; the
    /// core may then hold a partial registration and the caller should exit.
    pub fn plug(&mut self, plugins: Vec<Box<dyn Plugin>>, cfg: &PluginConfig) -> Result<(), PluginError> {
        let ids: BTreeSet<&str> = plugins.iter().map(|p| p.id()).collect();
        for id in cfg.disabled.iter().chain(cfg.settings.keys()) {
            if !ids.contains(id.as_str()) {
                return Err(PluginError::Unknown(id.clone()));
            }
        }
        let mut seen = BTreeSet::new();
        for plugin in &plugins {
            let id = plugin.id();
            validate_id(id)?;
            if !seen.insert(id) {
                return Err(PluginError::DuplicatePlugin(id.to_owned()));
            }
            if cfg.disabled.contains(id) {
                self.host.status.push((id.to_owned(), PluginStatus::Disabled));
                continue;
            }
            let mut registry = Registry {
                engines: Vec::new(),
                sinks: Vec::new(),
                verbs: Vec::new(),
                settings: cfg.settings.get(id).cloned().unwrap_or(Value::Null),
                config: self.config().clone(),
            };
            plugin.register(&mut registry)?;
            for adapter in registry.engines {
                self.register_engine(adapter)
                    .map_err(|source| PluginError::Engine { plugin: id.to_owned(), source })?;
            }
            for sink in registry.sinks {
                self.subscribe(sink);
            }
            for verb in registry.verbs {
                let name = verb.name().to_owned();
                if cfg.reserved.contains(&name) {
                    return Err(PluginError::ReservedVerb { verb: name, plugin: id.to_owned() });
                }
                if let Some((first, _)) = self.host.verbs.get(&name) {
                    return Err(PluginError::DuplicateVerb { verb: name, first: first.clone(), second: id.to_owned() });
                }
                self.host.verbs.insert(name, (id.to_owned(), verb));
            }
            self.host.status.push((id.to_owned(), PluginStatus::Active));
        }
        Ok(())
    }

    pub fn verb(&self, name: &str) -> Option<Arc<dyn Verb>> {
        self.host.verbs.get(name).map(|(_, verb)| Arc::clone(verb))
    }

    /// Every plugin verb, by name.
    pub fn verbs(&self) -> Vec<Arc<dyn Verb>> {
        self.host.verbs.values().map(|(_, verb)| Arc::clone(verb)).collect()
    }

    /// Manifest order.
    pub fn plugin_status(&self) -> &[(String, PluginStatus)] {
        &self.host.status
    }
}
```

In `tx-core/src/core.rs`: add `host: crate::plugin::Host,` to `TxCore` (after `sinks`) and `host: Default::default(),` in `open`. In `lib.rs`: `pub mod plugin;` and
`pub use plugin::{Plugin, PluginConfig, PluginError, PluginStatus, Registry, Verb, VerbError, VerbIo};`.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p tx-core --test plugins && cargo clippy -p tx-core --all-targets -- -D warnings`
Expected: 10 passed, clippy clean.

- [ ] **Step 5: Commit**

```bash
git add tx-core && git commit -m "feat(tx-core): plugin host — Plugin, Registry, Verb, TxCore::plug"
```

---

### Task 3: Engines become plugins under `plugins/`

**Files:**
- Move: `tx-engine-claude/` → `plugins/engine-claude/`, `tx-engine-codex/` → `plugins/engine-codex/` (`git mv`; crate names unchanged)
- Modify: `Cargo.toml` (workspace members), `tx-core-cli/Cargo.toml` (paths), `plugins/engine-claude/src/lib.rs`, `plugins/engine-codex/src/lib.rs`, `README.md:1-30` (paths)

**Interfaces:**
- Produces: `tx_engine_claude::ClaudePlugin` and `tx_engine_codex::CodexPlugin` (unit structs implementing `tx_core::Plugin` with ids `engine-claude` / `engine-codex`).

- [ ] **Step 1: Move the crates**

```bash
mkdir -p plugins
git mv tx-engine-claude plugins/engine-claude
git mv tx-engine-codex plugins/engine-codex
```

`Cargo.toml` members: `["tx-core", "plugins/engine-claude", "plugins/engine-codex", "tx-core-cli"]`. `tx-core-cli/Cargo.toml`: `tx-engine-claude = { path = "../plugins/engine-claude" }`, same for codex.

- [ ] **Step 2: Write the failing tests** (append to the existing `#[cfg(test)] mod tests` in each engine's `lib.rs`)

```rust
    #[test]
    fn plugin_registers_the_engine() {
        use tx_core::{Plugin, PluginConfig, TxCore, CoreConfig};
        let dir = tempfile::Builder::new().prefix("txc").tempdir_in("/tmp").unwrap();
        let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
        assert_eq!(ClaudePlugin.id(), "engine-claude");
        core.plug(vec![Box::new(ClaudePlugin)], &PluginConfig::default()).unwrap();
        assert_eq!(core.engine_ids(), vec!["claude"]);
    }
```

(`CodexPlugin` / `"engine-codex"` / `"codex"` in the codex crate.)

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p tx-engine-claude -p tx-engine-codex`
Expected: `cannot find value ClaudePlugin`.

- [ ] **Step 4: Implement**

In `plugins/engine-claude/src/lib.rs` after `impl EngineAdapter for Claude`:

```rust
/// The plugin wrapper: registers [`Claude`] under the plugin id `engine-claude`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudePlugin;

impl tx_core::Plugin for ClaudePlugin {
    fn id(&self) -> &'static str {
        "engine-claude"
    }

    fn register(&self, r: &mut tx_core::Registry) -> Result<(), tx_core::PluginError> {
        r.engine(Box::new(Claude));
        Ok(())
    }
}
```

Mirror for `CodexPlugin` / `Codex` / `"engine-codex"`.

- [ ] **Step 5: Run the whole workspace**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: green (the CLI still registers engines by hand; that changes in Task 4).

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "refactor(poc): engines move to plugins/ and implement Plugin"
```

---

### Task 4: `txc` — `plugins.yaml`, `MANIFEST`, dispatch fallback, `plugins` verb

**Files:**
- Create: `tx-core-cli/src/plugins.rs`
- Modify: `tx-core-cli/Cargo.toml`, `tx-core-cli/src/args.rs` (`USAGE`, `Global`, `Verb`, `parse`), `tx-core-cli/src/main.rs` (`open`, `run`)
- Test: `tx-core-cli/src/plugins.rs` (unit), `tx-core-cli/tests/plugins.rs` (end-to-end)

**Interfaces:**
- Consumes: `tx_core::{Plugin, PluginConfig, PluginError, TxCore::{plug, verb, verbs, plugin_status}, VerbIo}`, `ClaudePlugin`, `CodexPlugin`.
- Produces: `plugins::MANIFEST: &[fn() -> Box<dyn Plugin>]`, `plugins::load(path: &Path, reserved: &[&str]) -> Result<PluginConfig, UsageError>`, `plugins::RESERVED: &[&str]`, global `--plugins FILE`, built-in verb `plugins`.

- [ ] **Step 1: Dependency**

`tx-core-cli/Cargo.toml` `[dependencies]`: add `serde = { workspace = true }`, `serde_json.workspace = true`, `serde_norway = "0.9"`. Run `cargo fetch` once.

- [ ] **Step 2: Write the failing unit tests** (bottom of the new `tx-core-cli/src/plugins.rs`; write the file with only `use` lines + tests first)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    fn parse_text(text: &str) -> Result<PluginConfig, UsageError> {
        parse(text, &["ls"])
    }

    #[test]
    fn empty_yaml_enables_everything() {
        for text in ["", "# nothing\n", "plugins:\n", "plugins: {}\n"] {
            let cfg = parse_text(text).unwrap();
            assert!(cfg.disabled.is_empty(), "{text:?}");
            assert!(cfg.settings.is_empty(), "{text:?}");
            assert_eq!(cfg.reserved, BTreeSet::from(["ls".to_owned()]));
        }
    }

    #[test]
    fn enabled_false_disables_and_settings_pass_through() {
        let cfg = parse_text(
            "plugins:\n  engine-codex: { enabled: false }\n  hooks:\n    enabled: true\n    settings: { stuck_after_seconds: 5 }\n",
        )
        .unwrap();
        assert_eq!(cfg.disabled, BTreeSet::from(["engine-codex".to_owned()]));
        assert_eq!(cfg.settings, BTreeMap::from([("hooks".to_owned(), serde_json::json!({"stuck_after_seconds": 5}))]));
    }

    #[test]
    fn enabled_defaults_to_true() {
        let cfg = parse_text("plugins:\n  hooks: {}\n").unwrap();
        assert!(cfg.disabled.is_empty());
    }

    #[test]
    fn malformed_yaml_is_a_usage_error() {
        let error = parse_text("plugins: [1, 2\n").unwrap_err();
        assert!(error.0.starts_with("plugins.yaml: "), "{error}");
    }

    #[test]
    fn unknown_keys_on_an_entry_are_refused() {
        let error = parse_text("plugins:\n  hooks: { enabledd: true }\n").unwrap_err();
        assert!(error.0.contains("enabledd"), "{error}");
    }
}
```

- [ ] **Step 3: Run to verify failure**

Add `mod plugins;` to `main.rs`. Run: `cargo test -p tx-core-cli plugins::`
Expected: compile error — `parse` / `PluginConfig` unresolved.

- [ ] **Step 4: Implement `plugins.rs`**

```rust
//! The plugin switchboard: the compiled-in manifest and `plugins.yaml`.
//!
//! ```yaml
//! plugins:
//!   engine-codex: { enabled: false }
//!   hooks:
//!     settings: { stuck_after_seconds: 600 }
//! ```
//! A missing file enables everything; a malformed file or an unknown id is a usage error.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use tx_core::{Plugin, PluginConfig};

use crate::args::UsageError;

pub const FILE_NAME: &str = "plugins.yaml";

/// Every plugin this binary links, in listing order.
pub const MANIFEST: &[fn() -> Box<dyn Plugin>] = &[
    || Box::new(tx_engine_claude::ClaudePlugin),
    || Box::new(tx_engine_codex::CodexPlugin),
];

/// The built-in verbs; a plugin may not register one of these.
pub const RESERVED: &[&str] = &[
    "help", "engines", "plugins", "create", "ls", "show", "rename", "tag", "send", "refresh",
    "where", "attach", "stop", "forget",
];

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    plugins: Option<BTreeMap<String, Entry>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    settings: Option<serde_json::Value>,
}

fn yes() -> bool {
    true
}

pub fn load(path: &Path, reserved: &[&str]) -> Result<PluginConfig, UsageError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(UsageError(format!("{}: {error}", path.display()))),
    };
    parse(&text, reserved)
}

pub fn parse(text: &str, reserved: &[&str]) -> Result<PluginConfig, UsageError> {
    let file: File = if text.trim().is_empty() {
        File::default()
    } else {
        serde_norway::from_str(text).map_err(|error| UsageError(format!("{FILE_NAME}: {error}")))?
    };
    let mut cfg = PluginConfig {
        reserved: reserved.iter().map(|s| (*s).to_owned()).collect(),
        ..Default::default()
    };
    for (id, entry) in file.plugins.unwrap_or_default() {
        if !entry.enabled {
            cfg.disabled.insert(id.clone());
        }
        if let Some(settings) = entry.settings {
            cfg.settings.insert(id, settings);
        }
    }
    Ok(cfg)
}

pub fn manifest() -> Vec<Box<dyn Plugin>> {
    MANIFEST.iter().map(|make| make()).collect()
}
```

- [ ] **Step 5: Run the unit tests**

Run: `cargo test -p tx-core-cli plugins::`
Expected: 5 passed.

- [ ] **Step 6: Write the failing end-to-end tests**

```rust
// tx-core-cli/tests/plugins.rs
//! plugins.yaml drives what txc links; plugin verbs dispatch after the built-ins.

use std::path::PathBuf;
use std::process::{Command, Output};

struct Cli {
    dir: tempfile::TempDir,
}

impl Cli {
    fn new() -> Self {
        Self { dir: tempfile::Builder::new().prefix("txc").tempdir_in("/tmp").unwrap() }
    }
    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    fn write_yaml(&self, text: &str) {
        std::fs::create_dir_all(self.state()).unwrap();
        std::fs::write(self.state().join("plugins.yaml"), text).unwrap();
    }
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_txc"))
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID")
            .output()
            .unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(output.status.success(), "txc {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    fn fails(&self, args: &[&str], code: i32) -> String {
        let output = self.run(args);
        assert_eq!(output.status.code(), Some(code), "txc {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
}

#[test]
fn without_a_yaml_every_plugin_is_active() {
    let cli = Cli::new();
    assert_eq!(cli.ok(&["plugins"]), "engine-claude\tactive\nengine-codex\tactive\n");
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\n");
}

#[test]
fn a_disabled_engine_disappears() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  engine-codex: { enabled: false }\n");
    assert_eq!(cli.ok(&["plugins"]), "engine-claude\tactive\nengine-codex\tdisabled\n");
    assert_eq!(cli.ok(&["engines"]), "claude\n");
}

#[test]
fn unknown_id_is_a_usage_error_naming_the_key() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  engine-cloud: {}\n");
    let stderr = cli.fails(&["plugins"], 2);
    assert!(stderr.contains("engine-cloud"), "{stderr}");
}

#[test]
fn malformed_yaml_is_a_usage_error() {
    let cli = Cli::new();
    cli.write_yaml("plugins: [\n");
    let stderr = cli.fails(&["engines"], 2);
    assert!(stderr.contains("plugins.yaml"), "{stderr}");
}

#[test]
fn plugins_flag_overrides_the_default_file() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  engine-codex: { enabled: false }\n");
    let other = cli.dir.path().join("other.yaml");
    std::fs::write(&other, "plugins:\n  engine-claude: { enabled: false }\n").unwrap();
    assert_eq!(cli.ok(&["--plugins", other.to_str().unwrap(), "engines"]), "codex\n");
}

#[test]
fn unknown_verb_still_exits_2() {
    let cli = Cli::new();
    let stderr = cli.fails(&["frobnicate"], 2);
    assert!(stderr.contains("unknown verb \"frobnicate\""), "{stderr}");
}
```

- [ ] **Step 7: Run to verify failure**

Run: `cargo test -p tx-core-cli --test plugins`
Expected: `without_a_yaml_every_plugin_is_active` fails (`unknown verb "plugins"`).

- [ ] **Step 8: Wire the CLI**

`args.rs`:
- `Global` gains `pub plugins: Option<PathBuf>`; in `parse`'s global loop add
  `Some("--plugins") => { cursor.next(); global.plugins = Some(cursor.value("--plugins")?.into()); }`.
- `Verb` gains `Plugins` and `Plugin { name: String, argv: Vec<String> }`; in the verb match add `Some("plugins") => Verb::Plugins,` and replace the `Some(other)` arm with
  `Some(other) => Verb::Plugin { name: other.to_owned(), argv: cursor.rest() },` (the trailing "unexpected argument" check stays for the built-ins; `Plugin` consumed the rest).
- `USAGE`: first line becomes `usage: txc [--state-dir DIR] [--socket PATH] [--plugins FILE] [--profile-dir DIR]... [--skill-dir DIR]... [--env KEY=VALUE]... <verb> ...`; add the line `  plugins                                                           id and active|disabled per plugin` after `engines`; add to the footer: `Plugins are switched in <state-dir>/plugins.yaml (see --plugins).`
- Add a unit test to the existing `mod tests`: `parse_str("--plugins p.yaml frob a b")` → `global.plugins == Some("p.yaml".into())`, verb `Verb::Plugin { name: "frob", argv: ["a","b"] }`.

`main.rs`:
- `open`: replace the two `register_engine` lines with

```rust
    let mut core = TxCore::open(config)?;
    let yaml = global.plugins.clone().map(absolute).unwrap_or_else(|| core.config().state_dir.join(plugins::FILE_NAME));
    let cfg = plugins::load(&yaml, plugins::RESERVED).map_err(Failure::Usage)?;
    core.plug(plugins::manifest(), &cfg).map_err(Failure::Plugin)?;
    Ok(core)
```

  with a new error type used by `open`/`run`:

```rust
#[derive(Debug)]
enum Failure {
    Usage(args::UsageError),
    Core(CoreError),
    Plugin(tx_core::PluginError),
    Verb(tx_core::VerbError),
}
impl From<CoreError> for Failure { fn from(e: CoreError) -> Self { Failure::Core(e) } }
```

  `main` maps: `Failure::Usage(e)` → `eprintln!("txc: {e}\n{USAGE}")`, exit 2; the rest → `txc: <e>`, exit 1. `run` returns `Result<ExitCode, Failure>` so a plugin verb's own exit code propagates.
- `run`: the `Help` arm opens the core too (`match open(&invocation)`); on `Ok(core)` it prints `USAGE` followed, when `core.verbs()` is non-empty, by `\nplugin verbs:\n` and one `  {usage}\n` per verb; on `Err` it prints `USAGE` alone and exits 0. New arms:

```rust
        Verb::Plugins => {
            for (id, status) in core.plugin_status() {
                writeln!(out, "{id}\t{}", status.as_str()).ok();
            }
        }
        Verb::Plugin { name, argv } => {
            let Some(verb) = core.verb(&name) else {
                return Err(Failure::Usage(args::UsageError(format!("unknown verb {name:?}"))));
            };
            drop(out);
            let (mut stdin, mut stdout, mut stderr) = (std::io::stdin().lock(), std::io::stdout().lock(), std::io::stderr().lock());
            let mut io = tx_core::VerbIo { stdin: &mut stdin, stdout: &mut stdout, stderr: &mut stderr };
            let code = verb.run(&core, &mut io, &argv).map_err(Failure::Verb)?;
            return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
        }
```

- [ ] **Step 9: Run everything**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: green; `tests/cli.rs` and `tests/fuzz_cli.rs` unchanged and passing (the `unknown verb` text is preserved).

- [ ] **Step 10: Commit**

```bash
git add -A && git commit -m "feat(txc): plugins.yaml, MANIFEST, plugin verb dispatch, plugins verb"
```

---

### Task 5: `hooks` plugin — `hook <event>`, `hook status`, the seeding sink

**Files:**
- Create: `plugins/hooks/Cargo.toml`, `plugins/hooks/src/lib.rs`, `plugins/hooks/src/state.rs`
- Modify: `Cargo.toml` (member `plugins/hooks`), `tx-core-cli/Cargo.toml` (dep `tx-hooks = { path = "../plugins/hooks" }`), `tx-core-cli/src/plugins.rs::MANIFEST` (append `|| Box::new(tx_hooks::HooksPlugin)`)
- Test: `plugins/hooks/src/state.rs` (unit), `tx-core-cli/tests/hooks.rs` (end-to-end)

**Interfaces:**
- Consumes: `Plugin`, `Registry::{settings, config, sink, verb}`, `Verb`, `VerbIo`, `TxCore::{update_ext, get_session, list_sessions, resolve}`, `tx_core::events::now()`.
- Produces: `tx_hooks::HooksPlugin` (id `hooks`); verb `hook`; `ext["hooks"] = {"state": "working"|"waiting"|"idle", "since": f64}`; settings `{ stuck_after_seconds: u64 }` (default 600).
- `state.rs` public: `pub enum State { Working, Waiting, Idle }` with `as_str`, `pub fn state_for(event: &str, payload: &Value) -> Option<State>`, `pub fn read_payload(stdin: &mut dyn Read) -> Value`, `pub const PLUGIN_ID: &str = "hooks"`, `pub const SESSION_ID_ENV: &str = "TX_SESSION_ID"`.

- [ ] **Step 1: Crate skeleton**

`plugins/hooks/Cargo.toml`:

```toml
[package]
name = "tx-hooks"
version = "0.1.0"
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "tx-core plugin: engine hooks drive a per-session working / waiting / idle state."

[dependencies]
tx-core.workspace = true
serde_json.workspace = true

[dev-dependencies]
tempfile.workspace = true

[lints]
workspace = true
```

`src/lib.rs` for now: `pub mod state; pub use state::*;`.

- [ ] **Step 2: Write the failing unit tests** (in `src/state.rs`, below the code area)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn event_table() {
        let none = json!({});
        assert_eq!(state_for("prompt-submit", &none), Some(State::Working));
        assert_eq!(state_for("working", &none), Some(State::Working));
        assert_eq!(state_for("stop", &none), Some(State::Waiting));
        assert_eq!(state_for("session-end", &none), Some(State::Idle));
        for other in ["", "start", "ingest", "STOP"] {
            assert_eq!(state_for(other, &none), None, "{other:?}");
        }
    }

    #[test]
    fn notification_yields_only_on_known_subtypes() {
        for kind in ["idle_prompt", "permission_prompt", "elicitation_dialog"] {
            assert_eq!(state_for("notification", &json!({"notification_type": kind})), Some(State::Waiting));
        }
        assert_eq!(state_for("notification", &json!({"notification_type": "other"})), None);
        assert_eq!(state_for("notification", &json!({})), None);
        assert_eq!(state_for("notification", &json!([1])), None);
    }

    #[test]
    fn payload_degrades_to_empty_object() {
        for text in ["", "not json", "[1,2]", "\"str\"", "null"] {
            assert_eq!(read_payload(&mut text.as_bytes()), json!({}), "{text:?}");
        }
        assert_eq!(read_payload(&mut r#"{"a":1}"#.as_bytes()), json!({"a": 1}));
        struct Broken;
        impl std::io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> { Err(std::io::Error::other("no")) }
        }
        assert_eq!(read_payload(&mut Broken), json!({}));
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test -p tx-hooks`
Expected: compile error — `State`, `state_for`, `read_payload` missing.

- [ ] **Step 4: Implement `state.rs`**

```rust
//! The event → state table (port of tx-ide `hooks.rs`): which agent hook means what.

use std::io::Read;

use serde_json::{Map, Value};

pub const PLUGIN_ID: &str = "hooks";
pub const SESSION_ID_ENV: &str = "TX_SESSION_ID";

/// `Notification` subtypes meaning "the agent yielded — needs you".
const WAITING_NOTIFICATION_TYPES: [&str; 3] = ["idle_prompt", "permission_prompt", "elicitation_dialog"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Working,
    Waiting,
    Idle,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Waiting => "waiting",
            State::Idle => "idle",
        }
    }
}

/// `None` is "no transition": an unknown event or a notification that is not a yield.
pub fn state_for(event: &str, payload: &Value) -> Option<State> {
    match event {
        "prompt-submit" | "working" => Some(State::Working),
        "stop" => Some(State::Waiting),
        "session-end" => Some(State::Idle),
        "notification" => payload
            .get("notification_type")
            .and_then(Value::as_str)
            .filter(|kind| WAITING_NOTIFICATION_TYPES.contains(kind))
            .map(|_| State::Waiting),
        _ => None,
    }
}

/// The hook payload, tolerantly: `{}` for none / unreadable / bad JSON / non-object. A hook
/// must never fail the agent over its own input.
pub fn read_payload(stdin: &mut dyn Read) -> Value {
    let mut text = String::new();
    if stdin.read_to_string(&mut text).is_err() {
        return Value::Object(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value @ Value::Object(_)) => value,
        _ => Value::Object(Map::new()),
    }
}
```

Run: `cargo test -p tx-hooks` → 3 passed.

- [ ] **Step 5: Write the failing end-to-end tests**

```rust
// tx-core-cli/tests/hooks.rs
//! The hooks plugin through the binary: `txc hook <event>` inside a session, `hook status`.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::io::Write;

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}

struct Cli {
    dir: tempfile::TempDir,
}

impl Cli {
    fn new() -> Self {
        Self { dir: tempfile::Builder::new().prefix("txc").tempdir_in("/tmp").unwrap() }
    }
    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_txc"));
        command.arg("--state-dir").arg(self.state()).args(args).env_remove("TMUX").env_remove("TX_SESSION_ID");
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }
    fn run(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
        let mut child = self.command(args, env).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args, &[], "");
        assert!(output.status.success(), "txc {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    fn create(&self, name: &str) -> String {
        let cwd = self.dir.path().join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        self.ok(&["create", name, "--cwd", cwd.to_str().unwrap(), "--exec", "sleep", "60"]).trim().to_owned()
    }
    fn show(&self, name: &str) -> serde_json::Value {
        serde_json::from_str(&self.ok(&["show", name])).unwrap()
    }
    fn hook(&self, event: &str, id: &str, stdin: &str) -> Output {
        self.run(&["hook", event], &[("TX_SESSION_ID", id)], stdin)
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = Command::new("tmux").arg("-S").arg(self.state().join("tmux.sock")).arg("kill-server").output();
    }
}

macro_rules! require_tmux {
    () => {
        if !tmux_available() {
            eprintln!("tmux not available; skipping");
            return;
        }
    };
}

#[test]
fn create_seeds_idle_and_hooks_move_the_state() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "idle");
    assert!(cli.hook("prompt-submit", &id, "").status.success());
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "working");
    let since = cli.show("a")["ext"]["hooks"]["since"].as_f64().unwrap();
    assert!(cli.hook("working", &id, "").status.success());
    assert_eq!(cli.show("a")["ext"]["hooks"]["since"].as_f64().unwrap(), since, "same state → no write");
    assert!(cli.hook("stop", &id, "{}").status.success());
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "waiting");
    assert!(cli.hook("notification", &id, r#"{"notification_type":"other"}"#).status.success());
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "waiting");
    assert!(cli.hook("prompt-submit", &id, "").status.success());
    assert!(cli.hook("notification", &id, r#"{"notification_type":"permission_prompt"}"#).status.success());
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "waiting");
    assert!(cli.hook("session-end", &id, "garbage").status.success());
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "idle");
}

#[test]
fn not_ours_exits_zero_and_writes_nothing() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    let before = cli.show("a");
    let output = cli.run(&["hook", "stop"], &[], "");
    assert!(output.status.success(), "no TX_SESSION_ID");
    let output = cli.hook("stop", "0f6b3b8a-1111-4222-8333-444444444444", "");
    assert!(output.status.success(), "unknown session");
    let output = cli.hook("stop", "not-an-id", "");
    assert!(output.status.success(), "malformed id");
    let output = cli.hook("no-such-event", &id, "");
    assert!(output.status.success(), "unknown event");
    assert_eq!(cli.show("a"), before);
}

#[test]
fn unknown_session_is_not_ours() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    cli.ok(&["stop", "a"]);
    cli.ok(&["forget", "a"]);
    let output = cli.hook("stop", &id, "");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
}

#[test]
fn hook_without_event_is_usage() {
    let cli = Cli::new();
    let output = cli.run(&["hook"], &[], "");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn status_lists_sessions_and_flags_stuck() {
    require_tmux!();
    let cli = Cli::new();
    std::fs::create_dir_all(cli.state()).unwrap();
    std::fs::write(cli.state().join("plugins.yaml"), "plugins:\n  hooks: { settings: { stuck_after_seconds: 0 } }\n").unwrap();
    let id = cli.create("a");
    cli.create("b");
    let status = cli.ok(&["hook", "status"]);
    assert!(status.contains("a\tidle\t"), "{status}");
    assert!(status.contains("b\tidle\t"), "{status}");
    assert!(cli.hook("prompt-submit", &id, "").status.success());
    std::thread::sleep(std::time::Duration::from_millis(20));
    let status = cli.ok(&["hook", "status"]);
    assert!(status.contains("a\tstuck\t"), "{status}");
    assert!(status.contains("b\tidle\t"), "{status}");
}

#[test]
fn bad_settings_fail_at_startup() {
    let cli = Cli::new();
    std::fs::create_dir_all(cli.state()).unwrap();
    std::fs::write(cli.state().join("plugins.yaml"), "plugins:\n  hooks: { settings: { stuck_after_seconds: soon } }\n").unwrap();
    let output = cli.run(&["engines"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("stuck_after_seconds"));
}

#[test]
fn disabled_hooks_plugin_has_no_verb_and_no_seed() {
    require_tmux!();
    let cli = Cli::new();
    std::fs::create_dir_all(cli.state()).unwrap();
    std::fs::write(cli.state().join("plugins.yaml"), "plugins:\n  hooks: { enabled: false }\n").unwrap();
    cli.create("a");
    assert!(cli.show("a").get("ext").is_none());
    assert_eq!(cli.run(&["hook", "status"], &[], "").status.code(), Some(2));
}
```

- [ ] **Step 6: Run to verify failure**

Run: `cargo test -p tx-core-cli --test hooks`
Expected: `create_seeds_idle…` fails at `ext.hooks.state` (no plugin yet).

- [ ] **Step 7: Implement the plugin (`plugins/hooks/src/lib.rs`)**

```rust
//! tx-core plugin `hooks`: the agent's own hook system runs `txc hook <event>` inside the
//! session; the plugin records `ext["hooks"] = {"state", "since"}` on the record. Port of
//! tx-ide's `hooks.rs` without history ingest and chat-id capture.

pub mod state;

use std::io::Write;
use std::sync::Arc;

use serde_json::{Value, json};
use tx_core::events::now;
use tx_core::{
    CoreConfig, Event, EventSink, EventType, ListFilter, Lifecycle, Plugin, PluginError,
    Registry, SessionId, TxCore, Verb, VerbError, VerbIo,
};

pub use state::{PLUGIN_ID, SESSION_ID_ENV, State, read_payload, state_for};

pub const DEFAULT_STUCK_AFTER_SECONDS: u64 = 600;

const USAGE: &str = "\
usage: txc hook <event>            record the agent's state (run by the agent's hooks)
       txc hook status             name, state, since per running session
       txc hook install [--dry-run]   register the hooks with claude / codex
       txc hook uninstall
";

#[derive(Clone, Copy, Debug, Default)]
pub struct HooksPlugin;

impl Plugin for HooksPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        let stuck_after = match r.settings().get("stuck_after_seconds") {
            None => DEFAULT_STUCK_AFTER_SECONDS,
            Some(value) => value.as_u64().ok_or_else(|| PluginError::Register {
                plugin: PLUGIN_ID.into(),
                reason: format!("settings.stuck_after_seconds must be a non-negative integer, got {value}"),
            })?,
        };
        r.sink(Box::new(Seed { config: r.config().clone() }));
        r.verb(Arc::new(Hook { stuck_after }));
        Ok(())
    }
}

/// Seeds `idle` on create. Sinks get no core handle, so this opens one on the same state dir
/// (cheap: no tmux call).
struct Seed {
    config: CoreConfig,
}

impl EventSink for Seed {
    fn on_event(&self, event: &Event) {
        if event.type_ != EventType::Created {
            return;
        }
        let Ok(core) = TxCore::open(self.config.clone()) else { return };
        let _ = core.update_ext(&event.id, PLUGIN_ID, |v| *v = record(State::Idle));
    }
}

fn record(state: State) -> Value {
    json!({"state": state.as_str(), "since": now()})
}

struct Hook {
    stuck_after: u64,
}

impl Verb for Hook {
    fn name(&self) -> &'static str {
        "hook"
    }

    fn usage(&self) -> &'static str {
        "hook <event> | status | install [--dry-run] | uninstall   agent hooks → session state"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv.first().map(String::as_str) {
            None | Some("-h" | "--help") => {
                let _ = write!(io.stderr, "{USAGE}");
                Ok(2)
            }
            Some("status") => self.status(core, io),
            Some("install") | Some("uninstall") => {
                let _ = writeln!(io.stderr, "txc hook: {} is not implemented yet", argv[0]);
                Ok(1)
            }
            Some(event) => {
                self.apply(core, io, event)?;
                Ok(0)
            }
        }
    }
}

impl Hook {
    /// "Not ours" (no env var, malformed id, no record, unknown event) is a silent success.
    fn apply(&self, core: &TxCore, io: &mut VerbIo<'_>, event: &str) -> Result<(), VerbError> {
        let Some(id) = std::env::var(SESSION_ID_ENV).ok().and_then(|raw| raw.parse::<SessionId>().ok()) else {
            return Ok(());
        };
        let payload = read_payload(io.stdin);
        let Some(state) = state_for(event, &payload) else { return Ok(()) };
        match core.update_ext(&id, PLUGIN_ID, |v| {
            if v.get("state").and_then(Value::as_str) != Some(state.as_str()) {
                *v = record(state);
            }
        }) {
            Ok(_) | Err(tx_core::CoreError::NotFound(_)) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn status(&self, core: &TxCore, io: &mut VerbIo<'_>) -> Result<i32, VerbError> {
        let filter = ListFilter::new().lifecycle(Lifecycle::Running);
        let mut rows = Vec::new();
        for id in core.list_sessions(&filter)? {
            let session = core.get_session(&id)?;
            let (state, since) = match session.ext.get(PLUGIN_ID) {
                Some(v) => (
                    v.get("state").and_then(Value::as_str).unwrap_or("-").to_owned(),
                    v.get("since").and_then(Value::as_f64),
                ),
                None => ("-".to_owned(), None),
            };
            let shown = match (state.as_str(), since) {
                ("working", Some(since)) if now() - since >= self.stuck_after as f64 => "stuck".to_owned(),
                _ => state,
            };
            rows.push((session.name, shown, since));
        }
        rows.sort();
        for (name, state, since) in rows {
            let since = since.map(|s| format!("{s:.0}")).unwrap_or_else(|| "-".into());
            let _ = writeln!(io.stdout, "{name}\t{state}\t{since}");
        }
        Ok(0)
    }
}
```

Check `ListFilter`'s builder names in `tx-core/src/core.rs:36-247` (`ListFilter::new()` and a `lifecycle(..)` setter exist per the API sweep; if the setter is named differently, use that name). Then: workspace member `plugins/hooks`, CLI dep `tx-hooks`, `MANIFEST` third entry `|| Box::new(tx_hooks::HooksPlugin)`. Update `tests/plugins.rs` expectations in Task 4 to include `hooks\tactive` as the third line.

- [ ] **Step 8: Run everything**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: green. `tests/plugins.rs::without_a_yaml_every_plugin_is_active` now expects `engine-claude\tactive\nengine-codex\tactive\nhooks\tactive\n` and the disabled test `…\nhooks\tactive\n` — adjust those two asserts.

- [ ] **Step 9: Commit**

```bash
git add -A && git commit -m "feat(plugins): hooks plugin — hook <event>, hook status, idle seed"
```

---

### Task 6: `hook install` / `hook uninstall`

**Files:**
- Create: `plugins/hooks/src/install.rs`
- Modify: `plugins/hooks/src/lib.rs` (`Hook::run` arms)
- Test: `plugins/hooks/src/install.rs` (unit, temp HOME), `tx-core-cli/tests/hooks.rs` (one end-to-end round trip)

**Interfaces:**
- Produces: `install::install(home: &Path, exe: &Path, dry_run: bool, out: &mut dyn Write) -> Result<(), InstallError>` and `install::uninstall(home: &Path, out: &mut dyn Write) -> Result<(), InstallError>`.
- Files touched under `home`: `.claude/settings.json`, `.claude/.tx-core-managed.json` (sidecar), `.codex/hooks.json` + `.codex/.tx-core-managed.json` (only when `.codex/` exists), backups `<file>.bak.<unix-ts>`.

Entries are identified by the exact command strings the sidecar recorded (no foreign keys in the agent's settings: Claude Code validates its settings schema).

- [ ] **Step 1: Write the failing unit tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn home() -> tempfile::TempDir {
        tempfile::Builder::new().prefix("txc-home").tempdir_in("/tmp").unwrap()
    }
    fn read(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn install_creates_missing_settings() {
        let home = home();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), false, &mut out).unwrap();
        let settings = read(&home.path().join(".claude/settings.json"));
        let stop = &settings["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(stop["type"], "command");
        assert_eq!(stop["command"], "/opt/txc hook stop");
        assert_eq!(settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"], "/opt/txc hook prompt-submit");
        assert_eq!(settings["hooks"]["Notification"][0]["hooks"][0]["command"], "/opt/txc hook notification");
        assert_eq!(settings["hooks"]["SessionEnd"][0]["hooks"][0]["command"], "/opt/txc hook session-end");
        assert_eq!(settings["hooks"]["StopFailure"][0]["hooks"][0]["command"], "/opt/txc hook stop");
        assert!(home.path().join(".claude/.tx-core-managed.json").exists());
        assert!(!home.path().join(".codex").exists(), "codex untouched when absent");
    }

    #[test]
    fn install_is_idempotent_and_keeps_foreign_hooks() {
        let home = home();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let before = json!({"theme": "dark", "hooks": {"Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "say done"}]}]}});
        std::fs::write(claude.join("settings.json"), before.to_string()).unwrap();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), false, &mut out).unwrap();
        install(home.path(), Path::new("/opt/txc"), false, &mut out).unwrap();
        let settings = read(&claude.join("settings.json"));
        assert_eq!(settings["theme"], "dark");
        let stop_hooks: Vec<_> = settings["hooks"]["Stop"].as_array().unwrap().iter()
            .flat_map(|b| b["hooks"].as_array().unwrap().clone()).collect();
        assert_eq!(stop_hooks.len(), 2, "foreign + ours, once: {stop_hooks:?}");
        let backups: Vec<_> = std::fs::read_dir(&claude).unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("settings.json.bak.")).collect();
        assert_eq!(backups.len(), 1, "one backup, taken before the first write");
    }

    #[test]
    fn uninstall_restores_byte_identical() {
        let home = home();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        let text = "{\n  \"theme\": \"dark\",\n  \"hooks\": {\n    \"Stop\": [{\"matcher\": \"\", \"hooks\": [{\"type\": \"command\", \"command\": \"say done\"}]}]\n  }\n}\n";
        std::fs::write(claude.join("settings.json"), text).unwrap();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), false, &mut out).unwrap();
        uninstall(home.path(), &mut out).unwrap();
        assert_eq!(std::fs::read_to_string(claude.join("settings.json")).unwrap(), text);
        assert!(!claude.join(".tx-core-managed.json").exists());
    }

    #[test]
    fn uninstall_without_sidecar_is_a_noop() {
        let home = home();
        let mut out = Vec::new();
        uninstall(home.path(), &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("nothing to do"));
    }

    #[test]
    fn install_refuses_non_object() {
        let home = home();
        let claude = home.path().join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(claude.join("settings.json"), "[1]").unwrap();
        let mut out = Vec::new();
        let error = install(home.path(), Path::new("/opt/txc"), false, &mut out).unwrap_err();
        assert!(matches!(error, InstallError::NotObject(_)), "{error}");
        assert_eq!(std::fs::read_to_string(claude.join("settings.json")).unwrap(), "[1]");
    }

    #[test]
    fn dry_run_writes_nothing() {
        let home = home();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), true, &mut out).unwrap();
        assert!(!home.path().join(".claude").exists());
        assert!(String::from_utf8(out).unwrap().contains("hook stop"));
    }

    #[test]
    fn codex_hooks_when_codex_dir_exists() {
        let home = home();
        std::fs::create_dir_all(home.path().join(".codex")).unwrap();
        let mut out = Vec::new();
        install(home.path(), Path::new("/opt/txc"), false, &mut out).unwrap();
        let hooks = read(&home.path().join(".codex/hooks.json"));
        assert_eq!(hooks["hooks"]["Stop"][0]["hooks"][0]["command"], "/opt/txc hook stop");
        uninstall(home.path(), &mut out).unwrap();
        let hooks = read(&home.path().join(".codex/hooks.json"));
        assert_eq!(hooks, json!({}));
    }
}
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p tx-hooks install::`
Expected: compile error — `install`, `uninstall`, `InstallError` missing.

- [ ] **Step 3: Implement `install.rs`**

```rust
//! `hook install` / `hook uninstall`: add the `txc hook <event>` commands to the agents' hook
//! files behind a sidecar that records exactly what was added, so uninstall removes only that.
//! No foreign keys go into the agents' files (Claude Code validates its settings schema).

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// Agent hook event → our verb event.
const EVENTS: [(&str, &str); 5] = [
    ("UserPromptSubmit", "prompt-submit"),
    ("Stop", "stop"),
    ("StopFailure", "stop"),
    ("Notification", "notification"),
    ("SessionEnd", "session-end"),
];
const SIDECAR: &str = ".tx-core-managed.json";

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: std::io::Error },
    #[error("{}: not valid JSON: {source}", path.display())]
    Json { path: PathBuf, source: serde_json::Error },
    #[error("{0} is not a JSON object")]
    NotObject(String),
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> InstallError + '_ {
    move |source| InstallError::Io { path: path.to_owned(), source }
}

/// One agent's hook file: where it is and how a hook entry nests.
struct Target {
    file: PathBuf,
    sidecar: PathBuf,
    /// Claude nests under `hooks.<Event>[].hooks[]`; so does Codex (`hooks.json` root is
    /// `{"hooks": {...}}`), so both share one shape.
    label: &'static str,
}

fn targets(home: &Path, only_existing: bool) -> Vec<Target> {
    let mut out = vec![Target {
        file: home.join(".claude/settings.json"),
        sidecar: home.join(".claude").join(SIDECAR),
        label: "claude",
    }];
    if !only_existing || home.join(".codex").is_dir() {
        out.push(Target {
            file: home.join(".codex/hooks.json"),
            sidecar: home.join(".codex").join(SIDECAR),
            label: "codex",
        });
    }
    out
}

fn read_json(path: &Path) -> Result<Map<String, Value>, InstallError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(source) => return Err(InstallError::Io { path: path.to_owned(), source }),
    };
    match serde_json::from_str::<Value>(&text).map_err(|source| InstallError::Json { path: path.to_owned(), source })? {
        Value::Object(map) => Ok(map),
        _ => Err(InstallError::NotObject(path.display().to_string())),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<(), InstallError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io(dir))?;
    }
    let mut text = serde_json::to_string_pretty(value).expect("JSON value");
    text.push('\n');
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).map_err(io(&tmp))?;
    std::fs::rename(&tmp, path).map_err(io(path))
}

fn backup(path: &Path) -> Result<Option<PathBuf>, InstallError> {
    if !path.exists() {
        return Ok(None);
    }
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let backup = PathBuf::from(format!("{}.bak.{ts}", path.display()));
    std::fs::copy(path, &backup).map_err(io(&backup))?;
    Ok(Some(backup))
}

fn commands(exe: &Path) -> Vec<(&'static str, String)> {
    EVENTS.iter().map(|(event, verb)| (*event, format!("{} hook {verb}", exe.display()))).collect()
}

fn blocks<'a>(root: &'a mut Map<String, Value>, event: &str) -> Result<&'a mut Vec<Value>, InstallError> {
    let hooks = root.entry("hooks").or_insert_with(|| Value::Object(Map::new()));
    let hooks = hooks.as_object_mut().ok_or_else(|| InstallError::NotObject("hooks".into()))?;
    let blocks = hooks.entry(event.to_owned()).or_insert_with(|| Value::Array(Vec::new()));
    blocks.as_array_mut().ok_or_else(|| InstallError::NotObject(format!("hooks.{event}")))
}

fn has_command(blocks: &[Value], command: &str) -> bool {
    blocks.iter().any(|block| {
        block.get("hooks").and_then(Value::as_array).is_some_and(|hooks| {
            hooks.iter().any(|h| h.get("command").and_then(Value::as_str) == Some(command))
        })
    })
}

pub fn install(home: &Path, exe: &Path, dry_run: bool, out: &mut dyn Write) -> Result<(), InstallError> {
    for target in targets(home, true) {
        let mut root = read_json(&target.file)?;
        let mut added = Vec::new();
        for (event, command) in commands(exe) {
            let blocks = blocks(&mut root, event)?;
            if has_command(blocks, &command) {
                continue;
            }
            blocks.push(json!({"matcher": "", "hooks": [{"type": "command", "command": command, "timeout": 10}]}));
            added.push(json!({"event": event, "command": command}));
        }
        if added.is_empty() {
            let _ = writeln!(out, "{}: already current", target.label);
            continue;
        }
        for entry in &added {
            let _ = writeln!(out, "{}: add {}  {}", target.label, entry["event"].as_str().unwrap_or(""), entry["command"].as_str().unwrap_or(""));
        }
        if dry_run {
            let _ = writeln!(out, "{}: dry-run, nothing written", target.label);
            continue;
        }
        // The sidecar remembers the first install's backup so uninstall can name it.
        let mut sidecar = read_json(&target.sidecar)?;
        if sidecar.is_empty() {
            let backup = backup(&target.file)?;
            sidecar.insert("version".into(), json!(1));
            sidecar.insert("backup".into(), backup.map(|b| json!(b.display().to_string())).unwrap_or(Value::Null));
            sidecar.insert("added".into(), json!([]));
        }
        if let Some(list) = sidecar.get_mut("added").and_then(Value::as_array_mut) {
            list.extend(added);
        }
        write_json(&target.file, &Value::Object(root))?;
        write_json(&target.sidecar, &Value::Object(sidecar))?;
        let _ = writeln!(out, "{}: written {}", target.label, target.file.display());
    }
    Ok(())
}

pub fn uninstall(home: &Path, out: &mut dyn Write) -> Result<(), InstallError> {
    let mut any = false;
    for target in targets(home, false) {
        if !target.sidecar.exists() {
            continue;
        }
        any = true;
        let sidecar = read_json(&target.sidecar)?;
        let added: Vec<(String, String)> = sidecar
            .get("added")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|e| Some((e.get("event")?.as_str()?.to_owned(), e.get("command")?.as_str()?.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let mut root = read_json(&target.file)?;
        if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
            for (event, command) in &added {
                if let Some(blocks) = hooks.get_mut(event).and_then(Value::as_array_mut) {
                    blocks.retain(|block| {
                        !block.get("hooks").and_then(Value::as_array).is_some_and(|hooks| {
                            hooks.len() == 1 && hooks[0].get("command").and_then(Value::as_str) == Some(command)
                        })
                    });
                    if blocks.is_empty() {
                        hooks.remove(event);
                    }
                }
            }
            if hooks.is_empty() {
                root.remove("hooks");
            }
        }
        // Byte-identical restore when nothing else changed since install: prefer the backup.
        let backup = sidecar.get("backup").and_then(Value::as_str).map(PathBuf::from);
        match backup.filter(|b| b.exists()) {
            Some(backup) if read_json(&backup)? == root => {
                std::fs::copy(&backup, &target.file).map_err(io(&target.file))?;
            }
            _ => write_json(&target.file, &Value::Object(root))?,
        }
        std::fs::remove_file(&target.sidecar).map_err(io(&target.sidecar))?;
        let _ = writeln!(out, "{}: removed {} hook(s) from {}", target.label, added.len(), target.file.display());
    }
    if !any {
        let _ = writeln!(out, "nothing to do: no tx-core sidecar under {}", home.display());
    }
    Ok(())
}
```

Add `thiserror.workspace = true` to `plugins/hooks/Cargo.toml`. In `lib.rs`: `pub mod install;` and replace the `install | uninstall` arm:

```rust
            Some("install") => {
                let dry_run = argv.get(1).is_some_and(|a| a == "--dry-run");
                let exe = std::env::current_exe().map_err(|e| VerbError::Failed(format!("current_exe: {e}")))?;
                install::install(&home_dir()?, &exe, dry_run, io.stdout).map_err(|e| VerbError::Failed(e.to_string()))?;
                Ok(0)
            }
            Some("uninstall") => {
                install::uninstall(&home_dir()?, io.stdout).map_err(|e| VerbError::Failed(e.to_string()))?;
                Ok(0)
            }
```

with

```rust
fn home_dir() -> Result<std::path::PathBuf, VerbError> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(std::path::PathBuf::from)
        .ok_or_else(|| VerbError::Failed("HOME is not set".into()))
}
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p tx-hooks`
Expected: all pass. If `uninstall_restores_byte_identical` fails because the backup comparison sees a different `Map` ordering, compare `serde_json::Value`s (it does — `Map` equality is key-set based with `preserve_order`), so investigate `read_json(&backup)? == root` before changing the test.

- [ ] **Step 5: End-to-end round trip** (append to `tx-core-cli/tests/hooks.rs`)

```rust
#[test]
fn install_then_uninstall_round_trips_under_a_temp_home() {
    let cli = Cli::new();
    let home = cli.dir.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let text = "{\"theme\": \"dark\"}\n";
    std::fs::write(home.join(".claude/settings.json"), text).unwrap();
    let env = [("HOME", home.to_str().unwrap())];
    let output = cli.run(&["hook", "install"], &env, "");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let settings: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap().to_owned();
    assert!(command.ends_with("txc hook stop"), "{command}");
    assert!(std::path::Path::new(command.trim_end_matches(" hook stop")).is_absolute());
    let output = cli.run(&["hook", "uninstall"], &env, "");
    assert!(output.status.success());
    assert_eq!(std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(), text);
}
```

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` → green.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat(hooks): hook install / uninstall behind a sidecar"
```

---

### Task 7: Docs and non-goal updates

**Files:**
- Modify: `README.md` (layout table, commands block, "Recommended next extension points"), `docs/superpowers/specs/2026-10-15-tx-core-design.md:14-26` (non-goals)

- [ ] **Step 1: README**

In `poc/README.md`: the crates table gains rows `plugins/engine-claude`, `plugins/engine-codex`, `plugins/hooks` (paths changed; `tx-core` row mentions `plugin.rs`); add a "Plugins" section:

```markdown
## Plugins

Everything but the core is a plugin: a crate under `plugins/<id>/` implementing `tx_core::Plugin`,
which fills any of three slots — engines, event sinks, verbs. `txc` links them through
`tx-core-cli/src/plugins.rs::MANIFEST` and switches them in `<state_dir>/plugins.yaml`
(`--plugins FILE` overrides):

​```yaml
plugins:
  engine-codex: { enabled: false }
  hooks:
    settings: { stuck_after_seconds: 600 }
​```

Missing file = everything on. `txc plugins` lists ids and status; `txc help` lists plugin verbs.
Plugins keep per-session data in the record's `ext.<id>` object via `TxCore::update_ext`.

| plugin | contributes |
|---|---|
| `engine-claude`, `engine-codex` | one engine each |
| `hooks` | `txc hook <event>` (run by the agent's own hooks; `hook install` registers them), `hook status`, an `idle` seed on create |
```

Remove the "Engine binaries at runtime" / "a plugin host inside the library" lines from "Recommended next extension points" and point to the spec.

- [ ] **Step 2: The 2026-10-15 spec's non-goals**

Replace the two bullets "Cordis or any plugin host inside the library…" and "Engine activity (working / waiting / idle)…" with one line: `- (Superseded 2026-10-16 by the plugins spec: the library hosts plugins; engine activity is the hooks plugin's.)`

- [ ] **Step 3: Final verification and commit**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: green.

```bash
git add -A && git commit -m "docs(poc): plugins section, superseded non-goals"
```
