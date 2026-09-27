# 04. Your first route

## The idea

A **route** is "when a request comes for this path, answer like this". Servers usually have
many. When the server does not know a path, the polite answer is status **404 Not Found** —
we get there in lesson 07.

## The tx skill

Editing in nvim, right here. `:e server.py`, find `do_GET`. `:w` saves.

## Your task

1. Open `server.py` in this nvim (`:e server.py`).
2. In `do_GET`, before the welcome line, add: if `self.path == "/hello"`, answer
   `self.send_text(200, "hello\n")` and `return`.
3. Save, restart the `server` session (lesson 03), and try `curl -i localhost:8000/hello`.

Stuck? `tx tutor hint` prints the snippet.

## Check

`tx tutor check`
