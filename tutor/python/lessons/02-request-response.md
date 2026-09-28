# 02. Request and response

## The idea

The knock is a **request**. It says *what* it wants (a **method**, usually `GET` = "give me")
and *where* (a **path**, like `/` or `/hello`). The server sends back a **response**: a
**status code** (a number saying how it went — `200` means OK) and a **body** (the content).

`curl` is a tiny program that sends requests from the shell and prints the response.

## The tx skill

The shell pane (top right) and the viewer (bottom right) are ordinary shells — anything you
would do in a terminal works there.

## Your task

1. In the shell pane: `python3 server.py`. It prints `Serving on http://127.0.0.1:8000`.
   The server is now waiting; this pane is busy.
2. You need a second shell: `C-j` down to the viewer.
3. In the viewer: `curl -i localhost:8000/`. `-i` shows the status line and headers too.
   Find `200 OK` and the welcome text.
4. Try `curl -i localhost:8000/anything`. Same answer — the server answers every path the same
   way. We fix that in lesson 04.
5. Leave the server running for now.

## Check

`tx tutor check` — it starts its own copy of your server on a spare port, sends `GET /` and
expects 200.
