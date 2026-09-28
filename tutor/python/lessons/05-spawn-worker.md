# 05. Delegate to an agent worker

> Needs Claude Code (`claude`). Without it, skip ahead with `tx tutor goto 13`.

## The idea

Responses can carry structured data, not just text. The common format is **JSON** — like
`{"now": "2026-09-27T10:00:00Z"}`. The server tells the client what it is sending with a
**header**: `Content-Type: application/json`. Headers are labelled extra lines of information
sent before the body.

## The tx skill

`tx spawn` with `--prompt` starts a Claude Code **worker** on a task (the default engine is
`claude`). tx gives it its own git **worktree** — a separate copy of your project — so it
cannot trample your files. Worktrees share branches with your project, so asking the worker to
commit on a named branch lets you merge its work later with one command.

The quick way next time: `prefix+n` asks for a name, tags and a prompt, then runs this same
`tx spawn` in the pane's directory and tells the tx-assistant that the agent exists.

## Your task

1. In the shell:

   ```sh
   tx spawn time --tag tutor --cwd "$PWD" --prompt "In server.py add a GET /time route that answers JSON {\"now\": <current UTC time, ISO 8601>} with Content-Type application/json. Keep the existing routes. Commit your change on a new branch named tutor/time."
   ```

2. In the viewer (bottom right), `prefix+t` → `time`, and watch it work. You can type to it like any Claude session —
   ask it *why* it set that header.
3. Carry on when it has committed.

## Check

`tx tutor check` — it looks for a Claude worker tagged `tutor`.
