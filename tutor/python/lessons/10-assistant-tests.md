# 10. Tests, via the tx-assistant

## The idea

Checking by hand with `curl` gets old. A **test** starts the server, sends requests and asserts
the answers automatically. Python ships `unittest`; `python3 -m unittest` finds files named
`test*.py` and runs them.

## The tx skill

`prefix+/` opens a one-line prompt to the **tx-assistant** — a helper agent that manages tx for
you. It understands "here" and "this session" from where your cursor is.

## Your task

1. From this project's shell pane press `prefix+/` and type:
   `spawn a worker here tagged tutor to write test_server.py with unittest covering /hello, a 404, and POST then GET /notes; commit`
2. Watch it with `prefix+t`. Review and merge its branch (lesson 06).
3. Run `python3 -m unittest -v` yourself.

## Check

`tx tutor check`
