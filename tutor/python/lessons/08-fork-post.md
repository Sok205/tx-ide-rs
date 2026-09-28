# 08. POST, and trying two ideas at once

> Needs Claude Code (`claude`). Without it, skip ahead with `tx tutor goto 12`.

## The idea

`GET` asks for something. **POST** sends something — the data travels in the request **body**,
often as JSON. When the server creates something new, it answers **201 Created**.

We want notes: `POST /notes` with `{"text": "buy milk"}` stores a note, `GET /notes` lists them.

## The tx skill

`tx fork SOURCE [NEW_NAME]` starts a new session that carries the whole conversation so far —
a branch of the chat. Use it to try two designs side by side, then keep the better one.

## Your task

1. Ask `time` (send-message or attach) to add notes: `POST /notes` storing `{"text": ...}` in
   memory and answering 201 with the note, `GET /notes` answering the JSON list. Commit on a
   new branch named `tutor/notes-memory`.
2. `tx fork time` — the fork (named `time-fork` by default) starts from the same conversation.
   In the fork, ask for the same feature but storing notes in a `notes.json` file instead.
   Commit on a new branch named `tutor/notes-file`.
3. Review both the lesson-06 way:
   `tx spawn-nvim review-memory --tag tutor --diff main...tutor/notes-memory` and
   `tx spawn-nvim review-file --tag tutor --diff main...tutor/notes-file`, each opened in the
   viewer with `prefix+t`. Then `git merge` the branch you prefer.
   Restart the server (end of lesson 03) and try:
   `curl -i -X POST localhost:8000/notes -d '{"text": "buy milk"}'`

## Check

`tx tutor check`
