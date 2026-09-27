# 05. Delegate to an agent worker

## The idea

Responses can carry structured data, not just text. The common format is **JSON** — like
`{"now": "2026-09-27T10:00:00Z"}`. The server tells the client what it is sending with a
**header**: `Content-Type: application/json`. Headers are labelled extra lines of information
sent before the body.

## The tx skill

`tx spawn` with `--prompt` starts a Claude Code **worker** on a task (the default engine is
`claude`). tx gives it its own git **worktree** — a separate copy of your project on its own
branch — so it cannot trample your files.

## Your task

1. In the shell:
   ```
   tx spawn time --tag tutor --cwd "$PWD" --prompt "In server.py add a GET /time route that answers JSON {\"now\": <current UTC time, ISO 8601>} with Content-Type application/json. Keep the existing routes. Commit your change."
   ```
2. `prefix+t`, jump to `time`, and watch it work. You can type to it like any Claude session —
   ask it *why* it set that header.
3. Jump back here when it has committed.

## Check

`tx tutor check` — it looks for a Claude worker tagged `tutor`.
