# tx-core plugins — one host, three slots, a YAML switchboard

Status: design, 2026-10-16. Extends `2026-10-15-tx-core-design.md`; supersedes its non-goals
"no plugin host inside the library" and "engine activity". Grounded in `poc/` @ `3cf23d8`.

## Purpose

Port tx-ide's features into the poc as plugins without growing the CLI by hand. The two engine
crates already are plugins in shape (a crate, a trait, a registration call); this spec makes
that the one mechanism for everything else, gives it an on/off switch, and ports the first
feature — engine hooks (working / waiting / idle) — through it.

## Decisions

- **Core is the host.** `tx-core` gains `plugin.rs`: the `Plugin` trait, a `Registry` with
  three slots (engines, event sinks, verbs), `TxCore::plug`. Engines and sinks were already
  core concepts; verbs are the one new one. No separate `tx-plugins` crate.
- **One trait, many capabilities.** A plugin fills whichever slots it needs. A new capability
  is a new slot on `Registry`, never a new plugin type.
- **Core never sees YAML.** `plugins.yaml` is parsed by `txc` into a plain `PluginConfig`.
- **Plugin state lives in the session record.** `Session` gains `ext: BTreeMap<String, Value>`
  keyed by plugin id, written under the session lock. No plugin-private stores.
- **Compiled in.** All plugins are workspace crates under `poc/plugins/<id>/`, linked by `txc`
  through one `MANIFEST`. No dylibs, no subprocess protocol.

## Core additions (`poc/tx-core`)

### `plugin.rs`

```rust
pub trait Plugin: Send + Sync {
    /// `[a-z0-9-]+`, unique across the manifest. Also the key in plugins.yaml and in `ext`.
    fn id(&self) -> &'static str;
    fn register(&self, r: &mut Registry) -> Result<(), PluginError>;
}

pub struct Registry { /* engines: Vec<Box<dyn EngineAdapter>>, sinks: Vec<Box<dyn EventSink>>,
                        verbs: BTreeMap<String, Box<dyn Verb>>, settings: Value */ }
impl Registry {
    pub fn engine(&mut self, e: Box<dyn EngineAdapter>);
    pub fn sink(&mut self, s: Box<dyn EventSink>);
    pub fn verb(&mut self, v: Box<dyn Verb>) -> Result<(), PluginError>;   // DuplicateVerb
    pub fn settings(&self) -> &serde_json::Value;   // this plugin's block; Value::Null if none
}

pub trait Verb: Send + Sync {
    fn name(&self) -> &'static str;      // bare word, must not shadow a built-in txc verb
    fn usage(&self) -> &'static str;     // one line for the usage text
    fn run(&self, core: &mut TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError>;
}
pub struct VerbIo<'a> { pub stdin: &'a mut dyn Read, pub stdout: &'a mut dyn Write, pub stderr: &'a mut dyn Write }

#[derive(Default)]
pub struct PluginConfig {
    pub disabled: BTreeSet<String>,
    pub settings: BTreeMap<String, serde_json::Value>,
    pub reserved: BTreeSet<String>,   // verb names the front end owns
}
pub enum PluginStatus { Active, Disabled, Failed(String) }

impl TxCore {
    /// Register every plugin not in `cfg.disabled`. Fails on the first error (duplicate id,
    /// bad id, duplicate verb, duplicate engine id, a plugin's own `register` error); the
    /// core is then unusable for plugins and the caller exits.
    pub fn plug(&mut self, plugins: Vec<Box<dyn Plugin>>, cfg: &PluginConfig) -> Result<(), PluginError>;
    pub fn verb(&self, name: &str) -> Option<&dyn Verb>;
    pub fn verbs(&self) -> impl Iterator<Item = &dyn Verb>;      // name order
    pub fn plugin_status(&self) -> Vec<(String, PluginStatus)>;  // manifest order
}
```

`plug` runs each plugin's `register` against a fresh `Registry`, then moves engines into
`EngineRegistry` (reusing its `EngineTaken` check), sinks into `sinks`, verbs into a new
`verbs: BTreeMap` field. A disabled plugin is recorded as `Disabled` and never registered.
An id in `cfg.disabled` or `cfg.settings` that matches no plugin is `PluginError::Unknown`.

`Verb::run` takes argv already split and after `txc`'s globals; core still never parses
arguments or reads the environment. A verb may read env itself (it is CLI-side code).

### `Session.ext`

```rust
#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
pub ext: BTreeMap<String, serde_json::Value>,
```

`RECORD_VERSION` stays 1; records without `ext` load as empty. `show` prints it as part of the
record. New API:

```rust
impl TxCore {
    /// Take the session lock, load, hand `ext[plugin]` (Null if absent) to `f`, save if it
    /// changed. `f` must not call back into the core.
    pub fn update_ext(&self, id: &SessionId, plugin: &str, f: impl FnOnce(&mut Value)) -> Result<bool, CoreError>;
}
```

No event is logged for an `ext` change (plugins log their own if they care).

## CLI (`poc/tx-core-cli`)

- `const MANIFEST: &[fn() -> Box<dyn Plugin>]` lists every plugin crate; `main.rs:93-94`'s
  hardcoded `register_engine` calls go away.
- New global `--plugins FILE`; default `<state_dir>/plugins.yaml`. Missing file = everything
  enabled. Parsed with `serde_yml` into `PluginConfig`:

  ```yaml
  plugins:
    engine-claude: { enabled: true }
    engine-codex:  { enabled: false }
    hooks:
      enabled: true
      settings: { stuck_after_seconds: 600 }
  ```

  `enabled` defaults to true; `settings` is passed through as JSON. A malformed file or an
  unknown id is a usage error (exit 2) naming the key — never a silent fallback.
