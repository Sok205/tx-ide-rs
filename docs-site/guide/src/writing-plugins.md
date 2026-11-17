# Writing plugins

A plugin is a directory: `<id>/plugin.yaml`, the manifest, plus the files it names. Its verbs are
programs under `bin/`, written in anything. Put the directory into a plugin root and `tx <verb>`
runs it; nothing is compiled into `tx` and nothing is registered anywhere else.

```text
<root>/<id>/
  plugin.yaml                 the manifest
  bin/<program>               what its verbs run; helper scripts
  keys/  conf/  nvim/  agents/  definitions/    files it publishes (resources)
  …                           anything else is the plugin's own, read through $TX_PLUGIN_DIR
```

**Roots**, in order: `<data>/shipped/` (the plugins tx ships; `tx update` writes it and may
replace it freely) and then `<config>/plugins/` (yours; tx never writes there). `--plugin-dir DIR`,
repeatable, or `TX_PLUGIN_PATH` (colon-separated) replaces both. An entry of a root that is not a
directory holding a `plugin.yaml`, or whose name starts with `.`, is ignored.

**Plugin order** is root order, then id. It is the order `tx plugins` lists, and it decides who
keeps a verb two plugins declare: the first.

A plugin is **active** when it is installed, its manifest is valid for this tx, and
`config.yaml` does not say `plugins.<id>.enabled: false`. Only an active plugin has verbs. Its own
data lives in `<data>/plugins/<id>/`, never in its directory.

## An example

A plugin `hello`, in `~/.config/tx-ide/plugins/hello/`. The manifest, `plugin.yaml`:

```yaml
api: 1
id: hello
description: Says hello.
verbs:
  - name: hello
    usage: "hello [WORDS]   print the words and where I run"
    bin: hello
```

The program, `bin/hello`, made executable (`chmod +x`):

```sh
#!/bin/sh
echo "args: $*"; echo "id: $TX_PLUGIN_ID"; echo "settings: $TX_PLUGIN_SETTINGS"
echo "data: $TX_DATA_DIR"; echo "tx: $TX_BIN"
[ "$1" = fail ] && exit 7; [ "$1" = cat ] && cat; exit 0
```

With `plugins: { hello: { settings: { greeting: hi } } }` in `config.yaml`:

```console
$ tx hello a b
args: a b
id: hello
settings: {"greeting":"hi"}
data: /Users/you/.local/share/tx-ide
tx: /Users/you/.cargo/bin/tx
$ tx hello fail; echo $?
7
$ echo piped | tx hello cat | tail -1
piped
$ tx help | grep hello
  hello [WORDS]   print the words and where I run
$ tx plugins | grep hello
hello	active
```

To act on tx, the program runs `"$TX_BIN" …`: `"$TX_BIN" ls --json`, `"$TX_BIN" send …`. That is
the whole API for a plugin in another language: a manifest, the arguments, the variables below,
and `tx` itself.

## Running a verb

`tx <verb> <arguments…>` with a verb that is not built in:

1. `tx` looks the verb up among the verbs of the active plugins. Unknown:
   `tx: unknown verb "x"`, the usage, exit 2.
2. `tx` replaces itself (`execve`) with `<plugin>/bin/<bin>`, with the manifest's `args` and then
   the caller's arguments. Standard input, output and error, the terminal, the working
   directory, signals and the exit code are therefore the program's own: a full-screen program
   or a popup works unchanged. A program that cannot be started is
   `tx: <verb>: cannot run <path>: <error>`, exit 1.

The program inherits the caller's environment, with these on top:

| variable | value |
|---|---|
| `TX_BIN` | absolute path of the `tx` that ran it; the only way a plugin runs tx again |
| `TX_APPNAME`, `TX_CONFIG_DIR`, `TX_DATA_DIR`, `TX_STATE_DIR`, `TX_CACHE_DIR`, `TX_RUN_DIR`, `TX_TMUX_SOCKET` | the core `tx` resolved, absolute, whatever flags or variables named it |
| `TX_PLUGIN_PATH`, `TX_PROFILE_PATH`, `TX_SKILL_PATH` | colon-separated; set only when `--plugin-dir`, `--profile-dir`, `--skill-dir` (or the variable itself) replaced the defaults |
| `TX_CONFIG_FILE` | set only when `--config FILE` (or the variable) names another file |
| `TX_SESSION_ENV` | a JSON object; set only when `--env KEY=VALUE` was given |
| `TX_PLUGIN_API` | `1` |
| `TX_PLUGIN_ID`, `TX_PLUGIN_DIR`, `TX_PLUGIN_DATA_DIR` | the plugin, its directory, `<data>/plugins/<id>` (not created) |
| `TX_PLUGIN_SETTINGS` | `plugins.<id>.settings` of `config.yaml` as JSON; `null` without one |

