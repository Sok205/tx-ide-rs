# tx-core proof of concept

A plain core for a swarm of interactive coding agents. Each agent is an ordinary process inside a
detached session on tx's **own** tmux server. Agents keep running after the CLI that launched
them exits; attaching shows the agent's native terminal; detaching leaves it running.

This directory is its own Cargo workspace, separate from the `tx` binary in `../crates`.

```
cd poc
cargo test --workspace                                  # needs tmux on PATH; uses private sockets
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p tx-core-cli -- help                        # the txc binary
```

Design spec: `../docs/superpowers/specs/2026-10-15-tx-core-design.md`.
Decisions the spec left open: `../docs/superpowers/plans/2026-10-15-tx-core.md`.

## Crates

| crate | role | depends on |
|---|---|---|
| `tx-core` | the library: `TxCore` and the public operations | serde, serde_json, thiserror, uuid, tempfile |
| `plugins/engine-claude` (`tx-engine-claude`) | plugin: the `EngineAdapter` for Claude Code | tx-core |
| `plugins/engine-codex` (`tx-engine-codex`) | plugin: the `EngineAdapter` for Codex | tx-core |
| `plugins/hooks` (`tx-hooks`) | plugin: `txc hook …`, the agent's hooks → a per-session working / waiting / idle state | tx-core |
| `plugins/ide` (`tx-ide`) | plugin: `txc ui install\|attach`; composes the server's `tmux.conf` from plugin resources; the theme, navigation keys and helper scripts | tx-core, tx-ui |
| `plugins/labels` (`tx-labels`) | plugin: session labels (prefix+s, pane-border labels), `whoami` | tx-core, tx-ui |
| `plugins/kill` (`tx-kill`) | plugin: `txc kill`, prefix+X | tx-core, tx-ui |
| `plugins/picker` (`tx-picker`) | plugin: the fzf session picker `txc pick`, prefix+t | tx-core, tx-ui |
| `plugins/edit` (`tx-edit`) | plugin: the session and tag editors, prefix+e | tx-core, tx-ui |
| `plugins/spawn` (`tx-spawn`) | plugin: the new-agent form `txc _new-agent`, prefix+n; ships the COMMON and DEVELOPER roles and their skills | tx-core, tx-ui |
| `plugins/assistant` (`tx-assistant`) | plugin: `txc assistant warm`, the `tx-assistant` session `ui attach` lands in; ships the TX-ASSISTANT role | tx-core, tx-ui |
| `tx-ui` | library, not a plugin: the socket-bound `tmux::Ui` helper, the row renderer (`render`) and the modal editor (`editor`) shared by the key plugins | tx-core |
| `tx-core-cli` | `txc`: a thin CLI over the library; registers the bundled plugins | tx-core and every plugin above |

The library never parses arguments and never reads the process environment. `CoreConfig`
carries everything; only `txc` touches `std::env`. Without `--profile-dir` / `--skill-dir`,
`txc` resolves roles from `<state_dir>/agents` (the roles plugins ship, written by `ui install`)
and `<state_dir>/user-agents` (yours, never pruned), with skills under each one's `skills/`.

## Architecture

```
            txc (CLI)             your program / IDE
                 \                     /
                  v                   v
          +---------------------------------+
          |  TxCore  (one struct, open once) |
          |  config   CoreConfig            |
          |  store    SessionStore   <state_dir>/sessions/<id>.json
          |  tmux     Tmux           tmux -S <state_dir>/tmux.sock -f <state_dir>/tmux.conf
          |  events   EventLog       <state_dir>/log.jsonl
          |  engines  EngineRegistry id -> Box<dyn EngineAdapter>
          |  profiles Profiles       role + skill search dirs
          |  locks    Locks          <state_dir>/locks/{create,<id>}.lock
          +---------------------------------+
                        |
                        v   new-session -d -e TX_SESSION_ID=.. -e TX_TMUX_SOCKET=.. -e TX_CORE_HOME=.. -- program args
          +---------------------------------+
          |  private tmux server            |   session option @tx-id = record id
          |  session "worker-1"  pane %3    |   $N / %N looked up by @tx-id, never stored
          |  session "reviewer"  pane %5    |
          +---------------------------------+
```

### Where state lives