- Dispatch: the built-in `match` stays; its `_` arm looks up `core.verb(name)` and runs it with
  real stdio, else usage error. Built-in names win: `PluginConfig` has a `reserved:
  BTreeSet<String>` the CLI fills with its verb names, and `plug` fails with
  `PluginError::ReservedVerb` when a plugin registers one of them.
- `txc plugins` prints `id  active|disabled|failed: <why>` in manifest order.
- Usage text gains a `plugin verbs:` section built from `core.verbs()`.

## Layout

```
poc/
  tx-core/            (+ src/plugin.rs, Session.ext)
  tx-core-cli/        (+ plugins.yaml loader, MANIFEST, `plugins` verb)
  plugins/
    engine-claude/    (git mv tx-engine-claude; + impl Plugin → r.engine(Box::new(Claude)))
    engine-codex/     (same)
    hooks/            (new)
```

Crate names stay `tx-engine-claude` / `tx-engine-codex`; plugin ids are `engine-claude`,
`engine-codex`, `hooks`.

## The `hooks` plugin (`poc/plugins/hooks`)

Port of `crates/tx/src/hooks.rs` without history ingest and chat-id capture (those go to the
later `history` / `chat` plugins). The agent's hook system (Claude Code `UserPromptSubmit`,
`Stop`, `Notification`, `SessionEnd`; Codex's hooks) runs `txc hook <event>` inside the
session; the plugin records an activity state in `ext["hooks"]`:

```json
{ "state": "working" | "waiting" | "idle", "since": 1760600000.0 }
```

### Verbs

- `hook <event>` — `TX_SESSION_ID` from the env; absent, or no such session → exit 0 silently
  ("not ours"). Stdin payload read tolerantly: none / unreadable / bad JSON / non-object → `{}`.
  Table: `prompt-submit`, `working` → working; `stop` → waiting; `session-end` → idle;
  `notification` → waiting only if `payload.notification_type ∈ {idle_prompt,
  permission_prompt, elicitation_dialog}`, else no-op; anything else → no-op. A transition is
  written with `update_ext`; same state → no write. Exit is 0 on every handled path; only an
  I/O failure of the state dir exits 1.
- `hook install [--dry-run]` / `hook uninstall` — port of `_claude-settings` and
  `_codex-hooks`: add `{"matcher": "", "hooks": [{"type": "command", "command": "txc hook
  <event>"}]}` entries to `~/.claude/settings.json` (`UserPromptSubmit→prompt-submit`,
  `Stop|StopFailure→stop`, `Notification→notification`, `SessionEnd→session-end`) and the Codex equivalents to `~/.codex/hooks.json` when that dir
  exists. Idempotent: entries are identified by a `_tx_core_managed: true` key on the hook
  object; a marker sidecar `~/.claude/.tx-core-managed.json` records what was added; the file
  is backed up to `<file>.bak.<unix-ts>` before the first write. `uninstall` removes exactly
  the marked entries and the sidecar. `txc` is referenced by the absolute path of the running
  binary (`current_exe`), so the hook works regardless of PATH.
- `hook status` — one line per running session: `name  state  since`, where a `working`
  session older than `settings.stuck_after_seconds` (default 600) prints `stuck`. Sessions
  with no `ext.hooks` print `-`. Read-only.

### Sink

On `Created`, seeds `ext["hooks"] = {"state": "idle", "since": now}` so `status` has a row at
once. Done via a second `TxCore` opened on the same state dir (sinks get no core handle, per
the `EventSink` contract); the sink keeps that core in a `OnceLock`.

### Settings

`stuck_after_seconds: u64`, default 600; a non-integer is a `register` error.

## Testing

- **core** (`tx-core/tests/plugins.rs`): plug a stub plugin filling all three slots → engine
  launchable, sink receives `Created`, verb reachable; disabled plugin → status `Disabled`,
  its verb absent; unknown id in config → `Unknown`; two plugins with the same verb name →
  `DuplicateVerb`; `update_ext` concurrency (two threads, both writes survive); a v1 record
  without `ext` loads; a record with `ext` round-trips. Proptest arm in `fuzz_session` for
  `ext`.
- **cli** (`tx-core-cli/tests/plugins.rs`): `txc plugins` listing with and without a yaml;
  unknown id → exit 2 with the key in stderr; `enabled: false` hides the verb from usage and
  dispatch; `--plugins` override.
- **hooks** (`plugins/hooks/tests/`): event table and payload degradation as unit tests;
  integration through the CLI harness: create a `--exec sleep` session, run `txc hook stop`
  with `TX_SESSION_ID` / `TX_CORE_HOME` set → `show` has `ext.hooks.state == "waiting"`; wrong
  id → exit 0, record unchanged; `hook install` then `hook uninstall` against a temp `HOME`
  leaves `settings.json` byte-identical and the backup present; `hook status` shows `stuck`
  with `stuck_after_seconds: 0`.

All suites keep the existing isolation: a temp state dir in `/tmp`, private tmux socket,
`require_tmux!` skip.

## Out of scope (follow-ups)

- A `launch` slot letting plugins decorate `LaunchPlan` (per-session hook injection without
  touching `~/.claude`).
- Demoting a stuck `working` session (needs a timer or a reconcile pass).
- `ls` columns for plugin state; `history`, `chat`, picker, statusline plugins.
- Runtime reconfiguration (cordis-style reconcile); the process is short-lived.
