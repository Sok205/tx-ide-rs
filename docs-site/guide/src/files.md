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

## What lives where

**config** holds what you write by hand: `config.yaml` (the defaults for `tx create` in its
`agent:` block, the build output `tx gc` may delete in `worktree:`, and which plugins are on,
with their settings, in `plugins:`), `roles/` (your own
roles, never pruned, overriding a shipped role of the same name), `definitions/` (your own
`agents/` and `skills/`, each overriding a shipped one of the same name) and `layouts/` (one `<name>.yaml` per
saved pane layout, opened by `tx layout open <name>`; `tx layout path <name>` prints the file,
and `tx layout save <name>` writes one from a view).

**data** holds everything durable tx writes:

```text
agents/<id>/agent.json      the agent's record
agents/<id>/archive/        the engine's archived transcripts
log.jsonl                   one {"ts","actor","type","msg"} line per mutation
worktrees/<repo-key>/…      the agents' git worktrees; `tx gc` empties their build output
plugins/<id>/               each plugin's own data (artifacts, home, assistant, tutor, …)
roles/                      shipped roles and skills, written by `tx ui install`
bin/ keys/ conf/ nvim/      plugin resources, written by `tx ui install`
tmux.conf                   the server's config, written by `tx ui install`
```

`log.jsonl` is a notification trail, not the record: each line is at most 512 bytes and written
with a single append, so `tail -f` never sees a torn line. On an event, re-read the agent with
`tx show`.

**state** holds `locks/` (`create.lock` and one `<id>.lock` per agent, taken with `flock`) and
`profiles/<id>.md`, the combined role text handed to each agent's engine.

**cache** holds nothing yet.

**run** holds `tmux.sock`, tx's server. The directory is mode 0700, and the socket path must
stay at or under 100 bytes, within the Unix socket path limit.
