# 08. POST, and trying two ideas at once

## The idea

`GET` asks for something. **POST** sends something — the data travels in the request **body**,
often as JSON. When the server creates something new, it answers **201 Created**.

We want notes: `POST /notes` with `{"text": "buy milk"}` stores a note, `GET /notes` lists them.

## The tx skill

`tx fork SOURCE [NEW_NAME]` starts a new session that carries the whole conversation so far —
a branch of the chat. Use it to try two designs side by side, then keep the better one.

## Your task

1. Ask `time` (send-message or attach) to add notes: `POST /notes` storing `{"text": ...}` in
   memory and answering 201 with the note, `GET /notes` answering the JSON list. Commit.
2. `tx fork time` — the fork (named `time-fork` by default) starts from the same conversation.
   In the fork, ask for the same feature but storing notes in a `notes.json` file instead.
   Commit.
3. Review both (lesson 06), merge the one you prefer, restart `server`, and try:
   `curl -i -X POST localhost:8000/notes -d '{"text": "buy milk"}'`

## Check

`tx tutor check`
