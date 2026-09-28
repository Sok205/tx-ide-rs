# 03. Sessions: give your server its own home

## The idea

A server should keep running while you work on other things. Right now it is stuck in a pane
of this view. tx can run it in its own **session** — a named, tracked tmux session that
outlives the pane you started it from.

## The tx skill

- `tx spawn NAME --tag TAG --cmd "COMMAND"` starts a tracked session running COMMAND.
- `prefix+t` opens the **picker**: every tx session, fuzzy-searchable. Open it from the
  **viewer** (bottom right) and the session you pick appears *inside the viewer*; the lesson
  and the shell stay where they are. Picking another session later swaps what the viewer
  shows — the session it showed keeps running in the background.
- `tx ls` lists sessions in the shell. `prefix+s` shows every tmux session in a tree.
- `prefix+e` opens a small popup form to rename a session or edit its tags: a `Name` field and
  a `Tags` field (comma-separated). `↑`/`↓` (or Tab) switches field, `Enter` saves, `Esc`
  cancels. The CLI equivalent is `tx tag NAME TAG`.

## Your task

1. Stop the server from lesson 02: in the shell pane (top right) press `Ctrl-C`.
2. In the shell: `tx spawn server --tag tutor --cwd "$PWD" --cmd "$SHELL"`
3. `tx ls` — find `server`.
4. `C-j` down to the viewer, then `prefix+t`, type `serv`, Enter. The viewer now shows the
   `server` session, a shell. Run `python3 server.py` there — from now on this session hosts
   your server.
5. Still in the viewer, press `prefix+e`. The cursor starts in `Name`; press `↓` to move to `Tags` —
   its cursor lands at the end of the existing `tutor` — and type `,http-tutor`. Press `Enter`
   to save (`Esc` would cancel). The CLI equivalent is `tx tag server tutor,http-tutor`.
6. `C-k` back up to the shell. The lesson never left the screen.

From now on `curl localhost:8000/…` talks to the `server` session. To **restart the server**
after you change `server.py`: in the viewer (if it shows another session, `prefix+t` →
`server` first) press `Ctrl-C`, then run `python3 server.py` again.

## Check

`tx tutor check`
