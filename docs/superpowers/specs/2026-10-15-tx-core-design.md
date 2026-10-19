# tx-core — a plain core for a swarm of interactive agents

Status: design, 2026-10-15. Concept only, no code. Replaces the Cordis-based core-poc design
(branches `feat/plain-core`, `feat/live-state`). Grounded in `tx-ide-rs` `main` @ `dbb3386`;
every module below names the file it comes from.

## Purpose

Run and manage a swarm of interactive agents without a tx UI. Each agent runs as a normal process
inside a detached tmux session on tx's own tmux server. Agents keep running after the launching
CLI exits. Attaching shows the agent's native terminal; detaching leaves it running.

Adding a new coding-agent engine must be easy: one small trait, one registration call.

## Non-goals

- (Superseded 2026-10-16 by `2026-10-16-poc-plugins-design.md`: the library hosts plugins —
  `plugin.rs` — and engine activity is the `hooks` plugin's.)
- Swarm scheduling, task assignment, retries, review workflows, the tx-assistant coordinator.
- Worktree creation and sandboxing: core accepts a prepared working directory.
- Dashboards, Neovim, tmux layouts/themes, views, artifacts, transcripts and chat history.
- A durable mailbox, delivery retries, or proof that an agent read a message.
- Declarative engines from config files (possible later as one more adapter).

## Architecture

`TxCore` is one struct built once by `TxCore::open(config)`. It owns its collaborators as plain
fields — this is today's `SessionService` without `app.rs::plug_core`.

```
TxCore
├── store      SessionStore      <state_dir>/sessions/<id>.json
├── tmux       Tmux              always -S <state_dir>/tmux.sock -f <state_dir>/tmux.conf
├── events     EventLog          <state_dir>/log.jsonl
├── engines    EngineRegistry    id → Box<dyn EngineAdapter>
├── profiles   Profiles          role + skill search dirs
└── locks      Locks             <state_dir>/locks/
```

The Rust library exposes the operations. A thin CLI exposes the same capabilities to users,
scripts and agents. The library never parses arguments and never reads process environment;
`CoreConfig` carries everything.

```rust
pub struct CoreConfig {
    pub state_dir: PathBuf,                  // default ~/.tx-core
    pub tmux_socket: Option<PathBuf>,        // default <state_dir>/tmux.sock
    pub env: Vec<(String, String)>,          // base environment for every session
    pub profile_dirs: Vec<PathBuf>,          // searched in order; later dirs override
    pub skill_dirs: Vec<PathBuf>,
}
```

## Modules

| Module | From tx-ide-rs | Kept | Left out of core |
|---|---|---|---|
| `session` | `session.rs` | id, name, parent, tags, cwd, timestamps, lifecycle, launch | `group`, `attached_to`, `chats`, `artifact_id`, `nvim_socket`, `pid`, activity states |
| `core` | `service.rs` `SessionService` | the public operations below | nvim/view spawns, worker worktrees, archive/revive, artifacts, focus envelopes |
| `tmux` | `tmux.rs` | new/has/kill/rename session, `@tx-id`, send-keys, list-sessions | views, switch-client, splits, labels, buffers, status messages |
| `messaging` | `SessionService::deliver`, `messages::build_envelope` | envelope, send-keys, pause, `Enter` | transcript parsing (the rest of `messages.rs`) |
| `profile` | `roles.rs`, `skills.rs` | role resolution, `user` override / `.local` extend, skill grants | bundled role content (loaded as data) |
| `engine` | `engines/adapter.rs`, `engines/registry.rs` | `EngineAdapter` (2 methods), registry | resume/fork/seed/distiller/transcripts (optional `EngineChatOps`); `RegistrationId` |
| `store` | `store.rs` | `<id>.json`, atomic temp-file + rename writes | — |
| `reconcile` | `reconcile.rs` | vanished → `Exited` | stuck-`WORKING` demotion, launch-script sweep |
| `events` | `events.rs` | `log.jsonl`, one `O_APPEND` write per line | — |

Also outside core: `worktree.rs`, `chat.rs`, `history.rs`, `plugins/timeline`, `render.rs`,
`tutor/`, `sync.rs` / S3 storage, `hooks.rs`, `grouping.rs`, `migrations.rs`.

## Where state lives

| Home | What |
|---|---|
| disk | the record — the snapshot every read returns |
| tmux | the link only: session option `@tx-id` = record id |
| looked up on demand | execution targets `#{session_id}` (`$N`) and `#{pane_id}` (`%N`), found by `@tx-id`; never stored, so a rename in tmux cannot break them |

```rust
pub struct Session {
    pub id: SessionId,
    pub name: String,
    pub parent: Option<SessionId>,     // ancestry only; never schedules or stops children
    pub tags: Vec<String>,
    pub cwd: PathBuf,
    pub created_at: f64,
    pub ended_at: Option<f64>,
    pub lifecycle: Lifecycle,
    pub launch: Launch,
}

pub enum Lifecycle { Running, Exited, Stopped }   // Exited = vanished; Stopped = stop_session

pub enum Launch {
    Agent { engine: String, profile: Vec<String>, model: Option<String>, effort: Option<Effort> },
    Process { program: String, args: Vec<String> },
}
```

Engine ids are strings, so a new engine needs no change to `session`. `Exited` and `Stopped` are
terminal: nothing in core moves a record out of them.

## tmux: tx's own server

- Every tmux call goes through one function that prepends `-S <socket>` and `-f <config>`. The
  user's default tmux server is never touched. (`tmux.rs` today only knows `$TMUX`; this is the
  one addition.)
