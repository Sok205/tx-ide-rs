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
   Then, in the viewer, `prefix+t` → `time` to read the answer.
2. Yourself: `:vsplit server.py` in this lesson pane, then make `do_GET` answer
   `self.send_text(404, "not found\n")` for any path it does not handle. Keep `/`, `/hello`,
   `/time` working.
3. Restart the server (end of lesson 03); `curl -i localhost:8000/nope` should say `404` and
   `curl -i localhost:8000/hello` still `200`.
4. Commit it: `git commit -am "404 for unknown paths"`.

Stuck? `tx tutor hint`.

## Check

`tx tutor check`
