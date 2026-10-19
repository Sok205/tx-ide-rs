# tx-core Port Roadmap

> **For agentic workers:** this is the sequencing document, not a task plan. Each phase below
> gets its own task plan under `docs/superpowers/plans/` (REQUIRED SUB-SKILL:
> superpowers:writing-plans), executed with superpowers:subagent-driven-development or
> superpowers:executing-plans. Phases are checkboxes; a phase is done when its gate holds.

**Goal:** Every feature `crates/tx` ships lands in `poc/` as a plugin over `tx-core`, until
`txc` can replace `tx` for the daily swarm workflow. The tmux prefix bindings come first: a
user at the keyboard must lose nothing while the rest of the port proceeds.

**Architecture:** `tx-core` stays the plain core from `2026-10-15-tx-core-design.md`; features
are compiled-in plugin crates under `poc/plugins/<id>/` filling the `Registry` slots from
`2026-10-16-poc-plugins-design.md`. Per-session plugin state lives in `Session.ext`; bulk
plugin state lives in a plugin-owned directory. The core grows only by slots that at least two
plugins need.

**Tech Stack:** Rust 2024 (`rust-version = "1.92"`), serde / serde_json, thiserror, tmux via the
private socket, proptest for fuzz suites, ratatui 0.30 (timeline only).

**Spec:** `docs/superpowers/specs/2026-10-15-tx-core-design.md` (the core),
`docs/superpowers/specs/2026-10-16-poc-plugins-design.md` (the host). This roadmap records the
decisions taken on 2026-10-19 that those specs left open.

## Global Constraints

