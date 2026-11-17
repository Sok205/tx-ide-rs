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
keeps a verb or an engine kind two plugins declare: the first.

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

Every program a `bin` names, a script included, is also started once with `--tx-probe` as its
only argument when `tx update` installs a copy of it, and must then exit 0 at once, having done
nothing. A program on the Rust SDK does so by itself; a script has to look at its first argument
(`[ "$1" = --tx-probe ] && exit 0`). A sink script is usually the first place this matters.

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

This version reads, checks and acts on every section: `verbs` (above), `engine` (see [An
engine](#an-engine)), `on-event` (see [An event sink](#an-event-sink-on-event)), and `update` and
`resources`, which `tx update` runs and publishes (see [Files](files.md)). `tier`, `requires`,
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

### Checking a plugin

`tx check --plugins [ID…]` checks the installed plugins (all, or those named) and nothing else;
plain `tx check` checks the definitions and then the plugins. It opens no core, starts no
plugin program and writes nothing, not even the index. One line per finding, `path: message` with
the path of the plugin's `plugin.yaml` unless said otherwise, in plugin order; the exit code is 1
when any problem was printed. A clean run ends `ok: <n> plugins` (after the definitions' counts,
for plain `tx check`).

Problems:

- a manifest that does not read: `path:line: <the parser's message>` (an unknown key, a bad
  `tier`), `path: <the rule broken>` (3.4's rules, among them an `id` that is not the directory's
  name), and `path: api 2 is not supported (this tx: 1)`;
- `<directory>: plugin "<id>" is also installed in <the first directory>`;
- a verb or engine that is lost to a clash, in the words `tx plugins` shows: `verb "<name>" is a
  built-in`, `verb "<name>" is already "<id>"'s`, `engine kind "<kind>" is already "<id>"'s`,
  `ext key "<key>" is already "<id>"'s`; the later plugin in plugin order gets the line;
- `bin/<name> is missing` or `is not executable` (for every program the manifest names), and
  `resource <name> is missing`;
- `requires "<id>", which is not installed` (or `is disabled`, or `does not load`: that plugin has
  a line of its own). A plugin that `tx` still has compiled in counts as installed;
- in `config.yaml`: `agent.engine "<kind>" is not an active engine (active: …)`;
- `<data>/shipped: plugins are from tx <a>, this is tx <b>: run install --upgrade`, or `no
  shipped plugins are installed: run install` for an install that has data.

Warnings, printed and not counted:

- `warning: needs <program>, which is not on PATH`;
- in `config.yaml`, `warning: plugins.<id>: no such plugin`, with the reason for the three ids
  that no longer exist (`hooks`, `history`, `menu`);
- `warning: <resource> replaces "<id>"'s`: your plugin publishes a resource name that core
  (`tx-core`, for the roles and definitions it ships) or an earlier plugin already publishes. The
  later one wins, which is how you override a role; the warning only says you did.

## The index

Reading every manifest on every run would cost more than the rest of `tx`, so the scan of the
roots is kept in `<cache>/plugins-index.json`. It is used as long as it was written by this
version of tx for the same roots, and every root, every `plugin.yaml` and every directory of a
root that had none still has the modification time and size it had. Otherwise the roots are
scanned and the file is written again. You never refresh it: adding or removing a plugin
directory, or editing a manifest, shows on the next run. A cache directory that cannot be
written means a scan per run, not an error; `tx help` and a command refused as misuse read the
index and leave the file as it is.

## An engine

An engine turns a launch into a command line and a hook into a state. A plugin provides one by
naming a program in the `engine` section of its manifest; the program speaks the protocol below,
in any language. `kind` is the name that `--engine`, `agent.engine` in `config.yaml` and the
records use, `[a-z0-9-]{1,31}`; `ext_key` (by default the kind) is the key of a record's `ext`
that holds the engine's own data. `tx engines` lists the kinds of the active engines.

Like a verb, a kind belongs to the first plugin in plugin order that declares it, and so does an
`ext` key. A later plugin stays `active` without its engine, and `tx plugins` says why:
`engine kind "claude" is already "engine-claude"'s`, `ext key "claude" is already
"engine-claude"'s`.

### An example: the stand-in engine

The engine tx's own tests run on, `tx-core-cli/tests/fixtures/engine-fake/`. It stands for no
real agent: every launch is `sleep 600`, and hooks fired by hand move the agent. Its manifest:

```yaml
api: 1
id: engine-fake
description: A stand-in engine for tests. Every agent it launches is `sleep`, moved by hooks fired by hand.
engine:
  kind: fake
  bin: engine-fake
  methods: [archive]
  interrupt: { key: Escape, state: idle }
```

Its program, `bin/engine-fake`. It also notes each request in `calls` and its process id in
`pid`, under its data directory, for the tests to read:

```python
#!/usr/bin/env python3
import json
import os
import sys

DATA = os.environ["TX_PLUGIN_DATA_DIR"]
LAUNCH = {"program": "sleep", "args": ["600"]}
CHANGES = {
    "Stop": {"state": "idle", "idle_reason": "finished"},
    "UserPromptSubmit": {"state": "working"},
}


def answer(method, params):
    if method == "configure":
        # The complete new value of the record's `ext.fake`: what is there, then the options.
        current = params["session"].get("ext", {}).get("fake", {})
        return {"ok": {**current, **params["options"]}}
    if method in ("start", "resume", "fork"):
        return {"ok": LAUNCH}
    if method == "on_hook":
        return {"ok": CHANGES.get(params["hook"]["name"], {})}
    if method == "archive":
        return {"ok": None}
    return {"error": {"kind": "refused", "message": f"fake: no method {method}"}}


os.makedirs(DATA, exist_ok=True)
with open(os.path.join(DATA, "pid"), "w") as file:
    file.write(f"{os.getpid()}\n")
for line in sys.stdin:
    with open(os.path.join(DATA, "calls"), "a") as file:
        file.write(line)
    request = json.loads(line)
    print(json.dumps(answer(request["method"], request["params"])), flush=True)
```

Copied into `<config>/plugins/`:

```console
$ tx engines
claude
codex
fake
$ tx create a --cwd . --engine fake --prompt hi
3f6c8a52-…
$ tx show a | jq -r .state
starting
$ TX_SESSION_ID=3f6c8a52-… txi hook Stop </dev/null; tx show a | jq -r .state
idle
```

The last line is what an engine's CLI does through the hooks the engine wired; this one wires
none.

### The protocol

The program (`engine.bin` with `engine.args`) is started by the first call that needs it and
kept: one program per engine, per process that opened the core. It gets the environment of
[Running a verb](#running-a-verb). One request per line on its standard input, one response per
line on its standard output, in order; its standard error passes through. When the process that
started it is done, the program's input is closed; it has one second to exit, then it is killed.

```text
-> {"method": "start", "params": {"request": {…}, "prompt": "hi"}}
<- {"ok": {"program": "claude", "args": ["--session-id", "…"], "env": [], "workspace": [], "engine_data": {…}}}
<- {"error": {"kind": "refused", "message": "claude options: model must not be empty"}}
```

`error.kind` is `refused`, or `workspace` or `io`, which also name the `path` they are about. tx
prints the message as the error of the command.

| method | params | ok | when core calls it |
|---|---|---|---|
| `configure` | `session`, `options` | the new engine data | create, fork |
| `start` | `request`, `prompt` | a launch plan | create |
| `resume` | `request` | a launch plan | respawn |
| `fork` | `request`, `source`, `prompt` | a launch plan | fork |
| `on_hook` | `session`, `hook` | a change | every hook that reaches `txi hook` |
| `prepare`* | `session` | `null` | before every launch |
| `archive`* | `session`, `archive_directory` | `null` | after a hook, a stop, an exit |
| `restore`* | `session`, `archive_directory` | `true` or `false` | respawn |
| `receive_message`* | `sender`, `text` | an input | send |
| `interrupt`* | `session` | `null`, or `{"input", "state"}` | interrupt |

\* optional: asked only when `engine.methods` lists it. Otherwise tx answers itself, without
starting the program: nothing to prepare, archive or restore; the standard `<from-user>` /
`<from-agent>` envelope, submitted; `interrupt:` from the manifest, else "no way to interrupt".
(`methods` also takes `warnings`, `check` and `tools`; this version does not call them yet.)

The shapes:

- `session`, `source`: the agent record, as `tx show` prints it, `"version": 2` first. The
  engine's own data is its `ext.<ext_key>`.
- `options`: the launch options, a JSON object (`{"model": "fable", "effort": "high"}`).
  `configure` answers the complete new value of the engine's data, which tx stores before the
  launch reads it.
- `request`: `{"session": …, "profile": {"text", "file", "skill_dirs", "tools" (null: none
  given), "disallowed_tools", "mcp_servers", "provided_skills": [[name, provider]]},
  "hook_command": […]}`. `hook_command` is the argv prefix of every hook the engine wires, with
  the CLI's event name appended; empty: wire none.
- launch plan: `{"program", "args", "env": [[key, value]], "workspace": [{"write": {"path",
  "content"}} | {"symlink": {"src", "dst"}}], "engine_data": object | null}`. Only `program` is
  required. `engine_data`, when given, replaces the engine's data in the record before the
  process starts.
- `hook`: `{"name", "payload", "at"}`: the CLI's own event name, its payload (always an object),
  the time.
- change: `{"state": "starting" | "idle" | "working" | "ended" | null, "idle_reason": "finished"
  | "waiting" | null, "engine_data": object | null}`; `{}` changes nothing.
- input: `{"text": {"text", "submit"}}` or `{"key": "Escape" | "Enter" | "C-c" | "C-d"}`.
- `sender`: `{"user": {"via": string | null}}` or `{"agent": {"name"}}`.

A reader ignores a key it does not know.

Two keys of the engine's data are reserved, `conversation_id` and `transcript_path`: an engine
that records a conversation writes them, and tx reads the conversation from the record, never
from the engine.

A program that cannot be started, that ends without answering, or whose answer is not such a
line, fails the command as `engine <kind>: <what happened>`; `tx create` then leaves no record,
no worktree and no session. That program is discarded and a later call starts a new one.
`on_hook` cannot fail: it then changes nothing and says why in one line on standard error.

### Hooks

An engine's CLI reports through the hooks the engine wires at launch: each runs `hook_command`
with the event's name appended, and gets the payload on standard input.

- An engine whose manifest has `hook:` is sent `[<plugin>/bin/<hook.bin>, hook.args…]` as its
  `hook_command`: its own program is the hook entry. On the Rust SDK that is `--tx-hook`: one
  process per hook, with the engine in it.
- An engine without `hook:` is sent `txi hook`. `txi` opens the core, starts the engine's
  program, and asks it `on_hook`, then `archive`.

Either way a hook must never fail the agent: one that is not tx's (no `$TX_SESSION_ID`, an
agent tx does not know) exits 0, and a failure exits 1, never 2.

### What starts the program

`tx create`, `tx fork` and `tx respawn` start it once each, and `txi hook` once per hook.
`tx stop` starts it for an engine that lists `archive`, `tx send` and `tx interrupt` for one that
lists `receive_message` or `interrupt`. `tx ls`, `tx show`, `tx where`, `tx rename`, `tx tag`
and `tx engines` never do, unless `ls` or `show` finds an agent exited and has it archived. With
`TX_PLUGIN_TRACE` each start is a line `engine\t<plugin id>\t<program>`; an engine's own hook
entry writes `hook\t<plugin id>\t<program>`.

## An event sink: `on-event`

A plugin that wants to know what happens to the agents names a program under `on-event`. It is
run **once per event** of a type it lists (`events: all` for every one), by the process that
emitted the event (`tx`, `txi`, a plugin program), after the record is saved, every lock released
and the line of `<data>/log.jsonl` written. The program gets the environment of [Running a
verb](#running-a-verb), and on standard input one JSON line, then end of input:

```json
{"ts":1794950000.123,"type":"renamed","id":"…","name":"a1","actor":"","detail":"a"}
```

| field | value |
|---|---|
| `ts` | seconds since the epoch, a float |
| `type` | one of the event types of the manifest's rules |
| `id`, `name` | the agent the event is about, its name after the event |
| `actor` | the sending agent's id for `message-sent`; `""` for the user or core |
| `detail` | what happened, whole: the old name, the tags, the state change `idle → working`, … It is for people, not a contract. `log.jsonl` cuts a line at 512 bytes; the sink's line is never cut |

- **Output.** Standard output is discarded; standard error is the emitting process's, so a line
  a sink prints there is seen where the command ran.
- **Order.** The emitting process waits for the program before it runs the next sink or emits
  the next event: one process's events reach a sink in the order it emitted them, and the sinks
  run in plugin order. Events of different processes are not ordered against each other; `ts`
  and `log.jsonl` are what orders them.
- **Timeout.** `timeout` (1 to 30 seconds, default 5, under the 10 seconds a Claude hook may take)
  bounds the wait. The program is started in a process group of its own; past the timeout the
  whole group is killed and the command prints
  `tx: plugin <id>: on-event timed out after <n>s`. A program that ends in time is left alone,
  whatever it started.
- **Never a failure.** The exit code is ignored. A program that cannot be started is
  `tx: plugin <id>: on-event cannot run <path>: <error>`, and the command still succeeds.
- **At most once.** A process killed between the log line and the sink has logged the event and
  not delivered it. A consumer that must miss nothing, or that runs for long, follows
  `log.jsonl` from an offset of its own instead.
- **Cost.** Nothing while no active plugin declares `on-event`; otherwise one process per event
  per sink (about 1 ms for a compiled program, 3.5 ms for a shell script). A sink that runs
  `"$TX_BIN"` to act causes events of its own, which reach it too.

With `TX_PLUGIN_TRACE` each start is a line `sink\t<plugin id>\t<program>`. An
example, a log of every event:

```yaml
api: 1
id: log-events
description: Appends every event to a file.
on-event:
  events: all
  bin: sink
```

```sh
#!/bin/sh
[ "$1" = --tx-probe ] && exit 0
mkdir -p "$TX_PLUGIN_DATA_DIR"
cat >> "$TX_PLUGIN_DATA_DIR/events.jsonl"
```

On the Rust SDK a plugin keeps its in-process sink (`registry.sink(Box::new(…))`) and names its
program with `args: [--tx-event]`: the program reads the line and hands the event to the sinks
the hosted plugins register.

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
| `--tx-engine` | serves the engine a hosted plugin registers, on standard input and output (the protocol of [An engine](#an-engine)); opens no core |
| `--tx-hook <Event>` | the hook entry of the hosted engine: what `txi hook` does, with the engine in the process; exits 0 or 1, never 2 |
| `--tx-update` | opens the core and runs the registered updaters; prints `tx-update: respawn-agents` when one asks. With `TX_PLUGIN_RETIRE=1` it runs `Updater::retire` instead, whether or not the plugin is enabled |
| `--tx-event` | reads one event, the line of [An event sink](#an-event-sink-on-event), on standard input and hands it to every sink the hosted plugins register; opens no core. A line that is not an event: `tx-mockup: --tx-event: <error>`, exit 2 |
| `--tx-probe` | exits 0 at once |
| anything else | `tx-mockup: not a verb of this plugin: x`, exit 2 |

- A program may host several plugins. Each is registered only when it is active in this
  installation, with the settings `config.yaml` gives its id.
- The core such a program opens holds the hosted plugins in-process, a proxy for every other
  active plugin's verb and engine, and a sink for every other active plugin's `on-event`, so `core.verb("layout")` and `core.create_agent(…)` answer in
  a plugin as they do in `tx`.
- An engine written in Rust keeps its `LlmEngine`, registered by its plugin
  (`registry.engine(Box::new(…))`). Its manifest names the program twice, as the protocol server
  and as its own hook entry, so that a hook is one process with the engine in it:

  ```yaml
  engine:
    kind: claude
    bin: tx-engine-claude
    args: [--tx-engine]
    hook: { bin: tx-engine-claude, args: [--tx-hook] }
    methods: [prepare, archive, restore]
    interrupt: { key: Escape, state: idle }
  ```
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
