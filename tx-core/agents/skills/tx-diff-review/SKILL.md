---
name: tx-diff-review
description: Use when a code change is ready for the user to read — a diff artifact the user opens as an nvim diffview, durable across review rounds and after the worktree is gone.
---

# Diff review

When a change is ready to read, make it a diff artifact the first time, then refresh it for
each later round:

```bash
SELF=$(txc whoami)
txc artifact create --kind diff --title "$SELF review" --cwd <worktree>   # first round; prints the id
txc artifact modify <id>                                                   # later rounds: rewrite the patch
txc artifact open <id>                                                     # a bound nvim diffview view
```

- The base is pinned at create time to the **merge-base** with the default branch (pass
  `--base <sha>` to choose another). Never base on `main` itself — once main moves ahead,
  every commit you don't have shows as a deletion.
- `open` rewrites the patch and opens a view of kind `diff` running `DiffviewOpen <base>` in the
  worktree; `--tag <tag>` gives the view your scope tag, so the pair surfaces together in the
  operator's filters.
- The patch under `txc artifact path <id>` outlives the worktree: opened after the worktree is
  gone, txc checks the base out into a temporary worktree and applies the patch.
  `txc artifact close <id>` removes that temporary worktree once the view is done.

For an ad-hoc, non-durable look that leaves no artifact behind, open a view yourself:
`txc ui view open <name> --kind diff --parent "$(txc whoami)" --tag <tag> --cwd <worktree> -- nvim -c "DiffviewOpen <base>"`.
