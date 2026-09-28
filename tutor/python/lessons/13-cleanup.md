# 13. Clean up

## The idea

You built a server with routes, JSON, status codes, POST and tests. That is the core of every
web API.

## The tx skill

- `tx kill NAME` stops a session; its record stays.
- `tx archive NAME` files a finished record away from `tx ls`.
- `tx revive NAME` brings back an exited record whose tmux session is still alive.
- `prefix+X`, from inside a session's pane, is the same kill from the keyboard: it asks you to
  confirm ("kill tx session ..."), then runs `tx kill` on it (so the record is marked exited, not
  a raw tmux kill that would leave the store stale).

## Your task

1. `tx ls` — list what is left (it shows only live sessions).
2. Kill every tutor worker: open it in the viewer (`prefix+t`), press `prefix+X`, confirm with
   `y` — the viewer drops back to a shell — or run `tx kill NAME` in the shell.
3. Archive them: `tx archive NAME` for each.
4. Keep `server` if you like — it is a shell, not a worker.

## Check

`tx tutor check` — then you are done. `tx tutor status` shows your run.
