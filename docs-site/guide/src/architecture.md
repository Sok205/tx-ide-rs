# Architecture

The repository is one Cargo workspace. `tx` and every plugin are separate programs built from
it, and dependencies only point downwards.

## The crates

```text
tx-core-cli        tx and txi: arguments, the built-in verbs, dispatch. It links no plugin.
    |
tx-plugin          resolves the directories from flags and environment, reads config.yaml, opens
    |              the core with the installed plugins; the SDK `main` of a plugin program; the
    |              installer `tx update` lays the shipped plugins out with
tx-core            records, lifecycle, engines (the trait and the wire format), the plugin host:
                   manifest, index, the proxies that stand for another program's verb, engine
                   or sink

plugins/<id>       one package per plugin: a library, the program tx-<id>, plugin.yaml, resources
tx-client          the tmux and nvim library the IDE plugins and tx share
```

- **`tx-core`** is the library: [`TxCore`](../internal/tx_core/core/struct.TxCore.html), opened
  once, owns the agent store, the private tmux server, the event log, the engine registry, role
  resolution and file locks. It depends on serde, thiserror, uuid and tempfile, and nothing in
  the workspace.
- **`tx-plugin`** is what any program that runs as tx shares with it: the global flags, the
  directory resolution, the `config.yaml` reader and opening the core; it depends on `tx-core`
  alone. For a plugin written in Rust it is the SDK: `tx_plugin::sdk::main` is the whole `main`
  of the plugin's program (see [Writing plugins](writing-plugins.md)).
- **`tx-core-cli`** builds the two binaries on `PATH`. `tx` holds the built-in verbs (`create`,
  `ls`, `send`, `stop`, …) and is a dispatcher for every other verb: it finds the verb among the
  installed plugins and replaces itself with that plugin's program. `txi` is the internal entry
  point. Neither links a plugin, an engine adapter or a resource: the package depends on
  `tx-core`, `tx-plugin` and `tx-client`, and nothing under `plugins/`.
- **`plugins/<id>`** are one package each (`tx-<id>`): the plugin's library, a three-line program
  that hosts it, its manifest and the files it publishes. The engine adapters (`engine-claude`,
  `engine-codex`) depend on `tx-core` and `tx-plugin` alone. The IDE plugins also depend on
  `tx-client`, and a few reuse a sibling's library code: `home` and `timeline` use `tx-chat`'s
  transcript readers, `tour` uses `tx-artifacts`' store. `home-themes` has no program of its own:
  `tx-home` hosts it. `setup` is not built on the SDK: it is the installer's wizard.
- **`tx-client`** is the code the key plugins share: the socket-bound tmux helper, the views
  store, the row renderer, the modal editor, the fzf prompt and the colour palette. It is a
  library only and, of the workspace, depends on `tx-core` alone. The `ide` plugin, which owns
  `tx ui attach | view` and the updater that writes the IDE's server config on `tx update`, is
  `plugins/ide`, a plugin like the others.

## The library never reads the environment

`tx-core` never parses arguments and never touches `std::env`. Everything it needs arrives in a
[`CoreConfig`](../internal/tx_core/config/struct.CoreConfig.html): the five directories, the
socket path, the plugin roots, extra environment for launched sessions, and the hook command
engines should wire. Only `tx-plugin` reads the process environment, resolves the XDG
directories and builds the config, for `tx`, `txi` and every plugin program alike. This keeps
the library testable in isolation (every integration test opens a core on a
temporary directory and a private socket) and lets any program embed it:

```rust,ignore
let mut core = TxCore::open(CoreConfig::new(Directories::under("/tmp/tx-demo")))?;
core.register_engine(Box::new(tx_engine_claude::Claude))?;
```

The same principle runs the other way. When a plugin needs to re-invoke `tx` (from a key binding,
a popup or a detached child), it passes `CoreConfig::cli_arguments()`, the five directory flags
plus `--socket` when it is overridden and `--plugin-dir` for each plugin root that was named, so
the child opens the same core. Inside tmux, key fragments read those words from the server option
`@tx-arguments`.