Every global flag of `tx` has such a variable, read when the flag is absent (flag, then variable,
then default). So `"$TX_BIN" ls` run by a plugin opens exactly the core its parent opened, and so
does a plugin program that opens the core itself. Every session core launches carries `TX_BIN`
too (and `TX_PLUGIN_PATH` when the roots are named), next to the directories.

One plugin runs another's verb the same way, or, from Rust, through `TxCore::verb(name)`, which
starts that plugin's program with standard input closed and hands back its output, error and
exit code.

When `TX_PLUGIN_TRACE` names a file, whoever starts a plugin program first appends one line
`<role>\t<plugin id>\t<program>` to it (the role of a verb is `verb`), and the sessions a traced
command launches are traced too. It is how the number of processes behind a command is counted.

## The manifest: `plugin.yaml`, api 1

Every key not listed here is an error. A manifest written for an `api` this tx does not support
is refused with that one message, whatever else it contains.

```yaml
api: 1                       # required. The plugin API the manifest is written for.
id: kill                     # required. [a-z0-9-]+, equal to the directory name, not `tx-core`.
description: Stop the agent or close the view in the active pane, after a y/n prompt.   # required, one line
tier: core                   # core | ide | extra (default extra): what a preset selects
requires: [ide]              # plugin ids it is of no use without
needs: [fzf]                 # programs it runs, looked up on PATH by `tx check`
settings: { editor: nvim }   # its default settings, spelled out: used only to seed config.yaml

verbs:
  - name: kill               # [a-z0-9_-]+; a leading _ marks it internal
    usage: "kill <name>   stop a session, or close a view (prefix+X)"   # one line, shown by tx help
    bin: tx-kill             # a file name under bin/
    args: [kill]             # optional: words put before the caller's arguments

engine:                      # at most one
  kind: claude               # [a-z0-9-]{1,31}: what --engine, config.yaml and records name
  ext_key: claude            # the ext key the engine owns; default: the kind
  bin: tx-engine-claude
  args: [--tx-engine]
  hook: { bin: tx-engine-claude, args: [--tx-hook] }   # optional: its own hook entry
  methods: [prepare, archive, restore]   # the optional methods it implements
  interrupt: { key: Escape, state: idle }               # optional: a fixed answer to `interrupt`
  notes: ["claude adds to every agent's tools: Skill, ToolSearch"]

on-event:                    # at most one
  events: [state-changed, message-sent]   # event type names, or `all`
  bin: notify
  args: []
  timeout: 5                 # seconds, 1-30, default 5

update:                      # at most one
  bin: tx-ide
  args: [--tx-update]

resources:                   # files it publishes; the name is the path inside the plugin
  - keys/kill.tmux
  - bin/tmux-kill-tx-session
```

This version reads and checks every section, and acts on `verbs`. The `engine`, `on-event`,
`update` and `resources` sections are checked and not used yet: until they are, an engine, a
sink, an updater or a resource comes from a plugin compiled into `tx`. `tier`, `requires`,
`needs` and `settings` are for the installer and `tx check`; nothing is refused at run time
because of them.

Rules checked when the manifest is read:

- `bin` is a plain file name; the program is `<plugin>/bin/<bin>`. Whether it exists is found
  when it is run.
- A resource name is `<kind>/<path>` with `kind` one of `keys`, `conf`, `bin`, `nvim`, `agents`,
  `definitions`; `[A-Za-z0-9_./-]+`, no leading `/`, no `..` segment; a key fragment is
  `keys/<name>.tmux`.
- `engine.methods` is a subset of `prepare`, `receive_message`, `interrupt`, `archive`, `restore`,
  `warnings`, `check`, `tools`. `interrupt:` and the `interrupt` method exclude each other; its
  `key` is `Escape`, `Enter`, `C-c` or `C-d`.
- `on-event.events` names are the event types: `created`, `forked`, `respawned`, `renamed`,
  `tagged`, `message-sent`, `state-changed`, `engine-data-changed`, `exited`, `stopped`,
  `archive-failed`.

A manifest that breaks one makes the plugin an `error` row of `tx plugins`, with the reason (and
the line, where the YAML parser gives one); nothing of the plugin is loaded:

```console
$ tx plugins | grep hello
hello	error	api 2 is not supported (this tx: 1)
```