- `poc/` is its own Cargo workspace; the root `Cargo.toml` keeps `exclude = ["poc"]`.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` pass in
  `poc/` after every phase; tests use a temp state dir and a private tmux socket
  (`require_tmux!` skip).
- The library never parses arguments and never reads the process environment; only `txc` and
  plugin verbs (CLI-side code) touch `std::env`.
- Plugin ids are `[a-z0-9-]+`; verb names never shadow `RESERVED` in `tx-core-cli/src/plugins.rs`.
- Every tmux call made by a plugin carries `-S <core.socket_path()>` and `-f <state_dir>/tmux.conf`;
  the user's default server is never touched.
- Record writes stay temp-file + fsync + rename; plugin stores follow the same discipline.
- Python-parity shims (`argparse.rs`, `pyjson.rs`, `shlex.rs`, `palette.rs`) are not ported;
  `txc` output is its own contract.
- Every `prefix+<key>` in `tmux/tx-ide.tmux` keeps its key and its visible behaviour; only the
  command behind it changes from `tx` to `txc`.

## Review Focus

Failure modes the specs imply that no single phase's tests own; each owning phase adds the test.

1. A record whose `ext["<plugin>"]` is malformed for that plugin (hand edit, older plugin
   version) must be reported by that plugin's verb and never make `txc ls`, `txc show` or the
   prefix+t picker fail (phase 1 pins it in the picker feed; every later phase for its own key).
2. A `LaunchDecorator` that fails after an earlier decorator wrote into the workspace must leave
   no orphan: core's cleanup path covers the profile file and the tmux session, decorators cover
   their own writes (phase 2, read-only wrapper and skill symlinks).
3. Obsolete: there is no `ExtUpdated` event (phase 3 added no core event).
4. A chat transcript owned by another session must never be bound to this one (phase 3, the
   cross-bind refusal from `hooks.rs::capture_chat_ref`).
5. A plugin store under `<state_dir>/plugins/<id>/` written by two `txc` processes at once must
   not lose a write (phase 3 pins it with the history mirror flock; phase 5 with artifact revs).

---

## Decisions (2026-10-19)

- **D1. A plugin may own a store.** The plugins design said "no plugin-private stores"; that is
  withdrawn. Per-session facts stay in `Session.ext` (one JSON value per plugin, written under
  the session lock). Anything else a plugin persists lives under `<state_dir>/plugins/<id>/`,
  which core hands out and never reads.
- **D2. Core grows only by generic slots.** No type, field, event or method in `tx-core` may
  name a feature. An addition is allowed when it is the same mechanism for two or more plugins,
  present or clearly next. Each one in the table below names its consumers; an addition with
  one consumer waits until the second arrives.
- **D3. Plugins do not call each other.** Crates may depend on each other for library code
  (the timeline reuses the chat crate's message parser), but at run time a plugin learns about
  another only through `Session.ext` (read) and events (`ExtUpdated`). No capability registry.
- **D4. Engine knowledge may live in a feature plugin.** `hooks` already knows the Claude and
  Codex hook-file layouts. Likewise the chat plugin owns the transcript, fork and history-bundle
  layouts of every engine; engine plugins stay launch-only. This keeps `EngineAdapter` at its
  two methods instead of growing the optional `EngineChatOps` surface main has.
- **D5. UI seams do their own tmux.** Core's `Tmux` keeps new / kill / rename / send-keys /
  list-sessions. A plugin that needs popups, splits, `switch-client` or status messages runs
  tmux itself against `core.socket_path()`; that helper lives in the `tx-ui` library crate, shared
  by crate dependency with every later UI plugin.
- **D6. Prefix bindings drive the order.** Phases are sequenced so that each one turns on a
  set of `prefix+<key>` bindings end to end; a phase that enables no key comes after every
  phase that does.
- **D7. Antigravity is deferred.** No `engine-antigravity` plugin in this roadmap; its launch
  half and its chat half are listed under "Deferred" with what they would need.

## Generic core additions

Each row is its own small task plan, landed in the phase of its first consumer.

| addition | shape | consumers | phase |
|---|---|---|---|
| `Session.ext` read helper | `Session::ext<T: DeserializeOwned>(&self, plugin: &str) -> Result<Option<T>, serde_json::Error>` | picker (reads `hooks` state), every later plugin | 1 |
| launch slot | `trait LaunchDecorator: Send + Sync { fn id(&self) -> &str; fn decorate(&self, req: &LaunchRequest<'_>, plan: &mut LaunchPlan) -> Result<(), EngineError>; }`; `Registry::launch(Box<dyn LaunchDecorator>)`; core runs decorators in manifest order between `adapter.launch` and `apply_workspace` in `create_locked` (`poc/tx-core/src/core.rs:534-540`), on `LaunchOptions::Agent` only | spawn (read-only wrapper, skill symlinks), hooks (per-session hook injection), playground (companion env) | 2 |
| plugin resources | `Registry::resource(name, bytes)`, `TxCore::resources(prefix)` — named static blobs listed by prefix; the core never reads them | every key plugin (its `keys/<id>.tmux`), ide (conf, scripts), later hooks (templates), antigravity (.agents files) | 1 (landed 2026-10-19) |
| plugin data dir | `Registry::data_dir(&self) -> PathBuf` = `<state_dir>/plugins/<id>/`, created on first call, mode 0755 | chat/history, artifacts, timeline cache, tutor, sync | not added (phase 3 proved them unnecessary, see Phase 3) |
| `ExtUpdated` event | `EventType::ExtUpdated`; `Event.msg` = `{"plugin": id}`; emitted by `update_ext` only when the value changed, after the lock is released; logged to `log.jsonl` like every event | hooks → history ingest, hooks → timeline, chat ops → timeline | not added (phase 3 proved them unnecessary, see Phase 3) |
| sweep slot | `trait Sweeper: Send + Sync { fn id(&self) -> &str; fn sweep(&self, core: &TxCore, running: &[Session]) -> Result<(), SweepError>; }`; `Registry::sweeper(...)`; `refresh_all` runs sweepers after its own pass; a sweeper's error is logged to stderr by `txc`, never fails the refresh | hooks (stuck `working` demotion), chat (watchdog for unfinished chat ops), history (pending mirrors) | not added (phase 3 proved them unnecessary, see Phase 3) |

Not added, and why:

- Plugin data dir: the plugin derives `<state_dir>/plugins/<id>/` from `core.config()`.
- `ExtUpdated` event: each plugin installs its own agent hooks.
- Sweep slot: chat finishes abandoned ops lazily at the start of every chat verb; the
  stuck-`working` demotion stays open.
- `ls` columns for plugin state. `txc show` prints `ext`; plugins ship their own `<verb> status`
  (as `hook status` does), and the picker feed in `picker` reads `ext` directly.
- A capability or service registry between plugins (D3).
- `EngineAdapter` methods for transcripts, fork or seed (D4).
- New `Session` fields. Main's `group`, `attached_to`, `chats`, `artifact_id`, `nvim_socket`,
  `pid`, `state` all become `ext` entries of the plugin that owns them.

## Prefix bindings and the phase that turns each on

From `tmux/tx-ide.tmux`. "Behind it" names the main verb or script the key runs today.

| key | behind it | phase |
|---|---|---|
| prefix+t | `tx attach` (fzf picker popup) | 1 |
| prefix+e | `bin/tmux-edit-tx-session` → `tx _edit-session` | 1 |
| prefix+X | `bin/tmux-kill-tx-session` → `tx kill` | 1 |
| prefix+s | `tx _relabel` then `choose-tree` | 1 |
| nav keys | `bin/tmux-nav` (nested-session edge hop) | 1 |
| C-u / C-d | passthrough `if -F` on `pane_current_command` | 1 (tmux-only, re-pointed) |
| pane border | `bin/tmux-pane-session-name` | 1 |
| status line | `bin/tmux-system-resources` | 1 (no `tx` call; installed as is) |
| prefix+n | `bin/tmux-new-agent` → `tx _new-agent` → `spawn` | 2 |
| prefix+f | `bin/tmux-fork-tx-session` → `tx _fork-pane` → `fork` | 3 |
| prefix+/ | `bin/tx-assistant` → `send-user-message`, `focus-envelope` | 4 |
| prefix+a | `bin/tmux-artifact-menu` → `tx _artifact-menu` | 5 |
| prefix+T | `tx _timeline-attach` | 6 |
| focus hooks | `bin/tx-graph-focus-poke` (sessions-graph dashboard, not in this repo) | deferred |

## Phases

Sizes are main's line counts for the code being ported, a budget signal, not a target.

- [ ] **Phase 1 — the key plugins `ide`, `labels`, `kill`, `picker`, `edit`** (prefix+t, e, X, s, nav)
  - Five plugin crates under `poc/plugins/`, one id each: `ide` (`tx-ide`), `labels`
    (`tx-labels`), `kill` (`tx-kill`), `picker` (`tx-picker`), `edit` (`tx-edit`); and the
    library crate `poc/tx-ui` (`tx-ui`, not a plugin), which owns the extended tmux helper
    (D5): popups, `switch-client`, `display-message`, `-F` queries, the client ↔ pane join
    that `tmux-nav` and `tmux-pane-session-name` mirror.
  - From main: `render.rs` (the `ls` and picker tables), `verbs/listing.rs` (`attach`,
    `_list`, `_edit-session`, `_edit-tag`, `show` extras), `session_editor.rs`, the `kill`,
    `_relabel`, `_pane-info`, `_tmux-name`, `whoami`, `selfcheck` verbs of
    `verbs/sessions.rs`, `service.rs::kill`. ~4.3k lines.
  - `kill` here is `stop_session` plus the confirm prompt; worktree removal arrives with
    phase 2's `kill --remove`.
  - `txc ui install` writes the server options `@tx-bin` and `@tx-state`; `bin/*`, the tmux
    fragment and `nvim/lua/*` find the binary through them. Verb names ported from main keep
    main's spelling.
  - State: `ext["spawn"] = {"kind": "nvim|view|playground", "attached_to": id, "nvim_socket": path}`
    (written by phase 2; read here for the picker's role column); the group override is
    `ext["edit"] = {"group": …}`.
  - Core additions: `Session::ext` helper.
  - Gate: with `txc ui install` run against the core socket, prefix+t lists running
    sessions with hooks state and attaches on Enter; prefix+e renames and retags; prefix+X
    stops with the confirm prompt; prefix+s relabels `choose-tree`; `render` tables match
    main's fixtures byte-for-byte; a session with a malformed `ext["hooks"]` still lists.
  - Landed 2026-10-19 with these deviations:
    - the picker verb is `pick` (`attach` is a txc built-in);
    - no `tx` shim: scripts and the fragment find the binary through the server options
      `@tx-bin` and `@tx-state` written by `txc ui install`;
    - the group override lives in `ext.edit.group` (phase 5's grouping reads it from there);
    - `selfcheck` is not ported;
    - the TPM/resurrect/continuum block is dropped from the poc `tmux.conf`;
    - the fragment emits the prefix+T, /, f, n, a binds only when their verb exists in
      `txc help` (superseded the same day: a plugin's fragment is installed only while the plugin
      is active; no `txc help` gating remains);
    - `ui attach` is the way into the server until phase 7's `start`;
    - `render_ls` and the `show` extras are not ported (txc's `ls` and `show` are core
      built-ins);
    - `--host` and `--all` are not ported;
    - `ui attach` creates the `home` view (`@tx-view`, status on, pane-border labels) when it is
      missing, so the picker's nest path applies from the first attach;
    - restructured the same day into five plugins per `2026-10-19-poc-key-plugins-design.md`
      (D8–D12): one plugin per key, each carrying its `keys/<id>.tmux` resource; `ide` composes
      the server config from resources; `--poc` gone; the `@tx-ide-*` toggles replaced by plugin
      enablement.

- [ ] **Phase 2 — `spawn`** (prefix+n; proves the launch slot)
  - Crate `poc/plugins/spawn` (`tx-spawn`), plugin id `spawn`.
  - From main: `spawn.rs`, `worktree.rs`, `skills.rs` (materialisation; resolution is already
    `tx-core::profile`), `read_only.rs`, the spawn family of `verbs/sessions.rs` (`spawn`,
    `spawn-nvim`, `spawn-view`, `kill --remove`, `archive`, `revive`, `rm`, `parent`),
    `service.rs::spawn_worker_with`, `worker_launch_command`, `next_worker_name`,
    `remove_worker_worktree`, `spawn_nvim`, `spawn_view`, `verbs/new_agent.rs` and its
    `nvim/lua/tx-new-agent.lua` form. ~3.5k lines.
  - Role data: `agents/COMMON.md`, `DEVELOPER.md`, `user-agents/NAME.md` override and
    `NAME.local.md` extend, `agents/skills/*`; wired through `CoreConfig::profile_dirs` and
    `skill_dirs` by `txc`.
  - State: `ext["spawn"] = {"worktree": path | null, "read_only": bool, "role": [..],
    "archived_at": ts | null}`; `ext["spawn"]` also holds
    `{"kind": "nvim|view|playground", "attached_to", "nvim_socket"}` for nvim and view sessions.
    Worktrees live under `<state_dir>/worktrees/` as main does.
  - Core additions: launch slot. Two decorators in this crate: `ReadOnly` wraps `plan.program`
    in `sandbox-exec` / `bwrap` when `TX_SPAWN_READ_ONLY=1` is in `CreateOptions::env`;
    `Skills` appends `WorkspaceAction::Symlink` for each granted skill into the engine's
    discovery dir.
  - Gate: prefix+n opens the form and spawns a worker in a fresh worktree whose record carries
    the path; `kill --remove` removes both; a read-only worker cannot write inside the repo but
    can outside; `spawn-nvim` and `spawn-view` land in `ext["spawn"]` and show in the picker.
  - First slice landed 2026-10-19 (`poc/plugins/spawn`): prefix+n → `bin/tmux-new-agent` →
    `_new-agent`, main's nvim form embedded in the crate, the session created inline through
    `create_session` (no worktree, no read-only wrapper, no `ext["spawn"]` yet); the roles ship as
    `agents/` resources that `ui install` materialises, and `txc` defaults its profile and skill
    dirs to `<state>/agents` and `<state>/user-agents`. Worktrees, `kill --remove`, the launch
    slot and the spawn verb family remain.

- [ ] **Phase 3 — `chat`** (prefix+f; landed with zero core changes)
  - Crate `poc/plugins/chat` (`tx-chat`), plugin ids `chat` and `history` (one crate, two
    plugins, so `plugins.yaml` can disable chat ops while keeping ingest).
  - From main: `history.rs`, `chat.rs`, `messages.rs`, `verbs/chat.rs`, `verbs/fork_pane.rs`,
    the `history` and `chat ls` verbs of `verbs/listing.rs`, the transcript / rollout / bundle
    code of `engines/claude.rs`, `engines/codex.rs`, `engines/codex_rollout.rs`, the chat-ref
    capture of `hooks.rs::capture_chat_ref`. ~5k lines.
  - Landed with zero changes to `tx-core` and zero to `hooks`. `chat hooks install` and
    `history-hooks install` each register the plugin's own agent hooks next to the `hooks`
    plugin's (own sidecar, own code); the hidden verbs `_chat-hook` and `_history-ingest` run
    from them.
  - State: `ext["chat"] = {engine, launch, chats: [{id, role: primary|fork|handover|rollover,
    how, since, ended_at, origin, transcript}]}` and `ext["history"] = {chats: {<chat-id>:
    {transcript, bundle, at}}}`, the shapes of Task 1 (`ChatExt`, `HistoryExt`). Mirrored
    bundles live under `<state_dir>/plugins/history/<session-id>/<chat-id>/` (main's
    `history/`), one flock per (session, chat); ops state under `plugins/chat/`.
  - Forks are `Process` records: the engine crates' pure `launch` builds the argv, then
    `--fork-session` (claude) or `fork` (codex) is spliced in; the real agent launch is kept in
    `ext["chat"].launch`. Rollover is a `respawn-pane` of the source's pane.
  - Core gap found: fork wants a `fork` flag on `LaunchOptions::Agent` / `LaunchRequest`,
    honoured by `EngineAdapter::launch` and recorded in `Launch::Agent`. A fork could then be a
    normal Agent record and `ext["chat"].launch` would go away. Until then the picker shows
    forks as `shell`.
  - Same result for the plugins merged on branch `poc`: `artifacts` (its own `.log.jsonl`, since
    the core event types are closed; a flock per artifact) and `tutor` landed with zero core
    changes too.
  - Verbs: `fork`, `handover`, `rollover`, `resume`, `history [<session>]`, `chat ls`,
    `_fork-pane`, hidden `_chat-op-finish`, `_chat-op-watch`, `_history-ingest`.
  - Gate: prefix+f forks the session in the firing pane and reports on the status line; a
    Claude session's transcript is mirrored byte-for-byte after the installed Stop hooks; the fork's
    `ext["chat"]` ref is filled by the next hook; the cross-bind refusal test passes; `messages`
    parses main's fixture transcripts; `resume` relaunches from `ext["chat"]`.

- [ ] **Phase 4 — `assistant`** (prefix+/)
  - Crate `poc/plugins/assistant` (`tx-assistant`), plugin id `assistant`.
  - From main: `bin/tx-assistant` (`--from-buffer`, `--warm`), `agents/TX-ASSISTANT.md`,
    `service.rs::focus_envelope` and `transportable_command`, the `focus-envelope` verb, the
    `start` verb's `--warm` call. ~0.6k lines of Rust plus the script and role.
  - Depends on phases 2 and 3: the assistant is a spawned agent with the `tx-assistant` role,
    tagged `tx-system`, that receives `send-user-message` lines wrapped in the focus envelope
    and spawns workers, nvim companions and shells on request.
  - State: none beyond the session's own record and `ext["spawn"]`.
  - Gate: prefix+/ with the assistant down creates it, primes it, and delivers the line; with
    it up, delivers the line with the envelope naming the focused pane's session.
  - First slice landed 2026-10-19 (`poc/plugins/assistant`): `assistant warm` creates or reuses
    the session (COMMON + TX-ASSISTANT on claude, `tx-system`, cwd = the state dir), and
    `ui attach` nests a new `home` into it. No priming message (the profile travels as
    `--append-system-prompt`), no prefix+/, no focus envelope yet.

- [ ] **Phase 5 — `artifacts`** (prefix+a)
  - Crate `poc/plugins/artifacts` (`tx-artifacts`), plugin id `artifacts`.
  - From main: `artifact.rs`, `artifact_store.rs`, `artifact_service.rs`, `difflib.rs`,
    `grouping.rs`, `verbs/artifacts.rs` (not `sync`), `verbs/artifact_menu.rs`,
    `artifact_picker.rs`, the `group` verb. ~4k lines.
  - State: `data_dir()/<id>.json` + `data_dir()/<id>/current<ext>` + `revs/`; the touching
    session's id comes from `TX_SESSION_ID`; `ext["artifacts"] = {"bound": id, "group": name}`;
    `grouping::resolved` reads parent chain + bound artifact exactly as main does.
  - Verbs: `artifact create|modify|group|ls|show|diff|open|doctor`, `group`, `_artifact-menu`,
    `_artifact-pick`, `_artifact-go`. `artifact open` uses `tx-ui` as a library for the
    nvim view.
  - Gate: prefix+a in an agent pane lists the artifacts it touched and attaches, focuses or
    pops the chosen one; main's artifact fixtures round-trip; `doctor --repair` removes only
    the orphan rev; two concurrent `modify` calls produce two contiguous revs.

- [ ] **Phase 6 — `timeline`** (prefix+T)
  - Crate `poc/plugins/timeline` (`tx-timeline`), plugin id `timeline`. The only ratatui user.
  - From main: `plugins/timeline/*` (~3.7k lines). Rows come from `log.jsonl` (`Created`,
    `Exited`, `Stopped`, `MessageSent`, `ExtUpdated{hooks}` for working / idle spans) and from
    `tx-chat`'s message parser for arrows. The sqlite cache moves to `data_dir()/cache.sqlite`.
  - Verbs: `timeline`, `_timeline-attach`, `_timeline-resident`, `_timeline-json`, `health`.
  - Gate: prefix+T opens the resident timeline popup and q detaches it; `TestBackend` snapshot
    tests from main pass with the row source swapped; `_timeline-json` matches main's fixture.

- [ ] **Phase 7 — `start`, `tutor`, `playground`** (no new keys)
  - `start`: home base. Boots the IDE (`ui attach`'s `home` view), warms the assistant, `--tutor [LANG]` lands in the
    walkthrough. Lives in a `start` plugin.
  - `tutor`: crate `poc/plugins/tutor`, plugin id `tutor`. From `tutor/` and `verbs/tutor.rs`;
    verbs `tutor start|check|next|prev|goto|status|hint|reset`; progress under
    `data_dir()/<lang>.json`; packs stay in `tutor/<lang>/`. ~1.8k lines.
  - `playground`: `playground.rs` as a `LaunchDecorator` registered by a `playground` plugin (using `tx-ui`) plus the companion
    verb. ~0.4k lines.
  - Gate: `tx start --tutor python` opens the tutor view and lesson 1 checks pass against a
    spawned worker; a worker's Bash commands replay in its playground pane.

- [ ] **Phase 8 — `install`, `sync`, `migrate`** (the long tail)
  - `install`: `install` and `uninstall` scripts, `verbs/install.rs` (`_engines`,
    `_statusline`), `verbs/home.rs` (`_init-home`), `claude/statusline.sh`,
    `setup/engines/install.sh`, `claude.sh`, `codex.sh`, the hook shims under
    `<state_dir>/hooks`, the `~/.tx-ide/tmux.conf` shim and `~/.tmux.conf` block,
    `setup/iterm.sh`, `setup/nvim.sh`, `engines/codex_update.rs` (`_codex-update`). All as
    hidden verbs of a crate `poc/plugins/install`; `_claude-settings` and `_codex-hooks` are
    already `hook install`. ~2.5k lines.
  - `sync`: crate `poc/plugins/sync`, from `sync.rs` and `storage.rs` (the `Storage` boundary
    and S3). Mirrors `sessions/`, `log.jsonl`, `plugins.yaml` and every `plugins/<id>/` that
    opts in through a settings list. Verbs `sync push|pull|status`. ~1.1k lines.
  - `migrate`: crate `poc/plugins/migrate`, from `migrations.rs`. One-shot import of a
    `$TX_IDE_HOME` v6 tree into `<state_dir>`: records to core fields plus `ext` entries per
    the mapping in phases 1 to 5, `history/` to `plugins/chat/` → `plugins/history/`, `artifacts/` to
    `plugins/artifacts/`, `tutor/` to `plugins/tutor/`; `config.json`'s reconcile threshold,
    `sync` section and `plugins` section to `plugins.yaml` settings. ~1k lines.
  - Gate: `install` against a temp `HOME` and `TX_CORE_HOME` yields a working `txc` with hooks,
    statusline and the tmux fragment registered; `migrate` of a fixture `$TX_IDE_HOME`
    produces records `txc show` prints and `history` lists.

## Deferred

- **Antigravity** (D7): the launch half of `engines/antigravity.rs` (Go `flag` grammar,
  `.agents/` files, settings trust, `_agy-template`, `setup/engines/antigravity.sh`) would be a
  plugin `engine-antigravity`; the sqlite conversation-db fork surgery and brain-transcript
  reader would be `tx-chat`'s third engine. ~1.3k lines. Needs nothing from core.
- **sessions-graph focus poke**: `bin/tx-graph-focus-poke` rings a dashboard server that is
  not in this repo. Port the script when the server has a home; the tmux focus hooks stay
  commented out until then.

## Not ported

- `cordis` and `app.rs`: the host is `plugin.rs` plus the manifest.
- `argparse.rs`, `pyjson.rs`, `shlex.rs`, `palette.rs`: Python parity is not a `txc` goal.
  `difflib.rs` is kept because `artifact diff` output is user-facing.
- `reconcile.rs`: `refresh_all` plus sweepers replace it.
- `engines/registry.rs` `RegistrationId`: `plug` fails on duplicates instead.

## Per-phase plan handoff

Each phase starts by writing `docs/superpowers/plans/2026-MM-DD-poc-<plugin>.md` with
superpowers:writing-plans against this roadmap and the two specs, including the core-addition
task(s) its row in the table above assigns to it. The plugins design spec's "Out of scope"
list is superseded by this document.