| home | what |
|---|---|
| disk (`sessions/<id>.json`) | the record: id, name, parent, tags, cwd, timestamps, lifecycle, launch. The snapshot every read returns. |
| tmux | the link only: session option `@tx-id` = record id |
| looked up on demand | `#{session_id}` (`$N`) and `#{pane_id}` (`%N`) found by `@tx-id`, so a rename in tmux cannot break them |

`Lifecycle` is `Running`, `Exited` (vanished) or `Stopped` (`stop_session`). The two terminal
states are final: nothing in core moves a record out of them.

### One operation, end to end: `create_session`

1. Validate the name (`[A-Za-z0-9_-]{1,128}`), the env keys, and that `cwd` is a directory;
   canonicalize `cwd`.
2. Pure checks before any lock: the engine must be registered, the profile must resolve.
3. Take `create.lock`. Refuse the name if a `Running` record holds it.
4. Mint the id. For an agent: write `profiles/<id>.md`, call `engine.launch(&request)` (pure),
   apply the plan's workspace actions under `cwd`, merge its env.
5. `tmux new-session -d -s <name> -c <cwd> -e K=V … -- program args`, then `set-option @tx-id`.
6. Save the record, append `created` to the log. If anything after step 5 fails, the tmux session
   is killed and the profile file removed: no orphans.

Env layering, later wins: `TX_SESSION_ID`, `TX_TMUX_SOCKET`, `TX_CORE_HOME`, then `CoreConfig::env`,
then `CreateOptions::env`, then the engine plan's env.

### Liveness and messaging

- `list-sessions -F '#{@tx-id}\t#{session_id}\t#{pane_id}'` is the only liveness signal. "No
  server" means nothing is live; any other failure is `TmuxError::InspectionFailed` and the
  operation writes nothing. Reads (`list_sessions`, `get_session`, `resolve`) never touch tmux.
- `refresh_session` / `refresh_all` turn a `Running` record without a live `@tx-id` into
  `Exited`. Records are scanned before tmux is listed, so a session created in between is never
  misjudged.
- `send_message` wraps the text in `<from-user>…</from-user>` or
  `<from-agent session="name">…</from-agent>`, types it verbatim into the pane (`send-keys -l`,
  trailing `;` escaped), waits 300 ms, sends `Enter`. The whole delivery holds `<id>.lock`, so
  concurrent senders never interleave. A receipt means "submitted", nothing more.

### Concurrency

| lock | held across | prevents |
|---|---|---|
| `create.lock` | name check → tmux create → record save; also `rename_session` | two processes taking one name |
| `<id>.lock` | load → modify → save for rename, tags, stop, refresh | lost updates |
| `<id>.lock` | the whole `send_message` | interleaved keystrokes |

Lock order is always `create.lock` before `<id>.lock`. Locks are `std::fs::File::lock`
(flock); every acquisition re-checks that the path still names the locked inode.

### Safety properties the tests pin

- Ids and names are validated everywhere, including when read back from disk and from tmux, so
  no record, lock or profile file can land outside its directory.
- Record writes are temp-file + fsync + rename + directory fsync; a record whose id disagrees
  with its file name, or that is not a regular file, is reported, never silently used.
- Every tmux call carries `-S` and `-f`; `~/.tmux.conf` and the user's default server are never
  touched. `=name` and `=name:` exact targets; `--` before user-controlled arguments; arguments
  ending in `;` are escaped so tmux cannot read them as a second command.
- The event log appends one line of at most 512 bytes with a single `O_APPEND` write.

## How to extend

### An engine today (20 lines, no change to core)

```rust
use tx_core::{EngineAdapter, EngineError, LaunchPlan, LaunchRequest, WorkspaceAction};

pub struct Aider;

impl EngineAdapter for Aider {
    fn id(&self) -> &str { "aider" }

    fn launch(&self, r: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        let mut args = vec!["--read".into(), r.profile.file().display().to_string()];
        if let Some(model) = r.model { args.extend(["--model".into(), model.into()]); }
        if let Some(prompt) = r.initial_prompt { args.extend(["--message".into(), prompt.into()]); }
        let mut plan = LaunchPlan::new("aider").args(args);
        for dir in r.profile.skill_dirs() {
            plan = plan.workspace(WorkspaceAction::Symlink {
                src: dir.clone(),
                dst: std::path::PathBuf::from(".aider/skills").join(dir.file_name().unwrap()),
            });
        }
        Ok(plan)
    }
}

// in your binary:
core.register_engine(Box::new(Aider))?;
```

