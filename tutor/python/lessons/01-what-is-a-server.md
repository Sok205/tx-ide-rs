# 01. What is a server?

Welcome to tx tutor. Over 12 short lessons you will build a small web server in Python and
learn tx-ide along the way. No networking knowledge needed.

## The idea

A **server** is a program that waits. It sits on a numbered "door" of your computer — a
**port**, like 8000 — and when another program knocks with a question, it answers. Your
browser does this every time you open a page: it asks a server, the server answers.

## The tx skill

You are inside a tx **view** called `tutor-python`: a tmux session with two panes.
Left: this lesson, in nvim. Right: a shell in your project, `~/tx-tutor/python`.

- `C-l` moves right, `C-h` moves left (also `C-j` / `C-k` for down / up). The same keys
  move between nvim splits, tmux panes and nested sessions — you never need to think about
  which one you are in.

## Your task

1. Press `C-l` to go to the shell. Run `ls` — you will see `server.py`.
2. Press `C-h` to come back here.
3. In nvim, `:e server.py` opens the file (`:b#` brings you back to this lesson). Skim it —
   you do not need to understand it yet.

## Check

In the shell: `tx tutor check`, then answer `y`.
