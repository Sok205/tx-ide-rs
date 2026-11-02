---
name: tx-artifacts
description: Use when about to create or update a durable deliverable — a plan, doc, report, question set, a diff for review, or a code tour. The txc artifact workflow; subcommand syntax is in `txc artifact help`.
---

# txc artifact workflow

An artifact is a durable deliverable of one kind — `document`, `diff` or `tour` — recorded with
the agent that created it, with its files under `txc artifact path <id>`. Subcommand syntax is in
`txc artifact help`. The workflow:

- Create once, with a title; each create prints the new artifact's id:
  - `txc artifact create <file> --title "<title>"` — a document from a file (the default kind).
  - `txc artifact create --kind diff --title "<title>" --cwd <worktree> [--base <sha>]` — the
    diff of a checkout against a pinned base (default: the merge-base with the default branch).
  - `txc tour create <stops.lua> --title "<title>" --cwd <worktree>` — a code tour whose stops
    point into the worktree (see the `tx-code-tours` skill). It is an artifact of kind `tour`.
- Revise a document or tour by editing its working copy **in place** under
  `txc artifact path <id>` (`current.<ext>`; a tour's is `current.lua`), then run
  `txc artifact modify <id>` after each meaningful revision — that snapshots a revision and gives
  the log and the timeline their milestone. No scratch copies. (`modify <id> <file>` replaces the
  working copy with another file.)
- A diff is never hand-edited: `txc artifact modify <id>` rewrites its patch from the worktree,
  and so does every `txc artifact open`.
- `txc artifact open <id>` opens the artifact in an nvim view bound to it — a document on its
  file, a diff as a diffview in its worktree, a tour in its worktree with the stops loaded. Use it
  instead of hand-starting an nvim view on the file.
- `txc artifact ls [--kind K] [--session S]` lists artifacts; `txc artifact show <id>` prints the
  metadata and the revision log; `txc artifact diff <id>` diffs the last two revisions.
- A finished deliverable that clutters the prefix+a picker: `txc artifact archive <id>` hides it
  from the picker and `txc artifact ls` (content kept; `unarchive` restores, `ls --archived` shows
  it).
- The store lives under `$TX_DATA_DIR/plugins/artifacts/` — never create or move files there by
  hand; the record is authoritative and `txc artifact doctor` reports what drifted.
