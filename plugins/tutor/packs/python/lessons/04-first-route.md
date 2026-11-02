# 04. Your first route

## The idea

A **route** is "when a request comes for this path, answer like this". Servers usually have
many. When the server does not know a path, the polite answer is status **404 Not Found** —
we get there in lesson 07.

## The tx skill

Editing in nvim, right here, next to the lesson: `:vsplit server.py` opens the file beside this
lesson (`C-h` / `C-l` move between the two halves), find `do_GET`, `:w` saves, `:q` closes it.

## Your task

1. Open `server.py` beside this lesson: `:vsplit server.py`.
2. In `do_GET`, before the welcome line, add: if `self.path == "/hello"`, answer
   `self.send_text(200, "hello\n")` and `return`.
3. Save, restart the server (end of lesson 03), and try `curl -i localhost:8000/hello`.
4. Commit your work, so agents you start later build on it: `git commit -am "hello route"`.

Stuck? `tx tutor hint` prints the snippet.

## With the tx-assistant

Rather edit in a full-size nvim? From the shell pane, `prefix+/` → "spawn nvim here named
editor with server.py open", then in the viewer `prefix+t` → `editor`. The edit itself is still
yours — the assistant runs tx, it does not write your code.

## Check

`tx tutor check`
