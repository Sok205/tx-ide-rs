# Files and directories

tx keeps its files in five directories, laid out the way nvim uses the XDG Base Directory
specification (on macOS too), under the application name `tx-ide`. The library takes them as a
[`Directories`](../internal/tx_core/config/struct.Directories.html) value inside its
[`CoreConfig`](../internal/tx_core/config/struct.CoreConfig.html) and never looks them up itself;
`tx` and `txi` resolve them.

## Resolution

Each directory resolves to the first of: its flag, its `TX_*_DIR` variable, the XDG variable plus
the application name, and the XDG default.

| directory | flag | variable | default |
|---|---|---|---|
| config | `--config-dir` | `TX_CONFIG_DIR` | `~/.config/tx-ide` |
| data | `--data-dir` | `TX_DATA_DIR` | `~/.local/share/tx-ide` |
| state | `--state-dir` | `TX_STATE_DIR` | `~/.local/state/tx-ide` |
| cache | `--cache-dir` | `TX_CACHE_DIR` | `~/.cache/tx-ide` |
| run | `--run-dir` | `TX_RUN_DIR` | `$XDG_RUNTIME_DIR/tx-ide`, else `$TMPDIR/tx-ide.$USER` |

`TX_APPNAME` renames all five at once, the way `NVIM_APPNAME` does; it must match
`[A-Za-z0-9_.-]+`. Setting `TX_APPNAME=tx-dev` gives a completely separate installation (its own
records, server and socket) next to your everyday one.

Core exports all five `TX_*_DIR` variables and `TX_APPNAME` into every session it launches,
alongside `TX_SESSION_ID` and `TX_TMUX_SOCKET`. tmux does not pass the caller's `XDG_*` variables
on, so this is how a `tx` run by an agent, or a `txi hook` fired inside its session, opens the
same core as the `tx` that launched it.

The other global flags have a variable too, read when the flag is absent (flag, then variable,
then default):

| flag | variable | default |
|---|---|---|
| `--plugin-dir`, repeatable | `TX_PLUGIN_PATH`, colon-separated | `<data>/shipped`, then `<config>/plugins` |
| `--profile-dir`, repeatable | `TX_PROFILE_PATH`, colon-separated | `<data>/roles`, then `<config>/roles` |
| `--skill-dir`, repeatable | `TX_SKILL_PATH`, colon-separated | `<data>/definitions/skills`, then `<config>/definitions/skills` |
| `--config` | `TX_CONFIG_FILE` | `<config>/config.yaml` |
| `--env KEY=VALUE`, repeatable | `TX_SESSION_ENV`, a JSON object of strings | none |

A plugin program is started with the ones that replace a default, and with the directories, so it
resolves the core its `tx` resolved (see [Writing plugins](writing-plugins.md)). Two more
variables are not flags: `TX_BIN`, the absolute path of `tx`, which every session and every plugin
program carries so that a plugin can run tx again; and `TX_PLUGIN_TRACE`, a file that gets one
line for every plugin program started.

## What lives where

**config** holds what you write by hand: `config.yaml` (the defaults for `tx create` in its
`agent:` block, the build output `tx gc` may delete in `worktree:`, and which plugins are on,
with their settings, in `plugins:`; `tx setup` writes it once, when there is none, and nothing
writes it after that), `roles/` (your own
roles, never pruned, overriding a shipped role of the same name), `definitions/` (your own
`agents/` and `skills/`, each overriding a shipped one of the same name), `layouts/` (one `<name>.yaml` per
saved pane layout, opened by `tx layout open <name>`; `tx layout path <name>` prints the file,
and `tx layout save <name>` writes one from a view), `presets/` (your own presets, one
`<name>.yaml` each: see "Presets and the selection" below) and `plugins/` (your own plugins, one
directory `<id>/` with a `plugin.yaml` each: the second plugin root, which tx reads and never
writes).

**data** holds everything durable tx writes:

