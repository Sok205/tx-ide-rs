# 01. What is a server?

Welcome to tx tutor. Over 13 short lessons you will build a small web server in Python and
learn tx-ide along the way. No networking knowledge needed.

## The idea

A **server** is a program that waits. It sits on a numbered "door" of your computer — a
**port**, like 8000 — and when another program knocks with a question, it answers. Your
browser does this every time you open a page: it asks a server, the server answers.

## The tx skill

You are inside a tx **view** called `tutor-python`: a tmux session with four panes.

- Top left: this lesson, in nvim. It stays on screen the whole tutorial.
- Below it: a cheat sheet of tx-ide keys.
- Top right: a **shell** in your project, `~/tx-tutor/python`, for commands.
- Bottom right: the **viewer**, also a shell for now. From lesson 03 on, sessions you open with
  the picker appear here, so nothing ever covers the lesson.

How keys are written in these lessons:

- `C-l` means hold **Ctrl** and press `l` — one chord, **no prefix** first.
- `prefix+t` means press your tmux **prefix** (`C-b` by default; the cheat sheet shows yours),
  release it, then press `t`.
- Careful: if your tmux config binds `prefix` + `h/j/k/l` to resize panes (a common setup),
  pressing the prefix before `C-h/j/k/l` resizes instead of moving. Just use Ctrl alone.

- `C-l` moves right, `C-h` moves left, `C-j` / `C-k` move down / up (try `C-j` to reach the
  cheat sheet). The same keys
  move between nvim splits, tmux panes and nested sessions — you never need to think about
  which one you are in.
- **Enter on a command in a lesson types it into the shell** (top right) without running it:
  put the cursor on a `` `command` `` or on a line inside a code block and press Enter, then
  `C-l` to the shell, check it, and press Enter there. To copy text instead: `V` to select
  lines, `"+y` to copy to the clipboard, `Cmd+V` to paste.
- Lost (the tutorial is gone from your screen)? `prefix+s`, pick `tutor-python`, Enter — or
  run `tx tutor start` in any shell.

## The tx-assistant

Almost every tx command in these lessons can also be *asked for* in plain words. `prefix+/`
opens a one-line prompt; type a request and press Enter. The **tx-assistant** — a Claude agent
that runs tx for you — does it: spawns agent workers, shells and nvim sessions, tags, forks,
kills. It knows where you pressed `prefix+/`: "here" means that pane's directory, "this session"
the session it shows. Press it from the shell pane (top right), so "here" is your project.

- Its answer lands in its own session: from the viewer, `prefix+t` → `tx-assistant` to read it.
- Every lesson ends with a **With the tx-assistant** section: the same steps, as requests.
  Requests are in "double quotes" — type them into the `prefix+/` prompt, not the shell (Enter
  in the lesson only types `commands`).
- It needs Claude Code (`claude`). Without it, type the commands as shown.

## Your task

1. Press `C-l` to go to the shell. Run `ls` — you will see `server.py`.
2. Press `C-h` to come back here.
3. In nvim, `:e server.py` opens the file (`:b#` brings you back to this lesson). Skim it —
   you do not need to understand it yet.
4. Optional: from the shell pane, `prefix+/` and ask "what tx sessions are running?". Read the
   answer in the viewer: `C-j`, then `prefix+t` → `tx-assistant`.

## Check

In the shell: `tx tutor check`, then answer `y`.
