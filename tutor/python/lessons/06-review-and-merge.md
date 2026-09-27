# 06. Review the diff, then merge

> Needs Claude Code (`claude`). Without it, skip ahead with `tx tutor goto 12`.

## The idea

`curl -i localhost:8000/time` still says the welcome text. Why? The worker changed *its own
copy* of the project, not yours. Code reaches your project only when you **merge** it. Before
merging, you review it.

## The tx skill

- `tx spawn-nvim NAME --tag TAG --cwd DIR --diff` opens nvim with a side-by-side diff against
  `main`.
- `tx show time` prints the worker's record, including its worktree path (`cwd`).
- The worker committed on a **detached HEAD** in its worktree; `git switch -c` names that commit
  with a branch, which — since worktrees of one repo share branches — you can then merge from
  your project.

## Your task

1. `tx show time` — note the worker's `cwd` (its worktree).
2. `tx spawn-nvim review --tag tutor --cwd <that cwd> --diff` then `prefix+t` → `review`.
   Read the change. Close with `:qa`.
3. Name the worker's commit: `git -C <that cwd> switch -c tutor/time`. (The worker committed on
   a detached HEAD, which has no name to merge; a branch gives it one, and worktrees share
   branches with your project.)
4. In the project shell: `git merge tutor/time`.
5. Restart the `server` session and `curl -i localhost:8000/time`. Look for
   `Content-Type: application/json`.

## Check

`tx tutor check`
