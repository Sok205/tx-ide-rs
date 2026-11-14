# Architecture

The repository is one Cargo workspace. Its crates fall into three layers, and dependencies only
point downwards.

## The crates

```text
              tx-core-cli   (the tx and txi binaries; links every plugin)
             /     |      \
       plugins/*   |    tx-client   (the tmux layer; also the `ide` plugin)
             \     |      /
                 tx-core        (the library)
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
  plugin itself, `ide`, which owns `tx ui install | attach | view`.
- **`tx-core-cli`** builds the binaries. `tx` is a thin CLI over the library that registers the
  bundled plugins; `txi` is the internal entry point.

## The library never reads the environment

`tx-core` never parses arguments and never touches `std::env`. Everything it needs arrives in a
[`CoreConfig`](../internal/tx_core/config/struct.CoreConfig.html): the five directories, the
socket path, extra environment for launched sessions, and the hook command engines should wire.
Only `tx` and `txi` read the process environment, resolve the XDG directories and build the
config. This keeps the library testable in isolation (every integration test opens a core on a
temporary directory and a private socket) and lets any program embed it:

```rust,ignore
let mut core = TxCore::open(CoreConfig::new(Directories::under("/tmp/tx-demo")))?;
core.register_engine(Box::new(tx_engine_claude::Claude))?;
```

The same principle runs the other way. When a plugin needs to re-invoke `tx` (from a key binding,
a popup or a detached child), it passes `CoreConfig::cli_arguments()`, the five directory flags
plus `--socket` when it is overridden, so the child opens the same core. Inside tmux, key
fragments read those words from the server option `@tx-arguments`.

## The plugin model

Everything but the core is a plugin: a type implementing
[`Plugin`](../internal/tx_core/plugin/trait.Plugin.html), which has an `id` and a `register`
method. `register` receives a [`Registry`](../internal/tx_core/plugin/struct.Registry.html) and
fills any of four slots:

- **verbs**: named commands with `name`, `usage` and `run(core, io, argv)`. `tx help` lists them;
  names starting with `_` are internal (called by key bindings and hooks).
- **engines**: an [`LlmEngine`](../internal/tx_core/engine/trait.LlmEngine.html) adapter for a
  coding-agent CLI.
- **event sinks**: receivers of the core's events.
- **resources**: named static blobs the core stores and lists but never reads. By convention the
  name is `<kind>/<file>`: `keys/<id>.tmux` (a key-binding fragment), `conf/<file>` (tmux config),
  `bin/<file>` (a helper script), `agents/<file>` (a role or skill). `tx ui install` writes them
  under the data directory.

`tx` links its plugins through a compiled-in list, `MANIFEST` in `tx-core-cli/src/plugins.rs`, and
switches them in `<config>/plugins.yaml`:

```yaml
plugins:
  engine-codex: { enabled: false }
  artifacts:
    settings: { editor: nvim }
```

A missing file enables everything; an unknown id is a usage error; the ids of retired plugins
(`hooks`, `history`, `menu`) are skipped so an old file keeps loading. A disabled plugin's verbs,
engines and resources are all absent. `tx plugins` lists each id with `active` or `disabled`.
Plugins keep per-agent data in the record's `ext.<id>` object through `TxCore::update_ext`; core
never reads it. Plugins cannot be loaded or reloaded at run time.

## Plugins act through `tx`, not through each other

The home plugin's design calls this rule D3: a plugin reads the core and tmux, but it acts only
through `tx` verbs and tmux commands. It does not write agent records it does not own, set tmux
options on other plugins' sessions, or call into another plugin's internals. When the home screen
stops an agent it runs `tx stop <id>`; when it opens the timeline it runs `tx _timeline-attach` in
a popup; when it needs the assistant it runs `tx assistant warm`. The one sanctioned shortcut is
`TxCore::verb(name)`, which runs another plugin's verb in-process by name, the same contract as
spawning `tx`; `tx ui attach` uses it to hand the home pane to the `home` plugin when it is
active.

The payoff is that any plugin can be disabled without breaking the others: a missing verb is a
missing feature, not a link error. Library-level reuse (the transcript parser in `tx-chat`) is
fine; reaching into another plugin's data files is not.

## `tx` and `txi`

Both binaries share `tx-core-cli`'s library: argument parsing, the plugin switchboard and opening
the core. `tx` is what people type. `txi` is what machines run: nothing in it is listed in
`tx help`, and its one verb today is `txi hook <Event>`. When an engine adapter launches an agent
it wires the engine CLI's own hook mechanism to run `txi hook <Event>` (Claude through
`--settings`, Codex through `-c hooks.<Event>=…`). `txi` opens the same core from the `TX_*_DIR`
variables every session carries and hands the payload to `TxCore::hook`, which is how agent state
moves (see [Agents and records](agents-and-records.md)). A hook must never fail the agent: anything that is
not tx's (a call outside a session, an unknown id) exits 0.