- **Config:** the server starts with `-f <state_dir>/tmux.conf`, never `~/.tmux.conf`, so personal
  bindings and options cannot change agent sessions or break tx. Core writes a minimal file on
  `open` if none exists; the IDE replaces it with its own (generated from `tmux/tx-ide.tmux`).
  `-f` only matters when the server starts; every call passes it so whichever call starts the
  server loads it.
- **Socket path:** `open` fails with `CoreError::SocketPathTooLong` if the socket path exceeds 100
  bytes (the `sun_path` limit is 104 on macOS, 108 on Linux), instead of tmux's opaque error.
  Set `CoreConfig::tmux_socket` to a shorter path when `state_dir` is deep.
- **One server for agents and the IDE:** tmux cannot switch clients or move panes across servers.
  The tx UI (views, nvim, pickers) therefore runs on this same server — `tx` attaches the user to
  it — rather than nesting `tmux attach` inside a pane of the user's default server (two
  prefixes, two status bars). Core only guarantees the server; the UI on it is outside core.
- **Lifetime:** the server starts on the first `create_session` and exits with its last session
  (`exit-empty`). If the server crashes, every agent on it dies; records become `Exited` on the
  next refresh. Same as tx-ide-rs today.
- `$N` / `%N` targets are server-local and reset when the server restarts; they are looked up,
  never stored. Shell users must quote them: `-t '$3'`.
- Session names are exact targets (`=name`), as in `tmux.rs` (Q27).
- `list-sessions -F '#{@tx-id}\t#{session_id}\t#{pane_id}'` is the only liveness signal. "No server
  running" means no live sessions; any other failure is `TmuxError::InspectionFailed`.
  (`Tmux::list_sessions` today returns an empty `Vec` on failure, which would mark every session
  exited — fixed here.)
- `@tx-id` is set at session scope only, never `-g`/`-s`.

## Public operations

```rust
impl TxCore {
    pub fn open(config: CoreConfig) -> Result<Self, CoreError>;
    pub fn register_engine(&mut self, adapter: Box<dyn EngineAdapter>) -> Result<(), CoreError>;

    pub fn create_session(&self, opts: CreateOptions) -> Result<SessionId, CoreError>;
    pub fn list_sessions(&self, filter: &ListFilter) -> Result<Vec<SessionId>, CoreError>;
    pub fn get_session(&self, id: &SessionId) -> Result<Session, CoreError>;
    pub fn resolve(&self, name_or_id: &str) -> Result<SessionId, CoreError>;
    pub fn rename_session(&self, id: &SessionId, name: &str) -> Result<Session, CoreError>;
    pub fn set_session_tags(&self, id: &SessionId, tags: Vec<String>) -> Result<Session, CoreError>;
    pub fn send_message(&self, to: &SessionId, text: &str, opts: SendOptions)
        -> Result<DeliveryReceipt, CoreError>;
    pub fn refresh_session(&self, id: &SessionId) -> Result<RefreshResult, CoreError>;
    pub fn refresh_all(&self) -> Result<Vec<Session>, CoreError>;   // changed records only
    pub fn get_execution_location(&self, id: &SessionId) -> Result<ExecutionLocation, CoreError>;
    pub fn stop_session(&self, id: &SessionId) -> Result<Session, CoreError>;
    pub fn forget_session(&self, id: &SessionId) -> Result<bool, CoreError>;
}

pub struct CreateOptions {
    pub name: String,
    pub cwd: PathBuf,
    pub parent: Option<SessionId>,
    pub tags: Vec<String>,
    pub env: Vec<(String, String)>,
    pub launch: LaunchOptions,
}
pub enum LaunchOptions {
    Agent { engine: String, profile: Vec<String>, model: Option<String>,
            effort: Option<Effort>, initial_prompt: Option<String> },
    Process { program: String, args: Vec<String> },
}
pub struct ListFilter { pub parent: Option<SessionId>, pub tag: Option<String>,
                        pub lifecycle: Option<Lifecycle> }
pub struct SendOptions { pub sender: Option<SessionId> }          // None = the user
pub struct DeliveryReceipt { pub recipient: SessionId, pub pane: String, pub submitted_at: f64 }
pub enum RefreshResult { Unchanged(Session), Exited(Session) }
pub struct ExecutionLocation { pub socket: PathBuf, pub session_target: String, pub pane_target: String }
```