`launch` is pure: no I/O. Core writes the profile file, applies the workspace actions (relative
to `cwd`, refusing paths that escape it), starts the process. That makes an adapter testable
with a `Profile` and nothing else, as the two bundled engines show. Put each engine in its own
crate depending only on `tx-core`; the CLI registers the bundled ones in `main`.

### Reading the operation log from outside

`log.jsonl` is `{"ts","actor","type","msg"}` per line; types are `created`, `renamed`, `tagged`,
`message-sent`, `exited`, `stopped`, `forgotten`. It is a notification trail, not the record:
on an event, re-read the session with `get_session`. Lines are never torn, so `tail -f` works.

### From inside an agent

Every session carries `TX_SESSION_ID`, `TX_TMUX_SOCKET` and `TX_CORE_HOME`, so an agent can
run `txc send <name> …` and the message arrives as `<from-agent session="…">`. `txc` treats the
caller as an agent only when `$TMUX` names tx's own socket; anywhere else the sender is the user.

## Plugins

Everything but the core is a plugin: a crate under `plugins/<id>/` implementing `tx_core::Plugin`,
which fills any of four slots — engines, event sinks, verbs, resources (`tx-core/src/plugin.rs`). `txc`
links them through `tx-core-cli/src/plugins.rs::MANIFEST` and switches them in
`<state_dir>/plugins.yaml` (`--plugins FILE` overrides):

```yaml
plugins:
  engine-codex: { enabled: false }
  hooks:
    settings: { stuck_after_seconds: 600 }
```

Missing file = everything on; an unknown id or a malformed file is a usage error. `txc plugins`
lists ids and status; `txc help` lists plugin verbs. Plugins keep per-session data in the
record's `ext.<id>` object via `TxCore::update_ext`. Design:
`../docs/superpowers/specs/2026-10-16-poc-plugins-design.md`.

A resource is a named static blob a plugin publishes with `Registry::resource(name, bytes)`
(`keys/<id>.tmux`, `bin/<file>`, `conf/<file>`, `agents/<file>`); the core stores it and lists it with
`TxCore::resources(prefix)` but never reads one, and a disabled plugin's resources are absent just
like its verbs (D8 in `../docs/superpowers/specs/2026-10-19-poc-key-plugins-design.md`).