## The plugin model

Everything but the core and the built-in verbs is a plugin, and **a plugin is a directory**:
`<root>/<id>/plugin.yaml` plus the files it names. Its verbs, its engine, its updater and its
event sink, if it has them, are programs under `bin/`, in any language. The roots are
`<data>/shipped/` (the plugins tx ships, written by `tx update`) and then `<config>/plugins/`
(your own, which tx never writes); `--plugin-dir`, repeatable, or `TX_PLUGIN_PATH` replaces
both. Same format, same loader. [Writing plugins](writing-plugins.md) is the reference.

`tx` finds the installed plugins through an index it caches in `<cache>/plugins-index.json` and
trusts while every manifest and root still has the modification time and size it had, so a run
reads that one file and costs a `stat` per plugin, whatever their number. When a verb is not
built in, `tx` looks it up among the active plugins without opening the core and **replaces
itself** with the plugin's program (`execve`): standard streams, the terminal, signals and the
exit code are the plugin's own. A verb no active plugin has is `unknown verb`, exit 2. With no
plugin installed `tx` still has its built-ins, and no engine: `tx create` then fails with
`engine claude is not enabled (enabled: )`.

A manifest declares up to five things:

- **verbs**: named commands, each a program and the words put before the caller's arguments.
  `tx help` lists them; names starting with `_` are internal (called by key bindings and hooks).
