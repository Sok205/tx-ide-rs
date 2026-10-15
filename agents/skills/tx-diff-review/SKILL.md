---
name: tx-diff-review
description: Use when a code change is ready for the user to read — a diff artifact the user opens as an nvim diffview, durable across review rounds and after the worktree is gone.
---

# Diff review

When a change is ready to read, make it a diff artifact the first time, then refresh it for
each later round:

```bash
SELF=$(tx whoami)
tx artifact create --kind diff --title "$SELF review" --cwd <worktree>   # first round
tx artifact modify <id>                                                   # later rounds: rewrite the patch
tx artifact open <id>                                                     # a bound nvim diffview
```

- The base is pinned at create time to the **merge-base** with the default branch (pass
  `--base <sha>` to choose another). Never base on `main` itself — once main moves ahead,
  every commit you don't have shows as a deletion.
- `open` rewrites the patch and opens `DiffviewOpen <base>` in the worktree; the view inherits
  your session's tags, so the pair surfaces together in the operator's filters.
- The patch under `tx artifact path <id>` outlives the worktree: opened after the worktree is
  gone, tx checks the base out into a temporary worktree, applies the patch, and removes the
  worktree again when the view ends.

For an ad-hoc, non-durable look that leaves no artifact behind, `tx spawn-nvim <name> --tag
<tag> --cwd <worktree> --diff <base>` still works.