| plugin | contributes |
|---|---|
| `engine-claude`, `engine-codex` | one engine each |
| `hooks` | `txc hook <event>` (run by the agent's own hooks; `txc hook install` registers them in `~/.claude/settings.json` / `~/.codex/hooks.json` behind a sidecar, `uninstall` reverses it), `txc hook status`, an `idle` seed on create |
| `ide` | `txc ui install\|attach` (and the hidden `_resource <name>`); resources `conf/tmux.conf` (theme), `keys/ide.tmux` (navigation keys, C-u / C-d, M-digits, status line, pane-border look), `bin/tmux-nav`, `bin/tmux-system-resources` |
| `labels` | `whoami` and the internal `_relabel`, `_pane-label`, `_pane-info`, `_tmux-name`; resource `keys/labels.tmux` (prefix+s, the pane-border label, the after-new-window hook) |
| `kill` | `txc kill <name>`; resources `keys/kill.tmux` (prefix+X) and `bin/tmux-kill-tx-session` |
| `picker` | `txc pick` (the fzf session picker) and the internal `_list`; resource `keys/picker.tmux` (prefix+t). Its Ctrl-T and Ctrl-D / Y binds appear only while `edit` and `kill` are active |
| `edit` | the internal `_edit-session` and `_edit-tag`; resources `keys/edit.tmux` (prefix+e) and `bin/tmux-edit-tx-session`; owns `ext.edit` (`{"group": …}`) |
| `spawn` | the internal `_new-agent <pane> <client>` (the form, then `create_session`); resources `keys/spawn.tmux` (prefix+n), `bin/tmux-new-agent`, `agents/COMMON.md`, `agents/DEVELOPER.md`, `agents/skills/**` (from the repo's `agents/` tree) |
| `assistant` | `assistant warm` (start the `tx-assistant` session unless it is running; prints its name); resource `agents/TX-ASSISTANT.md` |

The picker's role column reads `ext.spawn.kind`, which a later plugin writes; until then every
process session shows `shell`.

`_new-agent` runs main's nvim form (`plugins/spawn/nvim/tx-new-agent.lua`, embedded and written
to a temp dir at run time): name, tags, project (the firing pane's directory), prompt, engine
(`core.engine_ids()`), model, effort (`low` to `max`) and role (every `<NAME>.md` in the profile
dirs but COMMON, which every agent gets; `none` first, DEVELOPER preselected). Needs `nvim`.
The session is created in the popup; the outcome lands on the client's status line.

`assistant warm` creates `tx-assistant` with the engine `claude`, roles COMMON + TX-ASSISTANT,
model `opus`, effort `medium`, tag `tx-system`, in the state dir (so the engine's skill links
land under `<state_dir>/.claude/skills`). A `Running` record whose tmux session is gone is
refreshed to `Exited` and the assistant started again.

## Running the IDE

Needs tmux, `fzf` and `curl` (the picker reloads itself through fzf's `--listen` port).

```
txc ui install    # writes <state_dir>/{bin,keys,conf}/* and <state_dir>/tmux.conf
txc ui attach     # attaches this terminal to the core server's `home` session
```

`ui install` materialises each active plugin's resources under `<state_dir>/{bin,keys,conf}` and
writes `<state_dir>/tmux.conf`, which sets `@tx-bin` and `@tx-state` and runs `keys/ide.tmux`
and then every other active plugin's `keys/<id>.tmux`. The key bindings and `bin/*` find `txc`
through those two server options, so re-run `ui install` after rebuilding `txc`. Files in those
directories that no active plugin provides are deleted, so `txc` needs nothing from the source
tree at run time. It refuses a binary or state path containing whitespace, a backslash, a
backtick or any of `"'$#;`, since the paths are spliced into tmux config lines.

Disabling a plugin in `plugins.yaml` and re-running `ui install` removes that plugin's keys, even
on a running server: `ui install` unbinds the keys the plugin's fragment declared, clears the
options and hooks it declared, and prints `reloaded`. When it finds no previous
`keys/.installed` on a running server (a server set up by an older `ui install`), it also prints
`legacy bindings may remain; restart the server to drop them`.

`ui attach` creates `home` on first use as a view (status bar and pane-border labels on) and
attaches to it. When the `assistant` plugin is active it first runs `assistant warm` and starts
`home`'s pane nest-attached to `tx-assistant`, so the IDE opens on the assistant; a failed warm
(no `claude`, roles not installed) is printed to stderr and `home` opens as a plain view. An
existing `home` is left as it is. To try the IDE, press prefix+n in it, or start a session by
hand and press the picker key:

```
txc create demo --cwd ~/code/project --exec zsh
txc ui attach     # then prefix+t
```

The prefix is `C-a`.

| keys | does |
|---|---|
| prefix+t | session picker; Enter nests into the session |
| prefix+n | new agent: the form, then the session in the pane's directory |
| prefix+e | edit the session's name, tags and group |
| prefix+X | kill the session, with a confirm prompt |
| prefix+s | refresh labels, then `choose-tree` |
| C-h / C-j / C-k / C-l | move between panes; the move bubbles out of nested sessions |
| C-u / C-d | page the agent's scrollback |

The pane border shows the nested session's name and tags. Later plugins bring their own keys
(prefix+T, /, f, a) in their own `keys/<id>.tmux`.

### Adding a plugin

A plugin is a crate under `poc/plugins/<id>` (package `tx-<id>`, depending on `tx-core` and, for
anything that talks to tmux, `tx-ui`) whose `Plugin` impl registers verbs, engines, sinks and
resources. Wire it in with three edits: a `plugins/<id>` entry in `members` in `poc/Cargo.toml`,
a `tx-<id> = { path = "../plugins/<id>" }` dependency in `poc/tx-core-cli/Cargo.toml`, and one
line in `tx-core-cli/src/plugins.rs::MANIFEST`; a verb may not take a name in
`plugins.rs::RESERVED`. Give it a `tests/resources.rs` (what it publishes, and nothing when
disabled; `kill`'s is the template) and a `poc/tx-core-cli/tests/<id>.rs` through the binary
(the `tests/common` harness has the state dir, a private server, and `shim_path` for stand-in
executables such as a `claude` that sleeps).

Resources land under the state dir when `ui install` runs: `bin/` and `keys/` executable,
`conf/` and `agents/` plain; files there that no active plugin provides are deleted, so a plugin
that publishes any of these must also be listed in `tests/ide.rs::ui_install_materialises_resources`.

### Adding a key

A key is a plugin, with:

- its verb(s), registered with `Registry::verb`;
- a `tmux/keys.tmux` registered as resource `keys/<id>.tmux`, which binds the key. A comment
  line near its top declares the prefix keys it binds: `# keys: <key>` (`none` for root-table binds only). If it
  sets global state, it also declares `# unset: <option> …` and `# unhook: <hook> …`, so that
  `ui install` can undo them when the plugin is disabled;
- optionally helper scripts, registered as `bin/<file>` resources (they land in `<state_dir>/bin`,
  mode 0755).

Nothing else in the source needs editing: `ui install` finds the fragment by its resource name,
and `<id>: { enabled: false }` in `plugins.yaml` turns the key off. Adding a key also means
extending `poc/tx-core-cli/tests/ide.rs`: the expected conf (`expected_conf`) with the new
`run-shell` line, `start_server`'s probe and `ui_install_fragment_loads` with the new bind, and
`fragments_do_nothing_outside_an_installed_server` with the id.

Run `ui attach` from a bare terminal. Inside another tmux whose socket is not the core's it
nests, and the outer tmux's prefix wins: press the prefix twice to reach the IDE's keys. Inside
the core's own server it switches the current client to `home` instead of nesting.

### Recommended next extension points

These came out of an architecture review and are deliberately not built yet. They are cheap to
add now and breaking to add later.

| point | shape | guardrail |
|---|---|---|
| Lifecycle hooks | `trait EventSink: Send + Sync { fn on_event(&self, e: &Event); }` + `TxCore::subscribe(Box<dyn EventSink>)`; `Event` carries the session id | called after locks drop; no `Result` (hooks cannot veto); no `&TxCore` (re-entry would deadlock on flock) |
| Engine envelope | default method `EngineAdapter::envelope(&self, sender, text) -> String` | pure; core still types it |
| Chat ops (resume, fork, transcripts) | separate crate with a second registry keyed by engine id, `trait EngineChatOps` | one future `resume` field on `LaunchRequest` |
| Workspace preparers (worktrees) | a function run by the caller before `create_session`; cleanup via an `EventSink` on `Stopped`/`Exited` | not a core concern (spec non-goal) |
| Declarative engines | one plugin `Declared { id, program, args }` with `{profile_file}`/`{model}`/`{prompt}` substitution, registered N times from `plugins.yaml` settings | ids validated by the registry |
| Launch decorators | a fourth registry slot letting a plugin edit a `LaunchPlan` (per-session hook injection without touching `~/.claude`) | pure, ordered by manifest |

Before freezing the API: add `Send + Sync` to `EngineAdapter`, put a session id in every log
line, mark `CoreError`, `EventType`, `WorkspaceAction`, `LaunchRequest`, `LaunchPlan`,
`CreateOptions`, `Session` and `CoreConfig` `#[non_exhaustive]` with builder constructors, and
re-export only the types a plugin author needs.

What not to add: generics over the store, async traits, dylib or WASM engines, hooks that can
veto, runtime (re)loading of plugins. Dynamic dispatch costs nothing next to a tmux spawn
(about 3 ms); scans of the record directory, not plugins, are what would slow a large swarm.

## Performance envelope (macOS, tmux 3.6a, release build)

| operation | cost |
|---|---|
| `create_session` | about 12 ms (two tmux spawns, two fsyncs) |
| `list_live` / `refresh_all` | 3 to 6 ms, flat up to 200 sessions |
| `list_sessions` / `resolve` | 1.2 ms at 100 records, 193 ms at 10 000 |
| `send_message` | 340 ms, of which 300 ms is the delivery pause |

The delivery pause is the one real throughput ceiling (about three messages per second per
recipient); making it a per-engine hint is the first optimization worth doing.

## Testing approach

Every module has unit tests; the integration suites in `tx-core/tests` cover the spec's ten
verification scenarios with real tmux on private sockets under `/tmp`, including eight OS
processes racing for one name and six threads sending into one pane. The `fuzz_*.rs` files are
property-based and adversarial suites written by separate review agents against each module;
the bugs they found (non-UTF-8 paths, path traversal through ids, trailing `;` in `send-keys`
and in `new-session` argv, FIFOs in the record dir, a create/refresh race) are all fixed and
pinned. Two tests stay `#[ignore]`d as documented platform limits: a cooked-mode tty drops a
single line over 1024 bytes, and flock cannot protect a lock file that is deleted under a holder.