- **an engine**: a program that speaks the engine protocol for a coding-agent CLI
  ([below](#an-engine-that-is-its-own-program)).
- **an event sink**, `on-event`: a program handed the core's events ([Event sinks](#event-sinks)).
- **an updater**, `update`: what `tx update` runs for the plugin after the core's update steps,
  to bring the files it owns up to this build. It must be idempotent. `tx update` runs it again
  with `TX_PLUGIN_RETIRE=1` to retire a plugin that is disabled or left the shipped selection,
  so that it takes back what it generated.
- **resources**: files of the plugin that `tx update` publishes under the data directory. The
  name is `<kind>/<path>`: `keys/<id>.tmux` (a key-binding fragment), `conf/<file>` (tmux
  config), `bin/<file>` (a helper script), `nvim/…`, `agents/<file>` (a role),
  `harness/…`. `tx update` writes core's own roles and harness, then every active
  plugin's resources in plugin order, into `bin`, `keys`, `conf`, `nvim`, `roles` and
  `harness`; a later source replaces an earlier one of the same name; see [Files](files.md).

**A plugin written in Rust** is a crate with a type implementing
[`Plugin`](../internal/tx_core/plugin/trait.Plugin.html): an `id` and a `register` method that
fills a [`Registry`](../internal/tx_core/plugin/struct.Registry.html) with verbs
(`run(core, io, argv)`), an engine ([`LlmEngine`](../internal/tx_core/engine/trait.LlmEngine.html)),
event sinks and updaters. Its program is three lines, `tx_plugin::sdk::main`, which opens the
core as `tx` would, with the plugins it hosts registered in-process and a proxy for every other
active plugin's verb, engine and sink. So `core.verb("layout")` and `core.create_agent(…)` answer
in a plugin program as they do in `tx`. This API is the SDK; `tx` links no plugin.

Plugins are switched in the `plugins:` block of `<config>/config.yaml`:

```yaml
plugins:
  engine-codex: { enabled: false }
  artifacts:
    settings: { editor: nvim }
```

No entry enables a plugin: one that is installed is active unless its entry says
`enabled: false`. A disabled plugin's verbs, engine, sink, updater and resources are all absent.
An entry that names no plugin (a typo, a plugin that is not installed, a retired id) stops
nothing: it is ignored and listed as `missing`. `tx plugins` prints one row per plugin, `<id>`,
its status (`active`, `disabled`, `error`, `missing`) and, for an error, why: the installed ones
in plugin order (root order, then id), then the missing entries; `tx plugins --json` adds where
each comes from (`shipped` or `user`) and what its manifest declares. Plugin order also settles
a clash: a verb two plugins declare stays with the first, as does an engine kind or the `ext`
key of an engine, and a verb named like a built-in is never a plugin's; the plugin that lost one
stays active and its row says so. Plugins keep per-agent data in the record's `ext.<id>` object
through `TxCore::update_ext`; core never reads it.

**Two versions.** A manifest's `api` is the contract between tx and any plugin; a manifest
written for an `api` this tx does not support is an `error` row and nothing of it loads. The
shipped plugins have a second one: they link the same `tx-core` as `tx` and read the records
directly, so they must come from the same build. `tx update` stamps `<data>/shipped/.stamp`
with its version, and a `tx` that finds another one there says so once on stderr and carries
on, on every verb but `update` and the internal `_…` ones:
`tx: shipped plugins are from tx 0.1.0: run install --upgrade`; or
`tx: no shipped plugins are installed: run install` when there is no stamp and the install
already has data. The stamp is one more input of the index, so the check is a `stat`. With the
roots replaced the shipped plugins are not what runs, and nothing is said.

## The processes

There is no daemon. Every process that needs the core opens it itself on the same directories;
the store's locks make that safe.

| who runs | what starts | how |
|---|---|---|
| you, a key, an agent | `tx <built-in>` | one process |
| the same | `tx <plugin verb>` | `tx` execs `bin/<program>`: one process after the exec |
| core, in any process that opened it | an engine | a child process speaking JSON lines, kept until its parent exits |
| the engine's CLI (a hook) | the engine's own program (`hook:`), else `txi hook` | one process; `txi` adds the engine child |
| core, after an event | a plugin's `on-event` program | one short process per event |
| `tx update` | a plugin's `update` program | one process per plugin |
| a plugin, for another plugin's verb | that plugin's program, through `TxCore::verb()` | a child process, its output captured |

A long-running screen (the home, the graph, the timeline) starts no process per refresh tick
because of the plugin boundary, and nothing spawns a process while it holds an agent's session
lock, with the one exception named under [engines](#an-engine-that-is-its-own-program).

## Plugins act through `tx`, not through each other

The home plugin's design calls this rule D3: a plugin acts on the core through the library, and
on other plugins through their verbs. It does not write agent records it does not own, set tmux
options on other plugins' sessions, call into another plugin's internals, or read another
plugin's data files. There are two ways to run a verb, for two needs:

- **`TxCore::verb(name)`**, when the plugin wants the output. In a core opened through
  `tx-plugin` it answers for every active plugin's verb, wherever that plugin runs: a verb of a
  plugin the program hosts runs in-process, and any other is a proxy that starts the
  plugin's program directly (not through `tx`) with standard input closed, and hands back its
  output, its error and its exit code. `tx ui attach` uses it to hand the home pane to the `home`
  plugin, the home to list layouts. `core.verb(name).is_some()` is the test for "is that feature
  installed and on".
- **`$TX_BIN`**, when the plugin wants a command line: a key binding, a popup, a view, a hook.
  When the home screen stops an agent it runs `tx stop <id>`; when it opens the timeline it runs
  `tx _timeline-attach` in a popup. A plugin program is not `tx`, so it takes the path from
  `tx_plugin::sdk::tx_bin()` (the `tx` that started it); `std::env::current_exe()` is right only
  for its own verbs.

The payoff is that any plugin can be disabled, or not installed at all, without breaking the
others: a missing verb is a missing feature, not a link error. Library-level reuse (the
transcript parser in `tx-chat`) is fine.

## `tx` and `txi`

Both binaries share `tx-core-cli`'s library: argument parsing, the plugin switchboard and opening
the core. `tx` is what people type. `txi` is what machines run: nothing in it is listed in
`tx help`, and its one verb is `txi hook <Event>`. When an engine launches an agent it
wires the engine CLI's own hook mechanism (Claude through `--settings`, Codex through
`-c hooks.<Event>=…`) to a hook command: its own program when its manifest names one, as both
shipped engines do, else `txi hook <Event>`. `txi` opens the same core from the `TX_*_DIR`
variables every session carries and hands the payload to `TxCore::hook`, which is how agent state
moves (see [Agents and records](agents-and-records.md)). A hook must never fail the agent:
anything that is not tx's (a call outside a session, an unknown id) exits 0. `txi` starts the
agent's engine before it takes the agent's lock (next section). An agent launched before its
engine had a hook entry of its own (an agent started by v0.1.0) keeps the `txi hook` command it
was started with, and that keeps working: `tx update` after the upgrade runs update step 1 and
asks for a respawn, and a respawned agent wires its engine's own program.

`txi` is the one next to the `tx` that launched the agent. `tx` names itself when it starts and
`txi` names its sibling; a plugin program that launches an agent is told through `TX_BIN`, which
every session and every plugin program carries.

## An engine that is its own program

The `engine` section of a manifest names a program that speaks the engine protocol: one JSON
request per line on its standard input, one response per line on its standard output ([Writing
plugins](writing-plugins.md#an-engine) is the reference). In every core opened through
`tx-plugin`, the engine of an active plugin is registered as a proxy,
[`ProcessEngine`](../internal/tx_core/plugin/external/struct.ProcessEngine.html); only the
engine's own program has the adapter in its process, and the rest of core cannot tell the two
apart. Engine kinds are open names: `tx engines` lists the ones an active engine provides, and a
launch on any other fails as `engine X is not enabled`.

The proxy starts the program on the first call that needs it and keeps it for the life of the
process. A command that launches or stops an agent starts one program and asks it for every call
(`configure`, `start`, `archive`). `tx send`, `tx interrupt`, `tx ls`, `tx show`, `tx where`,
`tx rename` and `tx tag` start none: the default envelope, the manifest's fixed answer to
`interrupt` and the record itself answer them. Reading a conversation never starts an engine: it
is two reserved keys of the engine's data.

Hooks are the hot path (Claude fires two around every tool call), and there are two ways through
it:

- An engine whose manifest has `hook:` wires **its own program** as the hook command
  (`<plugin>/bin/<program> --tx-hook <Event>`). That program hosts the engine: it opens the core
  and applies the hook in-process. One process per hook, and no protocol.
- An engine without one gets `txi hook <Event>`, and `txi` reaches it through the protocol. `txi`
  **starts the engine's program before it takes the agent's session lock**: it reads the record
  without the lock and calls `LlmEngine::warm`. The lock is then held for one exchange with a
  running program, not for the start of a process, which every other hook of the session would
  wait for.

`tx respawn` warms the same way before core resumes the agent. Off the hook path the program can
still start while the agent's lock is held. It happens at most once per command, never per hook
and never per refresh tick, since the program is kept:

- when an agent is found exited (by `tx ls`, `tx show`, `tx create` or a screen that refreshes),
  or a session that had already vanished is stopped, and its engine lists `archive`;
- when a plugin respawns an agent in-process instead of running `tx respawn`;
- on `tx interrupt` and `tx send`, for an engine whose manifest lists the `interrupt` or the
  `receive_message` method. No shipped engine does.

The shipped engines, `claude` and `codex`, run this way, each with a hook entry of its own; so
do an engine plugin of your own and the stand-in engine of the tests.

## Event sinks

Every operation that changes an agent emits events: core appends each to `<data>/log.jsonl`,
then hands it to every sink subscribed in the process, once the record is saved and every lock
released. A sink registered in-process is a function call. The `on-event` program of an active
installed plugin is subscribed in every core opened through `tx-plugin`, after the in-process
ones, as an [`ExecSink`](../internal/tx_core/plugin/sink/struct.ExecSink.html): for each event of
a type it lists, it starts the program, writes the event as one JSON line (the detail whole,
where the log cuts its line at 512 bytes) and waits for it, up to the manifest's timeout. So the
process that emitted the event is the one that runs the sinks, one at a time, in plugin order:
there is no daemon to deliver to, and no sink costs anything while no plugin declares one. A
sink cannot fail or veto the operation; one past its timeout is killed with its process group and
named on stderr. Delivery is at most once; a consumer that must miss nothing follows
`log.jsonl` instead ([Writing plugins](writing-plugins.md#an-event-sink-on-event) is the
reference).
