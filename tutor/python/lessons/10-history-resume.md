# 10. History: nothing is lost

> Needs Claude Code (`claude`). Without it, skip ahead with `tx tutor goto 13`.

## The idea

A quick detour from HTTP: your conversations with agents are worth keeping. The fork you did
not merge still holds a working design and all the reasoning behind it.

## The tx skill

- `tx history` — every past (exited/archived) session tx has seen, newest first.
- `tx chat ls NAME` — the chats behind one session.
- `tx resume TARGET` — re-spawn a past session and reattach its chat.

## Your task

1. Kill the fork you did not merge: `tx kill <its name>`.
2. `tx history` — find it. `tx chat ls <its name>` shows its chats.
3. `tx resume <its name>` and ask it:
   "Summarize the difference between your notes design and the merged one."

## Check

`tx tutor check`
