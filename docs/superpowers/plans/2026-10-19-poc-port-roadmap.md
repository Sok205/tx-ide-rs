# tx-core Port Roadmap

> **For agentic workers:** this is the sequencing document, not a task plan. Each phase below
> gets its own task plan under `docs/superpowers/plans/` (REQUIRED SUB-SKILL:
> superpowers:writing-plans), executed with superpowers:subagent-driven-development or
> superpowers:executing-plans. Phases are checkboxes; a phase is done when its gate holds.

**Goal:** Every feature `crates/tx` ships lands in `poc/` as a plugin over `tx-core`, until
`txc` can replace `tx` for the daily swarm workflow.

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

## Review Focus

Failure modes the specs imply that no single phase's tests own; each owning phase adds the test.

1. A plugin store under `<state_dir>/plugins/<id>/` written by two `txc` processes at once must
   not lose a write (phase 1 pins it with the tutor progress file; phase 5 with artifacts).
2. A `LaunchDecorator` that fails after an earlier decorator wrote into the workspace must leave
   no orphan: core's cleanup path covers the profile file and the tmux session, decorators cover
   their own writes (phase 2, read-only wrapper and skill symlinks).
3. An `ExtUpdated` sink that reopens the core on the same state dir must not deadlock on the
   session lock the writer still holds: the event is emitted after the lock is released
   (phase 3).
4. A record whose `ext["<plugin>"]` is malformed for that plugin (hand edit, older plugin
   version) must be reported by that plugin's verb and never make `txc ls` or `txc show` fail
   (every phase; first pinned in phase 1).
5. A chat transcript owned by another session must never be bound to this one (phase 3, the
   cross-bind refusal from `hooks.rs::capture_chat_ref`).

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
  tmux itself against `core.socket_path()`; that helper is a module of the plugin crate, shared
  by crate dependency if a second UI plugin needs it.

## Generic core additions

Each row is its own small task plan, landed in the phase of its first consumer.

| addition | shape | consumers | phase |
|---|---|---|---|
| plugin data dir | `Registry::data_dir(&self) -> PathBuf` = `<state_dir>/plugins/<id>/`, created on first call, mode 0755 | tutor, artifacts, chat/history, timeline cache, sync | 1 |
| `ExtUpdated` event | `EventType::ExtUpdated`; `Event.msg` = `{"plugin": id}`; emitted by `update_ext` only when the value changed, after the lock is released; logged to `log.jsonl` like every event | hooks → history ingest, hooks → timeline, chat ops → timeline | 3 |
| launch slot | `trait LaunchDecorator: Send + Sync { fn id(&self) -> &str; fn decorate(&self, req: &LaunchRequest<'_>, plan: &mut LaunchPlan) -> Result<(), EngineError>; }`; `Registry::launch(Box<dyn LaunchDecorator>)`; core runs decorators in manifest order between `adapter.launch` and `apply_workspace` in `create_locked` (`poc/tx-core/src/core.rs:534-540`), on `LaunchOptions::Agent` only | spawn (read-only wrapper, skill symlinks), hooks (per-session hook injection), playground (companion env) | 2 |
| sweep slot | `trait Sweeper: Send + Sync { fn id(&self) -> &str; fn sweep(&self, core: &TxCore, running: &[Session]) -> Result<(), SweepError>; }`; `Registry::sweeper(...)`; `refresh_all` runs sweepers after its own pass; a sweeper's error is logged to stderr by `txc`, never fails the refresh | hooks (stuck `working` demotion), chat (watchdog for unfinished chat ops), history (pending mirrors) | 3 |
| `Session.ext` read helper | `Session::ext<T: DeserializeOwned>(&self, plugin: &str) -> Result<Option<T>, serde_json::Error>` | every plugin with `ext` state | 1 |

Not added, and why:

- `ls` columns for plugin state. `txc show` prints `ext`; plugins ship their own `<verb> status`
  (as `hook status` does). A column slot would make the CLI's table the integration point.
- A capability or service registry between plugins (D3).
- `EngineAdapter` methods for transcripts, fork or seed (D4).
- New `Session` fields. Main's `group`, `attached_to`, `chats`, `artifact_id`, `nvim_socket`,
  `pid`, `state` all become `ext` entries of the plugin that owns them.

## Phases

Order follows dependency and value: the swarm verbs first, then the features that make a
running swarm legible, then UI, then the long tail. Sizes are main's line counts for the code
being ported, as a budget signal, not a target.

