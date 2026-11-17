# Architecture

The repository is one Cargo workspace. Its crates fall into three layers, and dependencies only
point downwards.

## The crates

```text
              tx-core-cli   (the tx and txi binaries; still links every plugin)
             /     |      \
       plugins/*   |    tx-client   (the tmux layer; also the `ide` plugin)
             \     |      /
               tx-plugin        (what a plugin program shares with tx; the SDK)
                   |
                tx-core         (the library)
```

- **`tx-core`** is the library: [`TxCore`](../internal/tx_core/core/struct.TxCore.html), opened
  once, owns the agent store, the private tmux server, the event log, the engine registry, role
  resolution and file locks. It depends on serde, thiserror, uuid and tempfile, and nothing in
  the workspace.
- **`plugins/<id>`** are one crate each (package `tx-<id>`). The engine adapters
  (`engine-claude`, `engine-codex`) depend on `tx-core` alone. The IDE plugins also depend on
  `tx-client`, and a few reuse a sibling's library code: `home` and `timeline` use `tx-chat`'s
  transcript readers, `tour` uses `tx-artifacts`' store.
- **`tx-client`** is the code the key plugins share: the socket-bound tmux helper, the views
  store, the row renderer, the modal editor, the fzf prompt and the colour palette. It is also a
  plugin itself, `ide`, which owns `tx ui attach | view` and the updater that writes the IDE's
  server config on `tx update`.
- **`tx-core-cli`** builds the binaries. `tx` is a thin CLI over the library that registers the
  bundled plugins; `txi` is the internal entry point.
- **`tx-plugin`** is what any program that runs as tx shares with it, so a plugin program can
  depend on it without linking the other plugins: the global flags, the directory resolution,
  the `config.yaml` reader and opening the core; it depends on `tx-core` alone. For a plugin
  written in Rust it is the SDK: `tx_plugin::sdk::main` is the whole `main` of the plugin's
  program (see [Writing plugins](writing-plugins.md)).

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

Everything but the core is a plugin, and a plugin reaches `tx` in one of two ways.

**A directory with a manifest.** `<root>/<id>/plugin.yaml` plus the files it names is a plugin:
its verbs are programs under `bin/`, in any language. The roots are `<data>/shipped/` (the
plugins tx ships, written by `tx update`) and then `<config>/plugins/` (your own, which tx never
writes); `--plugin-dir`, repeatable, or `TX_PLUGIN_PATH` replaces both. `tx` finds the installed
plugins through an index it caches in `<cache>/plugins-index.json` and trusts while every
manifest and root still has the modification time and size it had, so a run reads that one file
and costs a `stat` per plugin, whatever their number. When a verb is not built in, `tx` looks it up among the active plugins
without opening the core and **replaces itself** with the plugin's program (`execve`): standard
streams, the terminal, signals and the exit code are the plugin's own. [Writing
plugins](writing-plugins.md) is the reference.

**Compiled in.** A type implementing
[`Plugin`](../internal/tx_core/plugin/trait.Plugin.html), with an `id` and a `register` method,
listed in `MANIFEST` in `tx-core-cli/src/plugins.rs`. `register` receives a
[`Registry`](../internal/tx_core/plugin/struct.Registry.html) and fills any of five slots:

- **verbs**: named commands with `name`, `usage` and `run(core, io, argv)`. `tx help` lists them;
  names starting with `_` are internal (called by key bindings and hooks).
- **engines**: an [`LlmEngine`](../internal/tx_core/engine/trait.LlmEngine.html) adapter for a
  coding-agent CLI.
- **event sinks**: receivers of the core's events.
- **updaters**: what `tx update` runs for the plugin after the core's update steps, to bring the
  files it owns up to this build. Each must be idempotent; its `retire` takes back what it
  generated once the plugin stops being active.
- **resources**: named static blobs the core stores and lists but never reads. By convention the
  name is `<kind>/<file>`: `keys/<id>.tmux` (a key-binding fragment), `conf/<file>` (tmux config),
  `bin/<file>` (a helper script), `agents/<file>` (a role or skill). The `ide` plugin's updater
  (`tx update`) writes them
  under the data directory.

The two ways are one API. A plugin crate keeps its `Plugin`, its verbs and its updater, and gains
a manifest and a three-line program that hosts it (`tx_plugin::sdk::main`): the plugin runs
compiled into `tx` where no bundle of its id is installed, and as its own program where one is.
**An installed plugin always replaces the compiled-in one of the same id**, whether it is
enabled, disabled or in error. `mockup` is the first to ship both ways; the others follow, and
the compiled-in list goes once they all have.

Both kinds are switched in the `plugins:` block of `<config>/config.yaml`:

```yaml
plugins:
  engine-codex: { enabled: false }
  artifacts:
    settings: { editor: nvim }
```

No entry enables a plugin. A disabled plugin's verbs, engines and resources are all absent. An
entry that names no plugin (a typo, a plugin that is not installed, a retired id) stops nothing:
it is ignored and listed as `missing`. `tx plugins` prints one row per plugin, `<id>`, its status
(`active`, `disabled`, `error`, `missing`) and, for an error, why: the installed ones in plugin
order (root order, then id), then the compiled-in ones, then the missing entries;
`tx plugins --json` adds where each comes from (`shipped`, `user` or `built-in`) and what its
manifest declares. Plugin order also settles a clash: a verb two plugins declare stays with the
first, and a verb named like a built-in is never a plugin's; the plugin that lost one stays
active and its row says so. Plugins keep per-agent data in the record's `ext.<id>` object through
`TxCore::update_ext`; core never reads it.

## Plugins act through `tx`, not through each other

The home plugin's design calls this rule D3: a plugin acts on the core through the library, and
on other plugins through their verbs. It does not write agent records it does not own, set tmux
options on other plugins' sessions, call into another plugin's internals, or read another
plugin's data files. There are two ways to run a verb, for two needs:

- **`TxCore::verb(name)`**, when the plugin wants the output. In a core opened through
  `tx-plugin` it answers for every active plugin's verb, wherever that plugin runs: a verb of a
  plugin linked into the process runs in-process, and any other is a proxy that starts the
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
`tx help`, and its one verb today is `txi hook <Event>`. When an engine adapter launches an agent
it wires the engine CLI's own hook mechanism to run `txi hook <Event>` (Claude through
`--settings`, Codex through `-c hooks.<Event>=…`). `txi` opens the same core from the `TX_*_DIR`
variables every session carries and hands the payload to `TxCore::hook`, which is how agent state
moves (see [Agents and records](agents-and-records.md)). A hook must never fail the agent: anything that is
not tx's (a call outside a session, an unknown id) exits 0.

The hook command is `txi`, next to the `tx` that launched the agent. `tx` names itself when it
starts and `txi` names its sibling; a plugin program that launches an agent is told through
`TX_BIN`, which every session and every plugin program carries.
