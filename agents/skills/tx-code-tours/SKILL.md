---
name: tx-code-tours
description: Use when the user asks for a tour or walkthrough of code. Investigate the code, then annotate nvim with session-local vim.diagnostic notes and ordered quickfix navigation — as a tour artifact the user opens, or applied into an operator's live nvim. Never offer a tour unprompted.
---

# Code tours with nvim diagnostics

Show reasoning on the code using the shipped `tx-tour` runtime (`nvim/lua/tx-tour.lua` in the
tx-ide nvim config). Do not inline the runtime or use the JSON/`:TourApply` workflow.

## Build the tour

Investigate first. Order stops pedagogically—entry point, dispatch, edge cases, callers/tests—not by filename. Explain why each location matters rather than restating its code.

Write a Lua data file under a scratch directory. Use absolute paths, 1-based lines, labels of about five words prefixed `N/M`, plain-text bodies, and severity `WARN`, `INFO`, or `HINT`:

```lua
return {
  {
    file = "/absolute/path/to/source.ts",
    line = 42,
    label = "1/3 entry point",
    message = [[This boundary chooses the route tree.

Its result controls every downstream match.]],
    severity = "INFO",
  },
}
```

## Deliver the tour as an artifact

The usual way: a tour artifact the user opens whenever they want, in a bound nvim view:

```bash
tx artifact create /tmp/code-tour/stops.lua --kind tour --title "<topic> tour" --cwd <worktree>
tx artifact open <id>
```

The view starts nvim in the worktree at stop 1. To revise the tour, edit the stops file under
`tx artifact path <id>` in place and run `tx artifact modify <id>`.

## Or apply it into a running nvim

When the operator already has an nvim companion open, apply the tour there over RPC instead.
If none exists, read the `tx-sessions` skill and create one through `tx`. Never create or mutate sessions with raw tmux.

Resolve the companion's live tx record UUID, then inspect its pane read-only to find the nvim socket:

```bash
pane_process_id=$(tmux list-panes -st <tx-record-uuid> -F '#{pane_pid}' | head -1)
nvim_process_id=$(pgrep -P "$pane_process_id" | head -1)
socket_path=$(lsof -U -a -p "$nvim_process_id" | awk '{print $NF}' | grep nvim | head -1)
```

Run the bundled helper with the socket and stop-data file:

```bash
python3 <skill-directory>/scripts/apply_tour.py "$socket_path" /tmp/code-tour/stops.lua
```

## Navigating

Either way it opens at stop 1. Tell the operator: `]n` / `[n` navigate, `<leader>cn` opens the note. `<leader>cN` restarts, `<leader>cl` lists stops, and `:TourClear` clears the tour.

Keep all effects session-local. Never write repository files through RPC or globally reconfigure LSP diagnostics. Tours disappear on nvim restart.