- [ ] **Phase 1 — `tutor`** (independent; proves D1)
  - Crate `poc/plugins/tutor` (`tx-tutor`), plugin id `tutor`.
  - From main: `tutor/` (pack, lessons, progress, project, checks, http, tx_check) and
    `verbs/tutor.rs`. ~1.8k lines.
  - State: `data_dir()/<lang>.json` for progress (replaces `$TX_IDE_HOME/tutor/`); no `ext`.
  - Core additions: plugin data dir, `Session::ext` helper (used by `tx_check` to read other
    plugins' state by name).
  - Verbs: `tutor start <lang>`, `tutor check`, `tutor next`, `tutor prev`, `tutor status`.
    The `tx` check predicate reads `txc`'s records through `core.list_sessions`.
  - Gate: a pack scaffolds into a temp dir, a lesson check passes against a `--exec sleep`
    session, progress survives two concurrent `tutor check` runs.

- [ ] **Phase 2 — `spawn`** (the swarm; proves the launch slot)
  - Crate `poc/plugins/spawn` (`tx-spawn`), plugin id `spawn`.
  - From main: `spawn.rs`, `worktree.rs`, `skills.rs` (materialisation; resolution is already
    `tx-core::profile`), `read_only.rs`, the spawn family of `verbs/sessions.rs` (`spawn`,
    `worker`, `kill`, `archive`, `revive`, `remove`), `service.rs::spawn_worker_with`,
    `worker_launch_command`, `next_worker_name`, `remove_worker_worktree`. ~2.5k lines.
  - State: `ext["spawn"] = {"worktree": path | null, "read_only": bool, "role": [..]}`.
    Worktrees live where main puts them (beside the repo), not under `data_dir()`.
  - Core additions: launch slot. Two decorators in this crate: `ReadOnly` wraps `plan.program`
    in `sandbox-exec` / `bwrap` when `ext["spawn"].read_only` is requested through
    `CreateOptions::env` (`TX_SPAWN_READ_ONLY=1`, the decorator's own key); `Skills` appends
    `WorkspaceAction::Symlink` for each granted skill into the engine's discovery dir.
  - Verbs: `spawn`, `worker`, `kill` (stop + optional worktree removal), `archive` / `revive`
    (lifecycle stays core's; archive = stop + `ext["spawn"].archived_at`, revive = new session
    with `resume` from `ext["chat"]` once phase 3 lands, else from scratch).
  - Gate: `txc worker --repo <git dir> --prompt p` creates a worktree, a session whose `cwd` is
    that worktree and whose record carries the path; `kill --remove` removes both; a read-only
    worker cannot write inside the repo (`touch` fails) but can outside.

- [ ] **Phase 3 — `chat`** (transcripts, history, chat ops, messages; proves `ExtUpdated` and sweep)
  - Crate `poc/plugins/chat` (`tx-chat`), plugin ids `chat` and `history` (one crate, two
    plugins, so `plugins.yaml` can disable chat ops while keeping ingest).
  - From main: `history.rs`, `chat.rs`, `messages.rs`, `verbs/chat.rs`, the `history` and
    `chat ls` verbs of `verbs/listing.rs`, the transcript / rollout / bundle code of
    `engines/claude.rs`, `engines/codex.rs`, `engines/codex_rollout.rs`, the chat-ref capture
    of `hooks.rs::capture_chat_ref`. ~5k lines.
  - State: `ext["chat"] = {"chats": [{"id", "role": "primary|fork|handover|rollover", "since"}]}`;
    `data_dir()/<session-id>/<chat-id>/` for mirrored bundles (main's `history/`); one flock
    per (session, chat) as today.
  - Core additions: `ExtUpdated`, sweep slot.
  - Hooks plugin change (same phase): `hook <event>` also stores the payload's chat id and
    transcript path in `ext["hooks"].chat`; the `history` sink reacts to
    `ExtUpdated{plugin:"hooks"}` on a transition into `waiting` / `idle` and runs the ingest
    detached (`current_exe()` + hidden verb `_history-ingest <id>`). The `chat` sweeper
    finishes abandoned `handover` / `rollover` ops; the `hooks` sweeper demotes a `working`
    session older than `stuck_after_seconds` to `waiting`.
  - Verbs: `fork`, `handover`, `rollover`, `resume`, `history [<session>]`, `chat ls`, hidden
    `_chat-op-finish`, `_chat-op-watch`, `_history-ingest`.
  - Gate: a Claude session's transcript is mirrored byte-for-byte after `txc hook stop`;
    `fork` yields a session whose `ext["chat"]` holds a fork ref filled by the next hook;
    the cross-bind refusal test from main passes; `messages` parses main's fixture transcripts.

- [ ] **Phase 4 — `engine-antigravity`**
  - Crate `poc/plugins/engine-antigravity`, plugin id `engine-antigravity`.
  - From main: the launch half of `engines/antigravity.rs` (Go `flag` grammar, `.agents/`
    files, settings trust). The sqlite conversation-db fork surgery and brain-transcript reader
    go to `tx-chat` as its third engine. ~1.3k lines total, split roughly half and half.
  - Gate: `txc create --engine antigravity` launches with the right argv in a fake-`agy` test;
    `fork` on an antigravity session round-trips through the sqlite surgery on a fixture db.

- [ ] **Phase 5 — `artifacts`** (second store owner)
  - Crate `poc/plugins/artifacts` (`tx-artifacts`), plugin id `artifacts`.
  - From main: `artifact.rs`, `artifact_store.rs`, `artifact_service.rs`, `difflib.rs`,
    `grouping.rs`, `verbs/artifacts.rs` (not `sync`). ~2.9k lines.
  - State: `data_dir()/<id>.json` + `data_dir()/<id>/current<ext>` + `revs/`; the touching
    session's id comes from `TX_SESSION_ID` as today; `ext["artifacts"] = {"bound": id}` for a
    view bound by `artifact open`; `ext["artifacts"].group` carries main's `group` override,
    and `grouping::resolved` reads parent chain + bound artifact exactly as main does.
  - Verbs: `artifact create|modify|group|ls|show|diff|doctor`. `artifact open` waits for
    phase 6 (it spawns an nvim view).
  - Gate: main's artifact fixtures round-trip; `doctor --repair` on a store with an orphan rev
    removes only it; two concurrent `modify` calls produce two contiguous revs.

- [ ] **Phase 6 — `views`** (nvim and tmux seams)
  - Crate `poc/plugins/views` (`tx-views`), plugin id `views`. Owns the extended tmux helper
    (D5): popups, splits, `switch-client`, `display-message`, `-F` queries.
  - From main: `service.rs::spawn_nvim`, `spawn_view`, `focus_envelope`,
    `transportable_command`, `verbs/new_agent.rs`, `verbs/fork_pane.rs`,
    `verbs/artifact_menu.rs`, `artifact_picker.rs`, `session_editor.rs`, `_edit-session` /
    `_edit-tag` / `attach` picker / `start` from `verbs/listing.rs`, `render.rs` (the `ls`
    and picker tables), `playground.rs`, the hidden seams `_tmux-name`, `_pane-info`,
    `focus-envelope`, `_relabel`. The `nvim/` Lua stays where it is and calls `txc`. ~6k lines.
  - State: `ext["views"] = {"kind": "nvim|view|playground", "attached_to": id, "nvim_socket": path}`.
  - Core additions: none. The playground decorator (launch slot) adds the companion's env.
  - Verbs: `nvim`, `view`, `attach`, `start`, `_new-agent`, `_fork-pane`, `_artifact-menu`,
    `_artifact-pick`, `_artifact-go`, `_edit-session`, `_edit-tag`, `_pane-info`, `_tmux-name`,
    `focus-envelope`, `_relabel`, `artifact open` (registered here, delegating to `tx-artifacts`
    as a library).
  - Gate: the existing tmux key bindings (`bin/tmux-*`) work against `txc` with the socket
    swapped; `render` tables match main's fixtures byte-for-byte.

- [ ] **Phase 7 — `timeline`**
  - Crate `poc/plugins/timeline` (`tx-timeline`), plugin id `timeline`. The only ratatui user.
  - From main: `plugins/timeline/*` (~3.7k lines). Rows come from `log.jsonl` (`Created`,
    `Exited`, `Stopped`, `MessageSent`, `ExtUpdated{hooks}` for working / idle spans) and from
    `tx-chat`'s message parser for arrows. The sqlite cache moves to `data_dir()/cache.sqlite`.
  - Verbs: `timeline`, `_timeline-resident`, `health`.
  - Gate: `TestBackend` snapshot tests from main pass with the row source swapped.

- [ ] **Phase 8 — `sync`, `migrate`, `install`** (the long tail)
  - `sync`: crate `poc/plugins/sync`, from `sync.rs`, `storage.rs` (the `Storage` boundary and
    S3). Mirrors `sessions/`, `log.jsonl`, `plugins.yaml` and every `plugins/<id>/` that opts
    in through a settings list. ~1.1k lines.
  - `migrate`: crate `poc/plugins/migrate`, from `migrations.rs`. One-shot import of a
    `$TX_IDE_HOME` v6 tree into `<state_dir>`: records to core fields plus `ext` entries per
    the mapping in phases 2 to 6, `history/` to `plugins/chat/`, `artifacts/` to
    `plugins/artifacts/`, `tutor/` to `plugins/tutor/`. ~1k lines.
  - `install`: `verbs/install.rs`, `verbs/home.rs`, `engines/codex_update.rs` become `txc`
    hidden verbs in a crate `poc/plugins/install` so `install.sh` and `setup/engines/*.sh`
    drive `txc`; `_claude-settings` / `_codex-hooks` are already `hook install`. ~2.2k lines.
  - Gate: `install.sh` against a temp `HOME` and `TX_CORE_HOME` yields a working `txc` with
    hooks registered; `migrate` of a fixture `$TX_IDE_HOME` produces records `txc show` prints
    and `history` lists.

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
