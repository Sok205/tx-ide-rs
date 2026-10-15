---
name: tx-artifacts
description: Use when about to create or update a durable deliverable — a plan, doc, report, question set, a diff for review, or a code tour. The tx artifact workflow; subcommand syntax is in `tx artifact --help`.
---

# tx artifact workflow

An artifact is a durable deliverable of one kind — `document`, `diff` or `tour` — owned by the
session that created it, with its files under `tx artifact path <id>`. Subcommand syntax is in
`tx artifact --help`. The workflow:

- Create once, with a title:
  - `tx artifact create <file> --title "<title>"` — a document from a file (the default kind).
  - `tx artifact create --kind diff --title "<title>" --cwd <worktree> [--base <sha>]` — the
    diff of a checkout against a pinned base (default: the merge-base with the default branch).
  - `tx artifact create <stops.lua> --kind tour --title "<title>" --cwd <worktree>` — a code
    tour whose stops point into the worktree (see the `tx-code-tours` skill).
- Revise a document or tour by editing its file **in place** under `tx artifact path <id>`,
  then run `tx artifact modify <id>` after each meaningful revision — that stamps the artifact
  as modified and gives the log and the timeline their milestone. No scratch copies, no
  `--changes`. (`modify <id> <file>` replaces the file with another one.)
- A diff is never hand-edited: `tx artifact modify <id>` rewrites its patch from the worktree,
  and so does every `tx artifact open`.
- `tx artifact open <id>` opens the artifact in an nvim view already bound to it — a document
  on its file, a diff as a diffview in its worktree, a tour in its worktree with the stops
  loaded. Use it instead of hand-spawning an nvim companion on the file.
- A finished deliverable that clutters the prefix+a picker: `tx artifact archive <id>` hides it
  from the picker and `tx artifact ls` (content kept; `unarchive` restores, `ls --archived` shows
  it). In the picker, `d` on the row, then `y`, does the same and closes the artifact's view.
- The store lives under `$TX_IDE_HOME/artifacts/` — never create or move files there by hand;
  the record is authoritative and `tx artifact doctor` reports what drifted.
