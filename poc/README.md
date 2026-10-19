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
| `tx-core-cli` | `txc`: a thin CLI over the library; registers the bundled engines | the three above |

The library never parses arguments and never reads the process environment. `CoreConfig`
carries everything; only `txc` touches `std::env`.

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
which fills any of three slots — engines, event sinks, verbs (`tx-core/src/plugin.rs`). `txc`
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

| plugin | contributes |
|---|---|
| `engine-claude`, `engine-codex` | one engine each |
| `hooks` | `txc hook <event>` (run by the agent's own hooks; `txc hook install` registers them in `~/.claude/settings.json` / `~/.codex/hooks.json` behind a sidecar, `uninstall` reverses it), `txc hook status`, an `idle` seed on create |

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
