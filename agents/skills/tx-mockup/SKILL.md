---
name: tx-mockup
description: Use when brainstorming needs to show the operator a screen, layout, or animation — a ratatui mockup rendered in a tx view, in place of the superpowers browser companion. The agent writes a short JSON file; the viewer renders it and switches the operator's terminal there and back.
---

# tx-mockup

The terminal replacement for the brainstorming browser companion. No HTML, no server, no
browser tab: write a JSON description, run `txc mockup show`, and the operator sees it
rendered with ratatui in a view named `mockup-<your-session-name>`.

## When to use

Decide per question. The test: **would the operator understand this better by seeing it than
reading it?**

- **Mockup** when the content itself is visual: TUI screens and layouts, side-by-side layout
  comparisons, animations and splash screens, look-and-feel questions.
- **Terminal** when the answer is words: scope and requirements, conceptual A/B/C choices,
  trade-off lists, technical decisions, clarifying questions.

A question *about* a UI is not automatically visual. "What kind of start menu?" is a terminal
question. "Which of these start menu layouts?" is a mockup.

## The loop

1. **Start** once per brainstorm, after the operator agrees to a visual:
   `txc mockup start --project <project-dir>`. It prints one JSON line,
   `{"view":…,"content_dir":…,"state_dir":…}`. Keep both dirs. Remind the operator to add
   `.superpowers/` to `.gitignore` if it is not there.
2. **Write** `<content_dir>/<semantic-name>.json` with your file tool, then run
   `txc mockup show`. The operator's terminal switches to the view. Give a one-line summary of
   what is on screen and end your turn.
3. **Read** on your next turn: `<state_dir>/events` (JSON lines; `select`, `confirm`,
   `dismiss`) together with what the operator typed. The typed reply is primary; the events are
   the structured pick. An empty or missing events file means they never pressed a key.
4. **Iterate or advance**: feedback on the current screen goes into a new file
   (`layout-v2.json`). Only move on once the current question is settled.
5. **Wait** when the conversation returns to text: `txc mockup wait` puts up a
   "continuing in terminal…" screen and does not switch the operator.
6. **Stop** at the end: `txc mockup stop`. The files stay on disk.

If `show` prints `mockup ready in view <name>` the operator has no attached client. Relay
that line verbatim so they can switch to the view by hand.

**Never reuse filenames.** The viewer shows the newest file by modification time, so each
screen is a fresh file: `layout.json`, `layout-v2.json`, `splash.json`.

## Keys the operator has

`1`–`9` select an option, `←`/`→` or `h`/`l` move between options, `Tab` toggles side-by-side
and fullscreen, `Enter` confirms and switches back, `Esc`/`q` dismisses, `r` restarts animation.

## Format

JSON, keys kept short. A three-option screen should be about 20–60 lines. A file with
`"options"` is a comparison; a file with a top-level `"body"` is a single screen. Never both.

```json
{"title":"Start menu — which layout?","sub":"pick the closest","fps":8,
 "options":[
  {"id":"a","title":"Centered splash","note":"game title screen",
   "body":{"col":[
     {"size":7,"frames":["[green]▀█▀ ▀▄▀[/]","[dim]▀█▀ ▀▄▀[/]"]},
     {"list":["New tx session","Resume","Timeline","Quit"],"sel":0},
     {"block":"recent","body":{"text":"[yellow]wrangler-p1[/] [dim]2h ago[/]"}}]}},
  {"id":"b","title":"Dashboard panes","body":{"row":[
     {"size":"30%","block":"actions","body":{"list":["New tx","Resume"],"sel":0}},
     {"block":"history","body":{"table":[["wrangler-p1","tx-ide-rs","2h"]]}}]}}]}
```

**Screen:** `title`, `sub`, `fps` (default 8), then `options` (each `id`, `title`, optional
`note`, `body`) or `body`.

**Nodes** carry an optional `size` plus exactly one kind:

| kind | shape |
|------|-------|
| `"row": [nodes]` | children side by side |
| `"col": [nodes]` | children stacked |
| `"block": "title", "body": node` | a border with a title around one node |
| `"text": "…"` | lines of text, markup allowed |
| `"list": ["…"], "sel": 0` | items, `sel` highlights one |
| `"table": [["…"]], "head": ["…"]` | rows, optional header |
| `"frames": ["…"], "fps": 4` | one frame per entry, played at `fps` (overrides top-level) |

**Sizes:** `"size": 7` is a fixed length, `"size": "30%"` a share, absent means fill.

**Markup** inside any text: `[green]…[/]`, `[bold]`, `[dim]`, `[on blue]`, combined as
`[bold yellow]`. Named colours plus the tx palette names (`accent`, `warn`, …). `[[` is a
literal `[`.

Keep mockups mostly text art. Two to four options per screen. Scale fidelity to the question:
boxes and labels for layout, colour and animation only when that is what you are asking about.
