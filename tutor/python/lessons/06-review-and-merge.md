# 06. Review the diff, then merge

## The idea

`curl -i localhost:8000/time` still says the welcome text. Why? The worker changed *its own
copy* of the project, not yours. Code reaches your project only when you **merge** it. Before
merging, you review it.

## The tx skill

- `tx spawn-nvim NAME --tag TAG --cwd DIR --diff` opens nvim with a side-by-side diff against
  `main`.
- `tx show time` prints the worker's record, including its worktree path (`cwd`).
- In git, the worker's work is a branch you can merge.

## Your task

1. `tx show time` — note the `cwd` (the worktree) and find its branch:
   `git -C <that cwd> branch --show-current`.
2. `tx spawn-nvim review --tag tutor --cwd <that cwd> --diff` then `prefix+t` → `review`.
   Read the change. Close with `:qa`.
3. Back in your project shell: `git merge <branch>`.
4. Restart the `server` session and `curl -i localhost:8000/time`. Look for
   `Content-Type: application/json`.

## Check

`tx tutor check`