- **Reads** (`list_sessions`, `get_session`, `resolve`) return stored snapshots and never touch
  tmux. `resolve` matches an id first, then a name, preferring a `Running` record.
- **`create_session`**:
  1. Validate the name: `[A-Za-z0-9_-]+`. Mint the id.
  2. `Agent`: resolve the profile, then `engine.launch(&request)` → `LaunchPlan`; apply its
     `workspace` actions. `Process`: use `program` + `args` as given.
  3. `new-session -d -c <cwd>` with `TX_SESSION_ID=<id>`, `TX_TMUX_SOCKET=<socket>`, config env,
     option env, plan env (later wins); then set `@tx-id`.
  4. Save the record (`Running`), log `created`.
  If step 4 fails, the tmux session is killed so no orphan is left.
- **`rename_session`**: `rename-session` on tmux if running, then the record; log `renamed`.
- **`set_session_tags`**: replaces the tag list; log `tagged`.
- **`refresh_session`**: one `list-sessions`; a `Running` record without a live `@tx-id` becomes
  `Exited` with `ended_at`. Never revives. Log `exited`.
- **`stop_session`**: `kill-session`, then `Stopped` + `ended_at`; log `stopped`. The record stays.
- **`forget_session`**: deletes the record only; the tmux session (if any) keeps running and is no
  longer managed. Log `forgotten`. Returns `false` if there was no record.
- **`get_execution_location`**: looks up `$N` / `%N` by `@tx-id`; `NotRunning` if absent.
  Attach with `tmux -S <socket> attach -t <session_target>`. From inside another tmux client:
  `TMUX= tmux -S <socket> attach -t <session_target>`. The CLI's `tx attach <name>` does this.

## Messaging

- Envelope, as `messages::build_envelope` today:
  - `sender: None` → `<from-user>{text}</from-user>`
  - `sender: Some(id)` → `<from-agent session="{sender name}">{text}</from-agent>`
- Core never infers the sender. The CLI passes `$TX_SESSION_ID` as the sender only when `$TMUX`
  names tx's socket; otherwise the sender is the user.
- Delivery: the recipient must be `Running` and live by `@tx-id`, else `NotRunning`. Then
  `send-keys -l <envelope>` to its pane, pause, `send-keys Enter`; log `message-sent → <name>`.
- A receipt means input was submitted. Not read, not completed, not retried.

## Engines as plugins

```rust
pub trait EngineAdapter {
    fn id(&self) -> &str;
    fn launch(&self, req: &LaunchRequest) -> Result<LaunchPlan, EngineError>;
}

pub struct LaunchRequest<'a> {
    pub cwd: &'a Path,
    pub profile: &'a Profile,           // combined role text, its file, granted skill dirs
    pub model: Option<&'a str>,
    pub effort: Option<Effort>,
    pub initial_prompt: Option<&'a str>,
}

#[derive(Default)]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workspace: Vec<WorkspaceAction>,
}

pub enum WorkspaceAction {
    Write { path: PathBuf, content: String },     // relative paths resolve against cwd
    Symlink { src: PathBuf, dst: PathBuf },
}
```

- `launch` is pure: no I/O. Core applies `workspace`, so an adapter is unit-testable without a
  filesystem or tmux.
- `register_engine` refuses a duplicate id (`EngineTaken`); `create_session` with an unknown id
  fails with `UnknownEngine`.