```text
agents/<id>/agent.json      the agent's record
agents/<id>/archive/        the engine's archived transcripts
log.jsonl                   one {"ts","actor","type","msg"} line per mutation
worktrees/<repo-key>/…      the agents' git worktrees; `tx gc` empties their build output
plugins/<id>/               each plugin's own data (artifacts, home, assistant, tutor, …)
shipped/<id>/               the plugins tx ships, each a directory with a `plugin.yaml`: the
                            first plugin root, written by `tx update --bundles`
shipped/.selection          which plugins are laid out there, written by `tx setup` alone
shipped/.stamp              the tx and the build they were laid out from
roles/ definitions/         core's roles, and its agent definitions, skills and MCP servers
bin/ keys/ conf/ nvim/      helper scripts, key fragments, the theme, the nvim runtime
                            (these six: the published resources, written by `tx update`)
tmux.conf                   the server's config, composed by the `ide` updater on `tx update`
update-version              the last update step run, written by `tx update`
```

**The shipped plugins.** `tx update --bundles DIR --binaries DIR [--link]` lays them out from a
checkout: `DIR` holds one `<id>/plugin.yaml` per plugin (the checkout's `plugins/`), and
`--binaries` is where the built programs are (`target/release`). For each plugin it installs the
manifest, every resource the manifest lists and every program a `bin` names: the bundle's own
`bin/<name>` when that file is there (a script), else `<binaries>/<name>`. A program found in
neither stops the update before anything is written, and names the package to build. The plugins
are the ids under `plugins` in `.selection` (`{"preset": "full", "plugins": ["mockup", …]}`,
`"preset": null` for an explicit list); without that file, every bundle of `DIR`. `tx update`
only reads it. The `install` script runs both for you, `tx setup` to choose and then this
`tx update`: see [Install](index.md#install).

**Presets and the selection.** `tx setup --bundles DIR [--preset NAME | --plugins a,b,c |
--upgrade] [--list]` chooses among the bundles of `DIR` that read (one that does not is named and
left out) and is the only writer of `.selection`. A preset is a set of tiers and ids. Three
ship: `headless` (the plugins of tier `core`), `ide` (`core` and `ide`) and `full` (all three
tiers). Yours are `<config>/presets/<name>.yaml`, and one with a shipped name replaces it:

```yaml
description: The IDE without the timeline and the graph
tiers: [core, ide]
plugins: [tour]          # added
exclude: [timeline, graph]
```

Every key is optional. A file that does not read (an unknown key, say) is no preset: it is named
on stderr and left out, unless the run uses its name (`--preset <name>`, or `full` when nothing
else is asked), which then fails with the file and its error, even for a shipped name: it never
turns into the shipped preset. A preset selects exactly its tiers and `plugins` without its
`exclude`; an id under `plugins` that has no bundle in `DIR` is an error. `--plugins a,b,c` selects by id and adds, transitively, what
each plugin `requires` when that has a bundle, saying `tx setup: <a> requires <b>: added` on
stderr; a requirement without a bundle is only named. `--list` prints
`<name><TAB><ids separated by spaces>` per preset, the shipped ones first, and writes nothing.

With none of these flags, and stdin and stderr both terminals, it shows a checklist on stderr:
one row per bundle (a tick box, the id, the tier, the description, and `needs <program> (not
found)` for each program of its `needs` that is not on `PATH`) and a last row offering to link
`~/.config/nvim` and `~/.tmux.conf` to the checkout, unticked. It starts with `full` ticked.
`j`/`k` or the arrows move, `g` and `G` jump to the first and last row, space toggles a row
(ticking a plugin ticks what it requires, unticking one unticks what requires it), `1`, `2` and
`3` tick exactly `headless`, `ide` and `full`, Enter accepts, and `q` or Escape leaves without
writing anything (exit 1). The header names the preset the ticks equal, or `custom`. With no
plugin ticked Enter does not accept (the header says `nothing is selected`; the dotfiles row is
no plugin). Without a terminal the selection is `full`. A run never records an empty selection:
`--plugins` without an id is a usage error (exit 2), and a preset that selects no plugin of `DIR`
fails (exit 1).

The result is recorded as `{"preset": <name>, "plugins": [<the resolved ids, sorted>]}`, with
`"preset": null` for `--plugins` and for ticks that are no preset's exact set; of two presets
that hold the same plugins the checklist records the one last picked with `1`, `2` or `3`. It is
also printed, for the installer: `plugin<TAB><id>` per id, then `option<TAB>link-dotfiles<TAB>true`
when that row was ticked. And when `<config>/config.yaml` does not exist it is written: the
`agent:` and `worktree:` defaults of the repository's `config/tx-ide/config.yaml`, then
`plugins:` with one entry per selected plugin, `<id>: { enabled: true }`, or with the `settings`
of its manifest spelled out. A file that is there is never opened for writing.

`--upgrade` asks nothing and never writes `config.yaml`. It resolves the recorded preset again
against `DIR`, so a plugin new in a later version arrives with `full`; a recorded list
(`"preset": null`) is kept, without the ids that have no bundle any more, each named on stderr;
an install that recorded nothing becomes `full`. A recorded preset whose file is gone does not
stop it: its plugins are kept as a list and `"preset": null` is recorded, and so is one whose
file does not read (named on stderr). Only an install that recorded nothing stops at a `full.yaml`
that does not read, as every run that implies `full` does.

Each file goes in through a temporary file and a rename, so a program that is running keeps the
file it started from, and a file that did not change is left alone. A file a plugin no longer
names is removed; so is the directory of a plugin that is no longer selected, after the plugin
was retired (below). With `--link` the files are symbolic links to their sources, for a checkout
you develop in: a rebuild is live at once. Without it, every program just copied is run once with
`--tx-probe`, all at the same time: on macOS the first run of a new program file is slow, and this
pays for it during the update instead of on the first key; a shipped program answers
`--tx-probe` by exiting 0. Last, `.stamp` records `{"tx_version", "api", "bundles", "binaries",
"linked"}`: the shipped plugins link the same core as `tx`, so they must come from its build.

**The published resources.** On every run `tx update` writes the six directories `bin`, `keys`,
`conf`, `nvim`, `roles` and `definitions` itself, so an install without the `ide` plugin has its
roles and definitions too. The sources, in order: core's own roles and definitions, then every
active plugin's resources in plugin order (a plugin's `agents/…` lands under `roles/`). A later
source replaces an earlier one of the same name, so a plugin of your own can replace a shipped
role. `bin/` and `keys/` files are executable. A file in those directories that no source
accounts for is removed (only `keys/.installed`, the `ide` updater's record, stays), so keep
nothing of your own there; a symbolic link is removed itself and never followed, and when one of
the six directories is itself a symbolic link `tx update` refuses, publishes nothing and runs no
updater.

**Updaters and retiring.** After that, each active plugin's updater runs: for an installed
plugin, its `update` program, whose lines are shown as `<id>: <line>`; the line
`tx-update: respawn-agents` asks for the respawn message instead; a program that does not exit 0
is named (`tx: update: <id>: exit <n>`), the others still run, and `tx update` exits 1. What an
updater generated would outlive its plugin, so the same program is run with
`TX_PLUGIN_RETIRE=1` to take it back: for an installed plugin that is disabled, on every
`tx update`, and for a shipped plugin that left the selection, before its directory is removed.
A plugin you delete by hand gets no retire: disable it and run `tx update` first.

`update-version` says which update steps have run; absent reads 0. When a tx release
needs something done once per machine (files moved, a setting rewritten, leftovers removed),
it ships an update step; `tx update` runs each pending step in order and stamps its number after
each, so a failure leaves the stamp at the last success and a re-run continues from there. Steps
check before they act, so running one twice (a `config` shared across machines) does no harm. A
fresh install (a `data` that is empty apart from `shipped/` and the stamp, no subdirectory in
`config`) is stamped at the latest version without running any. Step 1, "plugins run as their own
programs", is the one an install of v0.1.0 runs after `git pull` and `install --upgrade`: it
changes no file and asks for a respawn, because agents started before the upgrade keep reporting
through `txi hook`, and a respawned agent uses its engine's own program. While the stamp is behind, every other verb prints
`tx: N update steps pending: run tx update` on stderr once and carries on; when a
step says so, `tx update` also asks you to respawn the running agents.

`log.jsonl` is a notification trail, not the record: each line is at most 512 bytes and written
with a single append, so `tail -f` never sees a torn line. On an event, re-read the agent with
`tx show`.

**state** holds `locks/` (`create.lock` and one `<id>.lock` per agent, taken with `flock`) and
`profiles/<id>.md`, the combined role text handed to each agent's engine.

**cache** holds `plugins-index.json`: what the last scan of the plugin roots found, kept so
that a run reads one file instead of every manifest. It is rebuilt whenever a root, a manifest or
the version of tx changed, and safe to delete.

**run** holds `tmux.sock`, tx's server. The directory is mode 0700, and the socket path must
stay at or under 100 bytes, within the Unix socket path limit.
