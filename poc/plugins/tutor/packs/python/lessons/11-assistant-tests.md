# 11. Tests, via the tx-assistant

> Needs Claude Code (`claude`). Without it, skip ahead with `tx tutor goto 13`.

## The idea

Checking by hand with `curl` gets old. A **test** starts the server, sends requests and asserts
the answers automatically. Python ships `unittest`; `python3 -m unittest` finds files named
`test*.py` and runs them.

## The tx skill

So far you have asked the **tx-assistant** (`prefix+/`) for one tx command at a time. This
lesson runs the whole loop through it — spawn, review — and you type only `git` and the tests.

## Your task

1. From this project's shell pane press `prefix+/` and type:
   "spawn a worker here tagged tutor to write test_server.py with unittest covering /hello, a
   404, and POST then GET /notes; commit on a new branch named tutor/tests"
2. Watch it: in the viewer, `prefix+t` → the new worker.
3. When it has committed, `prefix+/` → "spawn nvim here named review-tests tagged tutor with a
   diff of main...tutor/tests", open it in the viewer, then `git merge tutor/tests` in the shell.
4. Run `python3 -m unittest -v` yourself.

## Check

`tx tutor check`
