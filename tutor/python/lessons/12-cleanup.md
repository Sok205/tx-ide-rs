# 12. Clean up

## The idea

You built a server with routes, JSON, status codes, POST and tests. That is the core of every
web API.

## The tx skill

- `tx kill NAME` stops a session; its record stays.
- `tx archive NAME` files a finished record away from `tx ls`.
- `tx revive NAME` brings back an exited record whose tmux session is still alive.

## Your task

1. `tx ls` — list what is left (it shows only live sessions).
2. Kill every tutor worker: `tx kill NAME` for each.
3. Archive them: `tx archive NAME` for each.
4. Keep `server` if you like — it is a shell, not a worker.

## Check

`tx tutor check` — then you are done. `tx tutor status` shows your run.
