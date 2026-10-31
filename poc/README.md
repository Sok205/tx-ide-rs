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
| `plugins/engine-claude` (`tx-engine-claude`) | plugin: the `LlmEngine` for Claude Code | tx-core |
| `plugins/engine-codex` (`tx-engine-codex`) | plugin: the `LlmEngine` for Codex | tx-core |
| `plugins/labels` (`tx-labels`) | plugin: session labels (prefix+s, pane-border labels), `whoami` | tx-core, tx-client |
| `plugins/kill` (`tx-kill`) | plugin: `txc kill`, prefix+X | tx-core, tx-client |
| `plugins/picker` (`tx-picker`) | plugin: the fzf session picker `txc pick`, prefix+t | tx-core, tx-client |
| `plugins/chat` (`tx-chat`, plugin `chat`) | plugin: `resume`, `chat ls\|hooks`, prefix+f (through core's fork); captures chat ids through its own agent hooks | tx-core, tx-client, tx-engine-claude, tx-engine-codex |
| `plugins/chat` (`tx-chat`, plugin `history`) | plugin: the transcript mirror under `plugins/history/`, `history`, `history-hooks`; same crate as `chat` | tx-core, tx-client |
| `plugins/edit` (`tx-edit`) | plugin: the session and tag editors, prefix+e | tx-core, tx-client |
| `plugins/spawn` (`tx-spawn`) | plugin: the new-agent form `txc _new-agent`, prefix+n; ships the COMMON and DEVELOPER roles and their skills | tx-core, tx-client |
| `plugins/assistant` (`tx-assistant`) | plugin: `txc assistant warm`, the `tx-assistant` session `ui attach` lands in; ships the TX-ASSISTANT role | tx-core, tx-client |
| `plugins/mockup` (`tx-mockup`) | plugin: `txc mockup start\|show\|wait\|stop`, ratatui mockups rendered in a per-agent `mockup-<agent>` view the operator is switched to and back; ships the tx-mockup skill | tx-core, tx-client, ratatui |
| `plugins/tutor` (`tx-tutor`) | plugin: `txc tutor start\|check\|next\|prev\|goto\|status\|hint\|reset`, the vimtutor-style walkthrough in a `tutor-<lang>` view; ships the repo's `tutor/` lesson packs | tx-core, tx-client, toml, libc |
| `plugins/artifacts` (`tx-artifacts`) | plugin: `txc artifact create\|modify\|group\|ls\|show\|diff\|open\|doctor` (durable versioned deliverables in `<data>/plugins/artifacts/`), `txc group` (effort groups), prefix+a | tx-core, tx-client |
| `plugins/timeline` (`tx-timeline`) | plugin: `txc timeline` (swimlanes of session activity, `--snapshot`, `health`), the resident popup behind prefix+T, `_timeline-json` (main's `/api/timeline` payload) | tx-core, tx-client, tx-chat (message parser), ratatui |
| `plugins/menu` (`tx-menu`) | plugin: the tx-ide start menu `txc menu` (prefix+m, and once per server on the first attach): the shimmering `TX` title screen, every action one key away, the recent past sessions | tx-core, tx-client, ratatui, tempfile |
| `tx-client` | library: the socket-bound `tmux::Ui` helper, the `views` store (the non-LLM tmux sessions the client owns, kept in tmux session options), the row renderer (`render`), the modal editor (`editor`) and the fzf one-line prompt (`prompt`) shared by the key plugins; also plugin `ide`: `txc ui install\|attach\|view`; composes the server's `tmux.conf` from plugin resources; the theme, navigation keys and helper scripts | tx-core |
| `tx-core-cli` | `txc`: a thin CLI over the library; registers the bundled plugins. `txi`: the internal binary the engines' wired hooks run (`txi hook <Event>`) | tx-core and every plugin above |

The library never parses arguments and never reads the process environment. `CoreConfig`
carries everything; only `txc` / `txi` touch `std::env`. Without `--profile-dir` / `--skill-dir`,
`txc` resolves roles from `<data>/roles` (the roles plugins ship, written by `ui install`) and
then `<config>/roles` (yours, never pruned, overriding a shipped role of the same name), with
skills under each one's `skills/`.

## Where files live

Five directories, the XDG Base Directory layout the way nvim uses it (on macOS too), under the
application name `tx-ide`. `TX_APPNAME` renames all five at once, as `NVIM_APPNAME` does; it must
match `[A-Za-z0-9_.-]+`. Each directory resolves, first match wins:

| directory | flag | exported by core | XDG variable | default | holds |
|---|---|---|---|---|---|
| config | `--config-dir` | `TX_CONFIG_DIR` | `$XDG_CONFIG_HOME/<app>` | `~/.config/<app>` | `plugins.yaml`, `roles/` (yours) |
| data | `--data-dir` | `TX_DATA_DIR` | `$XDG_DATA_HOME/<app>` | `~/.local/share/<app>` | `sessions/<id>.json`, `log.jsonl`, `plugins/<id>/`, `roles/` (shipped), `bin/`, `keys/`, `conf/`, `tmux.conf` |
| state | `--state-dir` | `TX_STATE_DIR` | `$XDG_STATE_HOME/<app>` | `~/.local/state/<app>` | `locks/`, `profiles/<id>.md` |
| cache | `--cache-dir` | `TX_CACHE_DIR` | `$XDG_CACHE_HOME/<app>` | `~/.cache/<app>` | nothing yet |
| run | `--run-dir` | `TX_RUN_DIR` | `$XDG_RUNTIME_DIR/<app>` | `$TMPDIR/<app>.$USER` (else `/tmp`) | `tmux.sock` (the directory is mode 0700) |

Core exports all five, and `TX_APPNAME`, into every session it launches: tmux does not pass the
caller's `XDG_*` on, and this is how a `txc` or a `txi hook` inside a session opens the same core.
A plugin that re-invokes `txc` (key bindings, popups, detached children) passes
`CoreConfig::cli_arguments()`, the five flags plus `--socket` when it is overridden; the tmux key
fragments read the same words from the server option `@tx-arguments`.

`--state-dir` used to name the one directory everything lived in (`~/.tx-core`, or
`$TX_CORE_HOME`). It now names only the state directory, and `txc` refuses it, pointing at
`txc migrate`, when it is the only directory flag and names a directory with `sessions/` in it.

`txc migrate [--from DIR]` moves such a version-1 home (default `$TX_CORE_HOME`, else
`~/.tx-core`) once: `agents/` to `<data>/roles/`, `user-agents/` to `<config>/roles/`,
`plugins.yaml` to config, `sessions/`, `log.jsonl`, `plugins/`, `bin/`, `keys/`, `conf/` and
`tmux.conf` to data, `profiles/` to state; `locks/` and the socket stay. It refuses while the old
server has sessions (stop them first), when the old home has a `MOVED-TO` (a second run), and
when a destination already holds something, so two homes are never merged. It renames within a
filesystem and copies then deletes across filesystems, writes `<data>/MIGRATED-FROM` and
`<old>/MOVED-TO`, keeps the old directory, and finishes with `ui install`, since the moved
`tmux.conf` names the old paths. Records are moved as they are.

## Architecture

```
            txc (CLI)             your program / IDE
                 \                     /
                  v                   v
          +---------------------------------+
          |  TxCore  (one struct, open once) |
          |  config   CoreConfig            |
          |  store    SessionStore   <data>/sessions/<id>.json
          |  tmux     Tmux           tmux -S <run>/tmux.sock -f <data>/tmux.conf
          |  events   EventLog       <data>/log.jsonl
          |  engines  EngineRegistry id -> Box<dyn LlmEngine>
          |  profiles Profiles       role + skill search dirs
          |  locks    Locks          <state>/locks/{create,<id>}.lock
          +---------------------------------+
                        |
                        v   new-session -d -e TX_SESSION_ID=.. -e TX_TMUX_SOCKET=.. -e TX_*_DIR=.. -- program args
          +---------------------------------+
          |  private tmux server            |   session option @tx-id = record id
          |  session "worker-1"  pane %3    |   $N / %N looked up by @tx-id, never stored
          |  session "reviewer"  pane %5    |
          +---------------------------------+
```

### Where state lives

| home | what |
|---|---|
| disk (`sessions/<id>.json`) | the record: id, name, parent, tags, cwd, timestamps (`last_modified_at` is stamped on every write), lifecycle, launch, and an agent's `state`. The snapshot every read returns. |
| tmux | the link only: session option `@tx-id` = record id |
| looked up on demand | `#{session_id}` (`$N`) and `#{pane_id}` (`%N`) found by `@tx-id`, so a rename in tmux cannot break them |

`Lifecycle` is `Running`, `Exited` (vanished) or `Stopped` (`stop_session`). The two terminal
states are final: nothing in core moves a record out of them.

### Agent state: the engine adapter decides, core applies

An agent record carries `state` (an `LlmEngineState`): `starting`, `idle`, `working` or
`ended`. Idle means it is the operator's move, whether the turn finished or stopped on an approval
or a question; the next tool hook moves it back to `working`. The vocabulary is core's; when to
move between states is the adapter's. How long ago anything happened is the record-wide
`last_modified_at`.

At launch the adapter wires the hooks it needs into the CLI's own command line (Claude
`--settings '{"hooks":…}'`, Codex `-c hooks.<Event>=…`), each running `CoreConfig::hook_command`
plus the CLI's event name. For `txc` that is `txi hook <Event>`: `txi` is a second binary of
`tx-core-cli`, installed next to `txc`, the internal entry point machines run (nothing in it is
listed in `txc help`). It hands the payload to `TxCore::hook`, which asks
`LlmEngine::on_hook(state, &hook)` for the new state (`None`: nothing changes) and writes it
under the session lock, logging `state-changed` (`from → to`) when the state moved.
Core owns what the adapter cannot see: a new agent starts `starting`, and an exited or stopped one
reads `ended`. Process sessions have no state.

Because the hooks are `-c` overrides, every Codex session runs without Codex's shared background
server (Codex warns: "command-line configuration overrides … requires embedded mode"). Sessions
with a role already did, through `-c developer_instructions`; only role-less ones change.

### One operation, end to end: `create_session`

1. Validate the name (`[A-Za-z0-9_-]{1,128}`), the env keys, and that `cwd` is a directory;
   canonicalize `cwd`.
2. Pure checks before any lock: the engine must be registered, the prompt must be there, the
   profile must resolve.
3. Take `create.lock`. A process: refuse the name if a `Running` record holds it. An agent: make
   its own detached worktree of the checkout `cwd` at `HEAD`, under
   `<data>/worktrees/<repository-key>/<slug>--<name>`, with the first name free among `Running`
   records and on disk (`name`, `name-2`, … `name-99`); that is the agent's name and `cwd` from
   here on. A `cwd` that is not a git checkout fails with git's own words.
4. Mint the id. For an agent: write `<state>/profiles/<id>.md`, call `engine.launch(&request)` (pure),
   apply the plan's workspace actions under the worktree, merge its env.
5. `tmux new-session -d -s <name> -c <cwd> -e K=V … -- program args`, then `set-option @tx-id`.
6. Save the record, append `created` to the log. If anything after step 3 fails, the worktree is
   removed; after step 5, the tmux session is killed and the profile file removed: no orphans.

Env layering, later wins: `TX_SESSION_ID`, `TX_TMUX_SOCKET`, the five `TX_*_DIR`, then
`CoreConfig::env` (where `txc` puts `TX_APPNAME`),
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
use tx_core::{
    EngineError, LaunchPlan, LaunchRequest, LlmEngine, LlmEngineHook, LlmEngineState,
    WorkspaceAction,
};

pub struct Aider;

impl LlmEngine for Aider {
    fn id(&self) -> &str { "aider" }

    // No hooks wired: the session stays `starting`. See the bundled adapters for real rules.
    fn on_hook(&self, _: LlmEngineState, _: &LlmEngineHook) -> Option<LlmEngineState> { None }

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

`launch` and `on_hook` are pure: no I/O. Core writes the profile file, applies the workspace actions (relative
to `cwd`, refusing paths that escape it), starts the process. That makes an adapter testable
with a `Profile` and nothing else, as the two bundled engines show. Put each engine in its own
crate depending only on `tx-core`; the CLI registers the bundled ones in `main`.

### Reading the operation log from outside

`log.jsonl` is `{"ts","actor","type","msg"}` per line; types are `created`, `renamed`, `tagged`,
`message-sent`, `state-changed`, `exited`, `stopped`, `forgotten`. It is a notification trail, not the record:
on an event, re-read the session with `get_session`. Lines are never torn, so `tail -f` works.

### From inside an agent

Every session carries `TX_SESSION_ID`, `TX_TMUX_SOCKET`, the five `TX_*_DIR` and `TX_APPNAME`, so an agent can
run `txc send <name> …` and the message arrives as `<from-agent session="…">`. `txc` treats the
caller as an agent only when `$TMUX` names tx's own socket; anywhere else the sender is the user.

## Plugins

Everything but the core is a plugin: a crate under `plugins/<id>/` implementing `tx_core::Plugin`,
which fills any of four slots — engines, event sinks, verbs, resources (`tx-core/src/plugin.rs`). `txc`
links them through `tx-core-cli/src/plugins.rs::MANIFEST` and switches them in
`<config>/plugins.yaml` (`--plugins FILE` overrides):

```yaml
plugins:
  engine-codex: { enabled: false }
  artifacts:
    settings: { editor: nvim }
```

Missing file = everything on; an unknown id or a malformed file is a usage error. The id of a
retired plugin (`hooks`) is skipped, so an older file keeps loading. `txc plugins`
lists ids and status; `txc help` lists plugin verbs. Plugins keep per-session data in the
record's `ext.<id>` object via `TxCore::update_ext`. Design:
`../docs/superpowers/specs/2026-10-16-poc-plugins-design.md`.

A resource is a named static blob a plugin publishes with `Registry::resource(name, bytes)`
(`keys/<id>.tmux`, `bin/<file>`, `conf/<file>`, `agents/<file>`, a shipped role installed as
`<data>/roles/<file>`); the core stores it and lists it with
`TxCore::resources(prefix)` but never reads one, and a disabled plugin's resources are absent just
like its verbs (D8 in `../docs/superpowers/specs/2026-10-19-poc-key-plugins-design.md`).

| plugin | contributes |
|---|---|
| `engine-claude`, `engine-codex` | one engine each |
| `ide` | `txc ui install\|attach`, `txc ui view open <name> --kind <kind> [--parent <agent>] [--cwd <dir>] [--tag <tag>]… [--bound <value>] [-- <program> …]\|ls\|close <name>` (and the hidden `_resource <name>`); resources `conf/tmux.conf` (theme), `keys/ide.tmux` (navigation keys, C-u / C-d, M-digits, status line, pane-border look), `bin/tmux-nav`, `bin/tmux-system-resources` |
| `labels` | `whoami` and the internal `_relabel`, `_pane-label`, `_pane-info`, `_tmux-name`; resource `keys/labels.tmux` (prefix+s, the pane-border label, the after-new-window hook) |
| `kill` | `txc kill <name>`; resources `keys/kill.tmux` (prefix+X) and `bin/tmux-kill-tx-session` |
| `picker` | `txc pick` (the fzf session picker) and the internal `_list`; resource `keys/picker.tmux` (prefix+t). Its Ctrl-T and Ctrl-D / Y binds appear only while `edit` and `kill` are active |
| `edit` | the internal `_edit-session` and `_edit-tag`; resources `keys/edit.tmux` (prefix+e) and `bin/tmux-edit-tx-session`; owns `ext.edit` (`{"group": …}`) |
| `spawn` | the internal `_new-agent <pane> <client>` (the form, then `create_session`); resources `keys/spawn.tmux` (prefix+n), `bin/tmux-new-agent`, `agents/COMMON.md`, `agents/DEVELOPER.md`, `agents/skills/**` (from the repo's `agents/` tree) |
| `chat` | `resume`, `chat ls <session>`, `chat hooks install\|uninstall`, and the internal `_fork-pane` (prefix+f: `TxCore::fork_session` with the name from the popup), `_chat-hook`; resources `keys/chat.tmux` (prefix+f) and `bin/tmux-fork-tx-session`; owns `ext.chat` |
| `history` | `history` (the mirrored chats), `history-hooks install\|uninstall`, the internal `_history-ingest`; owns `ext.history` and `<data>/plugins/history/` |
| `assistant` | `assistant warm` (start the `tx-assistant` session unless it is running; prints its name); resource `agents/TX-ASSISTANT.md` |
| `mockup` | `mockup start --project DIR` (session dir under `<project>/.superpowers/mockup/<id>`, the `mockup-<agent>` view), `show` (switch the operator's outermost client there), `wait`, `stop`, and the internal `_mockup-view`; owns `ext.mockup` (`{"dir": …}`); resources `agents/COMMON.local.md`, `agents/skills/tx-mockup/SKILL.md` |
| `tutor` | `tutor start [LANG] [--dir DIR] [--detach]` (scaffold `~/tx-tutor/<lang>` as a git repo, open the `tutor-<lang>` view: lesson nvim, key cheat sheet, two shells), `check` (run the lesson's http / tx / file / command / confirm checks, advance), `next`, `prev`, `goto N`, `status`, `hint`, `reset [--hard]`; progress in `<data>/plugins/tutor/<lang>.json`; resources `tutor/**` (the repo's `tutor/` packs, materialised under `<data>/plugins/tutor/packs/`, plus its own `lesson.lua` bound to the core's socket). `tx` checks read the record's `status.state`; `chat_origin` matches nothing until a chat plugin exists |

| `artifacts` | `artifact <sub>` and `group <session> [<group> \| --clear]`, the internal `_artifact-menu`, `_artifact-pick`, `_artifact-go`; owns `ext.artifacts` (`{"bound": artifact id, "group": …}`) and the store `<data>/plugins/artifacts/` (`<id>.json`, `<id>/current<ext>`, `<id>/revs/<n><ext>`, its own `.log.jsonl` and `.locks/`); setting `editor` (default `nvim`) is what an artifact's view runs; resources `keys/artifacts.tmux` (prefix+a) and `bin/tmux-artifact-menu` |
| `timeline` | `timeline [--last DURATION] [--snapshot]` and `timeline health`, the internal `_timeline-resident`, `_timeline-attach` (start or attach the resident `tx-timeline` view; q detaches, the view stays warm) and `_timeline-json`; reads `ext.chat` / `ext.history` (transcripts), `status` (live state), `ext.edit` (group); working / idle spans come from the transcripts' turn markers; resource `keys/timeline.tmux` (prefix+T) |
| `menu` | `menu` and the internal `_menu-open <pane> <client>` (the menu in an 80% popup; after it closes, runs the choice through `txc` / tmux: `_new-agent`, `pick`, `_fork-pane`, `_timeline-attach`, `_artifact-menu`, `tutor start`, `assistant warm` + `switch-client`, `resume`), `_menu-first` (the client-attached hook: test-and-sets the server option `@tx-menu-shown`, skips clients nested in the server's own panes) and `_menu-tui`; items of disabled plugins are hidden; reads `ext.chat` (chat counts) and the other plugins' `keys/*.tmux` (the cheat sheet); resource `keys/menu.tmux` (prefix+m, hook `client-attached`) |

The picker's role column reads `ext.spawn.kind`, which a later plugin writes; until then every
process session shows `shell`.

`_new-agent` runs main's nvim form (`plugins/spawn/nvim/tx-new-agent.lua`, embedded and written
to a temp dir at run time): name, tags, project (the firing pane's directory), prompt, engine
(`core.engine_ids()`), model, effort (`low` to `max`) and role (every `<NAME>.md` in the profile
dirs but COMMON, which every agent gets; `none` first, DEVELOPER preselected). Needs `nvim`.
The session is created in the popup; the outcome lands on the client's status line.

`assistant warm` creates `tx-assistant` with the engine `claude`, roles COMMON + TX-ASSISTANT,
model `opus`, effort `medium`, tag `tx-system`, in the data dir (so the engine's skill links
land under `<data>/.claude/skills`). A `Running` record whose tmux session is gone is
refreshed to `Exited` and the assistant started again.

### Chat and history

Both plugins install their own agent hooks, each in its own sidecar; neither needs the other.
(Agent state needs no install: the engine adapters wire their hooks per launch.) Run each once
per machine:

```
txc chat hooks install      # captures the agent's chat id into ext.chat (resume)
txc history-hooks install   # mirrors transcripts byte for byte on every Stop
```

`uninstall` reverses either, and `--dry-run` prints the change. Without the chat hooks nothing
records the chat id, so `resume` reports `has no chat`. Fork is core's: `txc fork SOURCE --prompt T`
branches the source's conversation into a new agent in its own worktree (the engine reads the
conversation from `ext.<engine>`, not from `ext.chat`).

Data lives under the data dir: `plugins/history/<session>/<chat>/` (the mirrored transcript and
its sidecar files). Per-session state is `ext.chat` and `ext.history`. prefix+f forks
the session in the active pane through `_fork-pane`; the outcome is on the status line.

## Running the IDE

Needs tmux, `fzf` and `curl` (the picker reloads itself through fzf's `--listen` port).

```
txc ui install    # writes <data>/{bin,keys,conf,roles}/* and <data>/tmux.conf
txc ui attach     # attaches this terminal to the core server's `home` session
```

`ui install` materialises each active plugin's resources under `<data>/{bin,keys,conf,roles}` and
writes `<data>/tmux.conf`, which sets `@tx-bin`, `@tx-data` and `@tx-arguments` and runs
`keys/ide.tmux` and then every other active plugin's `keys/<id>.tmux`. The key bindings and
`bin/*` find `txc`, its directories and the helper scripts through those server options, so re-run `ui install` after rebuilding `txc`. Files in those
directories that no active plugin provides are deleted, so `txc` needs nothing from the source
tree at run time. It refuses a binary or directory path containing whitespace, a backslash, a
backtick or any of `"'$#;`, since the paths are spliced into tmux config lines.

Disabling a plugin in `plugins.yaml` and re-running `ui install` removes that plugin's keys, even
on a running server: `ui install` unbinds the keys the plugin's fragment declared, clears the
options and hooks it declared, and prints `reloaded`. When it finds no previous
`keys/.installed` on a running server (a server set up by an older `ui install`), it also prints
`legacy bindings may remain; restart the server to drop them`.

`ui attach` creates `home` on first use as a view of kind `home` (status bar and pane-border
labels on) and attaches to it. When the `assistant` plugin is active it first runs `assistant warm`, every
time, so the assistant is up whenever the IDE is; a new `home` starts with its pane
nest-attached to `tx-assistant`, so the IDE opens on the assistant. An existing `home` is left
as it is (reach the assistant with prefix+t). A failed warm (no `claude`, roles not installed)
is printed to stderr and `home` opens as a plain view. To try the IDE, press prefix+n in it, or
start a session by hand and press the picker key:

```
txc create demo --cwd ~/code/project --exec zsh
txc ui attach     # then prefix+t
```

**Views.** Every non-LLM tmux session on the core's server is a tx-client *view*: the `home`
base, the `tutor-<lang>`, `mockup-<agent>` and `tx-timeline` viewers, and the shells and editors
`txc ui view open` starts. A view has no core record and never shows in `txc ls`; its fields live
in tmux session options only (`@tx-view 1`, `@tx-view-kind home|editor|shell|timeline|tutor|mockup`,
`@tx-view-parent <agent id>`, `@tx-view-bound`, `@tx-view-tags`, `@tx-view-created`), so nothing
goes stale when the server dies. `txc ui view ls` first closes the views whose parent agent is
gone (forgotten or no longer running), then lists the rest. Closing and renaming go through the
`views` guard, which refuses any session core marked with `@tx-id`: an agent is stopped only
through `txc kill`. prefix+X closes a shell, editor, timeline, tutor or mockup view after a
confirmation, keeps refusing the home base, and kills an agent through `txc kill` as before.

The prefix is `C-a`.

| keys | does |
|---|---|
| prefix+t | session picker; Enter nests into the session |
| prefix+n | new agent: the form, then the session in the pane's directory |
| prefix+e | edit the session's name, tags and group |
| prefix+X | kill the session, with a confirm prompt |
| prefix+f | fork the session in the pane into a new worktree: a popup asks for the new name (blank: `<name>-fork`); the fork's first prompt is "You are a fork of `<name>`. Introduce yourself." |
| prefix+s | refresh labels, then `choose-tree` |
| prefix+a | in an agent: the artifacts it touched, its parent and children; Enter re-hosts the pane on the pick, f focuses the pane showing it, p pops it up. In an artifact's view: back to the agent |
| prefix+T | the resident timeline in a popup; q detaches, Enter jumps to the lane's session |
| prefix+m | the start menu (its keys mirror the prefix keys): n new, t attach, h history, f fork, T timeline, a artifacts, p plugins, K keys, ? tutor, / assistant, 1-3 resume a recent session, q quit; it also opens by itself on a fresh server's first attach |
| C-h / C-j / C-k / C-l | move between panes; the move bubbles out of nested sessions |
| C-u / C-d | page the agent's scrollback |

The pane border shows the nested session's name and tags. Later plugins bring their own keys
(/) in their own `keys/<id>.tmux`.

### Adding a plugin

A plugin is a crate under `poc/plugins/<id>` (package `tx-<id>`, depending on `tx-core` and, for
anything that talks to tmux, `tx-client`) whose `Plugin` impl registers verbs, engines, sinks and
resources. Wire it in with three edits: a `plugins/<id>` entry in `members` in `poc/Cargo.toml`,
a `tx-<id> = { path = "../plugins/<id>" }` dependency in `poc/tx-core-cli/Cargo.toml`, and one
line in `tx-core-cli/src/plugins.rs::MANIFEST`; a verb may not take a name in
`plugins.rs::RESERVED`. Give it a `tests/resources.rs` (what it publishes, and nothing when
disabled; `kill`'s is the template) and a `poc/tx-core-cli/tests/<id>.rs` through the binary
(the `tests/common` harness has one temp directory for all five, a private server, and `shim_path` for stand-in
executables such as a `claude` that sleeps).

Resources land under the data dir when `ui install` runs: `bin/` and `keys/` executable,
`conf/` and `roles/` (the `agents/` resources) plain; files there that no active plugin provides are deleted, so a plugin
that publishes any of these must also be listed in `tests/ide.rs::ui_install_materialises_resources`.

### Adding a key

A key is a plugin, with:

- its verb(s), registered with `Registry::verb`;
- a `tmux/keys.tmux` registered as resource `keys/<id>.tmux`, which binds the key. A comment
  line near its top declares the prefix keys it binds: `# keys: <key>` (`none` for root-table binds only). If it
  sets global state, it also declares `# unset: <option> …` and `# unhook: <hook> …`, so that
  `ui install` can undo them when the plugin is disabled;
- optionally helper scripts, registered as `bin/<file>` resources (they land in `<data>/bin`,
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
| Engine envelope | default method `LlmEngine::envelope(&self, sender, text) -> String` | pure; core still types it |
| Chat ops (resume, fork, transcripts) | separate crate with a second registry keyed by engine id, `trait EngineChatOps` | one future `resume` field on `LaunchRequest` |
| Declarative engines | one plugin `Declared { id, program, args }` with `{profile_file}`/`{model}`/`{prompt}` substitution, registered N times from `plugins.yaml` settings | ids validated by the registry |
| Launch decorators | a fourth registry slot letting a plugin edit a `LaunchPlan` (per-session hook injection without touching `~/.claude`) | pure, ordered by manifest |

Before freezing the API: add `Send + Sync` to `LlmEngine`, put a session id in every log
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
pinned. They are slow (thousands of cases, real tmux servers), so they only build behind the
`fuzz` feature: plain `cargo test --workspace` skips them; run them on purpose with
`cargo test --workspace --features fuzz`. Two tests stay `#[ignore]`d as documented platform limits: a cooked-mode tty drops a
single line over 1024 bytes, and flock cannot protect a lock file that is deleted under a holder.