- Each engine is its own crate (`tx-engine-claude`, `tx-engine-codex`) depending only on
  `tx-core`. The CLI registers the bundled ones in `main`. Their bodies come from today's
  `build_launch_command` + `prepare_workspace` in `engines/claude.rs` and `engines/codex.rs`.
- Resume, fork, seed, distiller and transcripts move to an optional `EngineChatOps` trait owned by
  the chat feature, outside core.

A complete engine plugin:

```rust
struct Aider;

impl EngineAdapter for Aider {
    fn id(&self) -> &str { "aider" }
    fn launch(&self, r: &LaunchRequest) -> Result<LaunchPlan, EngineError> {
        let mut args = vec!["--read".into(), r.profile.file().display().to_string()];
        if let Some(model) = r.model { args.extend(["--model".into(), model.into()]); }
        if let Some(prompt) = r.initial_prompt { args.extend(["--message".into(), prompt.into()]); }
        Ok(LaunchPlan { program: "aider".into(), args, ..Default::default() })
    }
}

core.register_engine(Box::new(Aider))?;
```

## Profiles

From `roles.rs` / `skills.rs`, with search dirs from `CoreConfig` instead of fixed `Home` paths:

- A profile is a list of role names. Each name resolves to `<dir>/<NAME>.md`; a later dir
  replaces an earlier one, `<NAME>.local.md` extends it. Contents are concatenated in order.
- Frontmatter `tx: { skills: [...] }` grants skills; each resolves to `<skill_dir>/<name>/`.
- `Profile` = combined text, a file holding it (`<state_dir>/profiles/<session id>.md`), and the
  skill dirs. Adapters choose how to hand it to their engine.

## Concurrency

tx-ide-rs has no locks today; these are new. `std::fs::File::lock` (Rust ≥ 1.89), lock files in
`<state_dir>/locks/`, no new dependency.

| Lock | Held across | Prevents |
|---|---|---|
| `create.lock` | name check → tmux create → record save; also `rename_session` | two processes taking one name |
| `<id>.lock` | load → modify → save for rename, tags, stop, refresh writes | lost updates |
| `<id>.lock` | the whole `send_message` (envelope, pause, `Enter`) | interleaved keystrokes from concurrent senders |

Lock order is always `create.lock` before `<id>.lock`. Writes re-load the record under the lock.

## Events

`events.rs` unchanged: one line per mutation, `{"ts","actor","type","msg"}`. Types: `created`,
`renamed`, `tagged`, `message-sent`, `exited`, `stopped`, `forgotten`. `actor` is the sender or
the caller's session id, else empty. An operation log, not a conversation archive.

## Errors

One `thiserror` enum:

```rust
pub enum CoreError {
    NotFound(String), NameTaken(String), InvalidName(String), NotRunning(SessionId),
    UnknownEngine(String), EngineTaken(String), SocketPathTooLong(PathBuf),
    Profile(RoleError), Engine(EngineError), Store(StoreError), Tmux(TmuxError), Lock(io::Error),
}
```

`TmuxError::InspectionFailed` is never read as "exited": the operation fails and writes nothing.

## Verification

`cargo check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`. Integration tests use a
temp `state_dir`, so each gets its own tmux socket.

1. **Swarm:** register a test engine defined in the test file; create two agents (its program is
   a shell stub) and one `Process`; exchange messages; drop `TxCore`; `open` again from a new
   process → same sessions `Running`; `tmux -S <socket> attach -t <session_target>` reaches the
   right pane while the others keep running.
2. **Concurrent create:** N processes create one name → exactly one succeeds, the rest
   `NameTaken`, one tmux session.
3. **Concurrent updates:** parallel tag and rename → no update lost.
4. **Per-recipient serialization:** parallel sends to one session → every envelope arrives whole,
   none interleaved.
5. **Reconcile:** kill a session behind core's back → `refresh_session` gives `Exited`; again →
   `Unchanged`.
6. **Inspection failure:** unusable socket path → `InspectionFailed`; record files byte-identical.
7. **Stop / forget:** stop → `Stopped`, tmux session gone, record kept; forget → record gone, tmux
   session still running.
8. **Isolation:** the user's default tmux server has no new sessions after the suite; a
   `~/.tmux.conf` option (test sets `HOME` to a temp dir with one) is not visible on tx's server.
9. **Engine plugin:** duplicate id → `EngineTaken`; unknown id → `UnknownEngine`; `LaunchPlan`
   workspace actions are applied relative to `cwd`.
10. **Socket path:** a `state_dir` whose socket path exceeds 100 bytes → `SocketPathTooLong` from
    `open`, no files created.
