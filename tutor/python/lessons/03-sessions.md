# 03. Sessions: give your server its own home

## The idea

A server should keep running while you work on other things. Right now it is stuck in a pane
of this view. tx can run it in its own **session** — a named, tracked tmux session that
outlives the pane you started it from.

## The tx skill

- `tx spawn NAME --tag TAG --cmd "COMMAND"` starts a tracked session running COMMAND.
- `prefix+t` opens the **picker**: every tx session, fuzzy-searchable. Enter jumps to one.
- `tx ls` lists sessions in the shell. `prefix+s` shows them in tmux's tree with names and tags.
- `tx tag NAME TAG` reads or sets the tags of a session (comma-separated).

## Your task

1. Stop the server from lesson 02: go to its pane and press `Ctrl-C`.
2. In the shell: `tx spawn server --tag tutor --cwd "$PWD" --cmd "python3 server.py"`
3. `tx ls` — find `server`.
4. `prefix+t`, type `serv`, Enter. You are now inside the `server` session, seeing its output.
5. Still there, run `tx tag server tutor,http-tutor` to add the tag `http-tutor` (tags are
   comma-separated).
6. `prefix+t` again and jump back to `tutor-python`.

From now on `curl localhost:8000/…` talks to the `server` session. After you change
`server.py`, restart it: jump to `server` and press `Ctrl-C`, then run `python3 server.py`
again.

## Check

`tx tutor check`
