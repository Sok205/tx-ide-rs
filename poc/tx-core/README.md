# tx-core

A plain core for a swarm of interactive agents. Each agent is a normal process inside a detached
session on tx's **own** tmux server (`-S <run>/tmux.sock -f <data>/tmux.conf`). Agents
keep running after the launching CLI exits; attaching shows the agent's native terminal.

Design: `../../docs/superpowers/specs/2026-10-15-tx-core-design.md`.
Plan and the decisions the spec left open: `../../docs/superpowers/plans/2026-10-15-tx-core.md`.

## Crates

| crate | what |
|---|---|
| `tx-core` | the library: `TxCore` and the public operations |
| `tx-engine-claude`, `tx-engine-codex` | one `LlmEngine` each, depending only on `tx-core` |
| `tx-core-cli` | `txc`, a thin CLI that registers the bundled engines |

## Layout

`CoreConfig::directories` names five directories (`Directories { config, data, state, cache,
run }`); the front end resolves them (XDG, `TX_APPNAME`; see `../README.md`), the library never
reads the environment.

```
<data>/sessions/<id>.json   the record; atomic temp-file + rename writes, mode 0600
<data>/log.jsonl            one {"ts","actor","type","msg"} line per mutation, ≤ 512 bytes, O_APPEND
<data>/tmux.conf            written once if absent; `ui install` replaces it
<data>/plugins/<id>/        a plugin's own data (CoreConfig::plugin_data_dir)
<state>/profiles/<id>.md    the combined role text handed to the engine
<state>/locks/              create.lock, <id>.lock  (std::fs::File::lock)
<run>/tmux.sock             the private server (must stay ≤ 100 bytes; set CoreConfig::tmux_socket)
```

`tx_core::migrate::migrate_home` moves a version-1 home (everything in one directory) into them.

Disk is the record. tmux holds only the session option `@tx-id`; `$N` / `%N` targets are looked
up by it on every call and never stored, so a rename behind core's back changes nothing.

## Using the library

```rust
let mut core = TxCore::open(CoreConfig::new(Directories {
    config: "/home/me/.config/tx-ide".into(),
    data: "/home/me/.local/share/tx-ide".into(),
    state: "/home/me/.local/state/tx-ide".into(),
    cache: "/home/me/.cache/tx-ide".into(),
    run: "/tmp/tx-ide.me".into(),
}))?;
core.register_engine(Box::new(tx_engine_claude::Claude))?;
let id = core.create_session(CreateOptions::new(
    "worker-1",
    "/repo",
    LaunchOptions::agent("claude")
        .profile(["dev"])
        .initial_prompt("start with the failing test"),
))?;
core.send_message(&id, "status?", SendOptions::default())?;
let at = core.get_execution_location(&id)?;   // tmux -S <socket> attach -t <session_target>
```

## Adding an engine

```rust
struct Aider;

impl LlmEngine for Aider {
    fn id(&self) -> &str { "aider" }
    // Wires no hooks, so its sessions stay `starting`; see tx-engine-claude for real rules.
    fn on_hook(&self, _: LlmEngineState, _: &LlmEngineHook) -> Option<LlmEngineState> { None }
    fn launch(&self, r: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        let mut args = vec!["--read".into(), r.profile.file().display().to_string()];
        if let Some(model) = r.model { args.extend(["--model".into(), model.into()]); }
        if let Some(prompt) = r.initial_prompt { args.extend(["--message".into(), prompt.into()]); }
        Ok(LaunchPlan::new("aider").args(args))
    }
}

core.register_engine(Box::new(Aider))?;
```

`launch` and `on_hook` are pure. Core writes the profile file, applies the plan's `workspace` actions (relative
to `cwd`, refusing anything that escapes it), then starts `program args…` with
`TX_SESSION_ID`, `TX_TMUX_SOCKET`, the config env, the per-session env and the plan env.

## Guarantees worth knowing

- Names: `[A-Za-z0-9_-]{1,128}`; ids: `[A-Za-z0-9_-]{1,64}` (uuid v4 in practice). Both are
  validated everywhere, including when read back from disk, so a record or lock file can never
  land outside its directory.
- `NameTaken` counts every unarchived record, running or not: a stopped agent keeps its name
  (a new create gets `<name>-2`), an archived one frees it. A tmux session that still holds the
  name also yields `NameTaken`. A respawn whose name an unarchived record took meanwhile fails
  with `NameHeld`; nothing is renamed automatically.
- `archive_session` stops a running session, then hides it from `list_sessions` (unless the
  filter asks for `archived()`); the record, worktree and profile stay, and a respawn clears it.
- `Exited` and `Stopped` are terminal. Reads never touch tmux. `list-sessions` is the only
  liveness signal; any failure of it other than "no server" is `InspectionFailed`, and the
  operation writes nothing.
- Messages are typed verbatim (`send-keys -l`, trailing `;` handled), then `Enter` after a
  300 ms pause. The whole delivery holds `<id>.lock`, so concurrent senders never interleave.
- A corrupt, misnamed or non-UTF-8 record file fails `list_sessions` by file name instead of
  being hidden. FIFOs, directories and sockets named `*.json` are ignored.

## Known limits

- A cooked-mode (line-buffered) recipient drops a single line longer than the tty input
  buffer (1024 bytes on macOS). Interactive agents run their tty raw and are unaffected.
- The tmux command line must stay under roughly 16 kB, so very large `env` values fail.
- Workspace actions are checked lexically; they cannot see symlinks. Adapters are trusted code.
- Deleting `locks/` while agents run breaks mutual exclusion (flock is per inode). Core never
  deletes lock files.

## Verification

From `poc/` (its own Cargo workspace, separate from the `tx` binary):

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Integration tests need `tmux` on `PATH` (they skip otherwise) and run every server on a private
socket under `/tmp`; the user's default server is never touched.
