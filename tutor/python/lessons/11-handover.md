# 11. Handover: a fresh mind with the context

## The idea

Long conversations get slow and muddled. Better: summarize what matters and start fresh.

## The tx skill

- `tx handover SOURCE TASK [NEW_NAME]` — a new session receives a distilled brief of SOURCE's
  chat for TASK, and carries on.
- `tx rollover [SESSION]` — the same, in place: the session continues with a fresh, summarized
  chat (default: the one you are in).

## Your task

1. `tx handover <the test worker> "keep improving test coverage for server.py"`
2. `prefix+t` → the new session. Ask: "What would you test next?" It should know the project.

## Check

`tx tutor check`