So does a second directory of an id that an earlier root already holds:
`also installed in <the first directory>`. The first one runs.

Two things are decided when the manifests meet `config.yaml`, because they depend on who is
enabled, and they cost a plugin only the verb, not its place:

- The built-in verbs are reserved: `help`, `engines`, `plugins`, `create`, `fork`, `respawn`,
  `ls`, `show`, `rename`, `tag`, `send`, `where`, `attach`, `stop`, `interrupt`, `gc`, `update`,
  `check`, `definitions`. A plugin's verb of one of these names is dropped:
  `verb "create" is a built-in`.
- A verb an earlier plugin in plugin order already has is dropped:
  `verb "hello" is already "hello"'s`.

The plugin stays `active`, and `tx plugins` shows what it lost next to its status.

## The index

Reading every manifest on every run would cost more than the rest of `tx`, so the scan of the
roots is kept in `<cache>/plugins-index.json`. It is used as long as it was written by this
version of tx for the same roots, and every root, every `plugin.yaml` and every directory of a
root that had none still has the modification time and size it had. Otherwise the roots are
scanned and the file is written again. You never refresh it: adding or removing a plugin
directory, or editing a manifest, shows on the next run. A cache directory that cannot be
written means a scan per run, not an error; `tx help` and a command refused as misuse read the
index and leave the file as it is.

## A plugin in Rust: the SDK

A plugin crate keeps the in-process API: its `impl Plugin` with `register`, its `Verb`s
(`run(core, io, argv)`) and its `Updater`. With `tx-plugin` as a dependency it gains a program:

```rust,ignore
fn main() -> std::process::ExitCode {
    tx_plugin::sdk::main(vec![Box::new(tx_mockup::MockupPlugin)])
}
```

and a manifest whose verbs name that program, each with its own name as the first argument:

```yaml
verbs:
  - name: mockup
    usage: "mockup start --project DIR [--for S] | show [--for S] | wait [--for S] | stop [--for S]   ratatui mockups for brainstorming"
    bin: tx-mockup
    args: [mockup]
```

`tx_plugin::sdk::main(hosted)` reads `[global flags] <first> [rest…]`:

| first | does |
|---|---|
| a verb one of the hosted plugins registers | resolves the core as `tx` would (flags, variables, defaults), opens it, runs the verb with the process's standard streams, exits with its code |
| `--tx-update` | opens the core and runs the registered updaters; prints `tx-update: respawn-agents` when one asks. With `TX_PLUGIN_RETIRE=1` it runs `Updater::retire` instead, whether or not the plugin is enabled |
| `--tx-probe` | exits 0 at once |
| anything else | `tx-mockup: not a verb of this plugin: x`, exit 2 |

- A program may host several plugins. Each is registered only when it is active in this
  installation, with the settings `config.yaml` gives its id.
- The core such a program opens holds the hosted plugins in-process and a proxy for every other
  active plugin's verb, so `core.verb("layout")` answers in a plugin as it does in `tx`.
- Because the program resolves the core itself, it can be run directly for its own verbs,
  without `tx` in front: `tx-mockup --data-dir … mockup show`.
- `tx_plugin::sdk::tx_bin()` is the `tx` that started the program. Command lines the plugin
  builds for tmux (keys, popups, views), for another plugin's verb or for a built-in start with
  it; `std::env::current_exe()` is right only for the plugin's own verbs.
- What the program is asked wrongly it reports under its own name, as above. What goes wrong in
  tx (the core, `config.yaml`, a verb that fails) reads `tx: <error>`, with tx's exit codes: a
  verb prints the same whichever way it runs.

The plugin's own tests stay in-process (`core.plug(vec![Box::new(XPlugin)], …)`), and one more
keeps the manifest honest:

```rust,ignore
#[test]
fn the_manifest_matches_the_plugin() {
    tx_plugin::testing::check_bundle(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        vec![Box::new(tx_mockup::MockupPlugin)],
    );
}
```

`check_bundle` fails unless the manifest reads, every resource is a file of the plugin, every
`bin` is a file under `bin/` or a `[[bin]]` of the package, each verb's `args` start with its
name, and the verbs, their usages, the engine kind and the presence of an updater are what
`register` produces. `check_manifest` is the first three alone, for a plugin whose program is not
built on the SDK or that has none.

Until every plugin has a manifest, a plugin of this repository is also listed in `MANIFEST`
(`tx-core-cli/src/plugins.rs`) and runs compiled into `tx` wherever its bundle is not installed;
see [Architecture](architecture.md).
