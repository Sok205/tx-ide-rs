# 13. Clean up

## The idea

You built a server with routes, JSON, status codes, POST and tests. That is the core of every
web API.

## The tx skill

- `tx stop NAME` (or `tx kill NAME`) ends a session; its record stays, out of `tx ls` and in
  `tx ls --ended`, and its name is free again.
- `tx revive NAME` brings back an exited record whose tmux session is still alive.
- `prefix+X`, from inside a session's pane, is the same kill from the keyboard: it asks you to
  confirm ("kill tx session ..."), then runs `tx kill` on it (so the record is marked exited, not
  a raw tmux kill that would leave the store stale).

## Your task

1. `tx ls` — list what is left (it shows only live sessions).
2. Stop every tutor worker: open it in the viewer (`prefix+t`), press `prefix+X`, confirm with
   `y` — the viewer drops back to a shell — or run `tx stop NAME` in the shell.
3. `tx ls --ended` — the stopped workers are listed there.
4. Keep `server` if you like — it is a shell, not a worker.

## With the tx-assistant

From the shell pane, `prefix+/`:

- Steps 1–2: "stop every tutor worker, keep server"

## Check

`tx tutor check` — then you are done. `tx tutor status` shows your run.
