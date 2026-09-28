# 07. 404, and asking your agent

## The idea

Status codes come in families: `2xx` success, `3xx` "look elsewhere", `4xx` "your request is
wrong" (like **404 Not Found**), `5xx` "the server broke". A good server says 404 for paths it
does not know instead of pretending everything is fine.

## The tx skill

- Attach to a worker (`prefix+t`) and just talk to it — it is a normal Claude session.
- `tx send-message time "…"` drops a message into a worker without leaving your pane.

## Your task

1. `tx send-message time "Explain HTTP status code families in three lines. Do not change code."`
   Then, from this lesson pane, `prefix+t` → `time` to read the answer. Come back:
   `prefix+s` → `tutor-python`.
2. Yourself, in nvim: make `do_GET` answer `self.send_text(404, "not found\n")` for any path
   it does not handle. Keep `/`, `/hello`, `/time` working.
3. Restart `server`; `curl -i localhost:8000/nope` should say `404`.

Stuck? `tx tutor hint`.

## Check

`tx tutor check`
