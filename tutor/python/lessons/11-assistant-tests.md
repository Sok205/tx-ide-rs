# 11. Tests, via the tx-assistant

> Needs Claude Code (`claude`). Without it, skip ahead with `tx tutor goto 13`.

## The idea

Checking by hand with `curl` gets old. A **test** starts the server, sends requests and asserts
the answers automatically. Python ships `unittest`; `python3 -m unittest` finds files named
`test*.py` and runs them.

## The tx skill

`prefix+/` opens a one-line prompt to the **tx-assistant** — a helper agent that manages tx for
you. It understands "here" and "this session" from where your cursor is.

## Your task

1. From this project's shell pane press `prefix+/` and type:
   `spawn a worker here tagged tutor to write test_server.py with unittest covering /hello, a 404, and POST then GET /notes; commit on a new branch named tutor/tests`
2. Watch it: in the viewer, `prefix+t` → the new worker. Then review and merge it the lesson-06
   way: `tx spawn-nvim review-tests --tag tutor --diff main...tutor/tests`, then
   `git merge tutor/tests`.
3. Run `python3 -m unittest -v` yourself.

## Check

`tx tutor check`
