# 08. Ask your agent from the code

> Needs Claude Code (`claude`). Without it, skip ahead with `tx tutor goto 13`.

## The idea

Besides the standard headers like `Content-Type`, a server may send headers of its own. By
convention they start with `X-` — for example `X-Served-By: tx-tutor`, telling the client which
program answered. The body stays the same; the extra line travels in front of it.

## The tx skill

Leader below means `Space` (LazyVim's leader key).

- `tx spawn-nvim NAME --tag TAG --open FILE` starts an nvim that is its own tx session. Only
  such an nvim can talk to agents; this lesson pane belongs to the view, so it cannot.
- `Space a C` queues a question or request about the code under the cursor (or a selection).
  A `? Q1 queued` marker appears at the end of the line; nothing is written to the file.
- `Space a c` opens the review panel: the queued questions, and the live agent chats to send
  them to. `Tab` moves, `Space` (un)selects, `Enter` sends, `q` closes. The agent receives the
  question with its place in the code (`server.py:12`).
- `Space a i`, inside a diff of your working tree (`:DiffviewOpen <commit>`), flips the file to
  **inline** mode: one editable window with the change highlighted in place — handy to fix a
  line of an agent's work. (Branch-vs-branch diffs like `main...tutor/header` have no working
  tree side, so they stay side by side.)
- `Space a n` leaves an `AINote:` comment above the line: a note in the code for an agent to
  pick up later.

## Your task

1. In the shell: `tx spawn-nvim edit --tag tutor --open server.py`, then in the viewer
   `prefix+t` → `edit`. This is your editor from now on; the lesson stays where it is.
2. In `edit`, put the cursor on `def send_text` and press `Space a C`. Type:
   `Add an X-Served-By: tx-tutor header to every response. Commit on a new branch named tutor/header.`
   and press `Enter` to queue it.
3. `Space a c`, make sure `time` is the chat (`Tab` to it if not), `Enter` to send. Watch the
   worker: in the viewer, `prefix+t` → `time`.
4. When it has committed, review it the lesson-06 way:
   `tx spawn-nvim review-header --tag tutor --diff main...tutor/header`, then in the viewer
   `prefix+t` → `review-header`. Close with `:qa`. Then `git merge tutor/header` in the shell.
5. See the change in your own file: in the viewer `prefix+t` → `edit`, run
   `:DiffviewOpen ORIG_HEAD` (the commit before the merge), put the cursor in the diff and
   press `Space a i`. Now it is one editable `server.py` with the new lines highlighted — you
   could fix a line of the agent's work right here. `Space a i` flips back; `:DiffviewClose`.
6. Restart the server (end of lesson 03) and `curl -i localhost:8000/` — look for
   `X-Served-By: tx-tutor`.

Instead of step 2–3 you could have run `tx send-message time "…"` from the shell — but
asking from the code sends the agent *where* you are looking, too.

## With the tx-assistant

From the shell pane, `prefix+/`:

- Step 1: "spawn nvim here named edit tagged tutor with server.py open"
- Step 4: "spawn nvim here named review-header tagged tutor with a diff of main...tutor/header"

Steps 2–3 stay in nvim: `Space a C` / `Space a c` are how you talk to an agent *from the code*.

## Check

`tx tutor check`, then answer `y`.
