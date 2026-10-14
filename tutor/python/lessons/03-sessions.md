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
- `prefix+e` opens a small popup form to rename a session or edit its tags and group: a `Name`
  field, a `Tags` field (comma-separated) and a `Group` field. It is modal like vim and opens in
  normal mode: `j`/`k` (or Tab) switch field, `h`/`l` move along the text, `i`/`a`/`A` type,
  `Esc` returns to normal mode, `Enter` saves, `Esc` or `q` in normal mode cancels. The CLI
  equivalent is `tx tag NAME TAG`. `Group` is the explicit effort-group override; left empty it
  shows, in cyan italics, the automatic group tx derives from the tags, the parent session or
  the name.

## Your task

1. Stop the server from lesson 02: in the shell pane (top right) press `Ctrl-C`.
2. In the shell: `tx spawn server --tag tutor --cwd "$PWD" --cmd "$SHELL"`
3. `tx ls` — find `server`.
4. `C-j` down to the viewer, then `prefix+t`, type `serv`, Enter. The viewer now shows the
   `server` session, a shell. Run `python3 server.py` there — from now on this session hosts
   your server.
5. Still in the viewer, press `prefix+e`. The cursor starts in `Name`; press `j` to move to
   `Tags`, then `A` to type at the end of the existing `tutor`, and type `,http-tutor`. Press
   `Enter` to save (`Esc` twice would cancel). The CLI equivalent is
   `tx tag server tutor,http-tutor`.
6. `C-k` back up to the shell. The lesson never left the screen.

From now on `curl localhost:8000/…` talks to the `server` session. To **restart the server**
after you change `server.py`: in the viewer (if it shows another session, `prefix+t` →
`server` first) press `Ctrl-C`, then run `python3 server.py` again.

## With the tx-assistant

From the shell pane, `prefix+/`, one request at a time:

- Step 2: "spawn a shell here named server tagged tutor"
- Step 3: "list my tx sessions"
- Step 5: "tag server with tutor and http-tutor"

Opening `server` in the viewer (`prefix+t`) and starting `python3 server.py` there stay yours.

## Check

`tx tutor check`
