# 06. Review the diff, then merge

> Needs Claude Code (`claude`). Without it, skip ahead with `tx tutor goto 13`.

## The idea

`curl -i localhost:8000/time` still says the welcome text. Why? The worker changed *its own
copy* of the project, not yours. Code reaches your project only when you **merge** it. Before
merging, you review it.

## The tx skill

- `tx spawn-nvim NAME --tag TAG --diff BASE` opens nvim with a side-by-side diff.
  `--diff main...tutor/time` shows exactly what the branch changed since it left `main`.
- The worker committed on the branch `tutor/time` in its own worktree. Worktrees share branches
  with your project, so you can review and merge that branch right here.

## Your task

1. In the shell: `git status`. If `server.py` shows as modified (your lesson-04 work), commit it
   first — `git merge` refuses to run over uncommitted changes: `git commit -am "hello route"`.
2. `git branch` — you should see `tutor/time`. (Missing? Ask the worker:
   `tx send-message time "put your commit on a new branch named tutor/time"`.)
3. `tx spawn-nvim review --tag tutor --diff main...tutor/time`, then in the viewer `prefix+t` →
   `review`. Read the change. Close with `:qa`.
4. In the shell: `git merge tutor/time`. If git says **CONFLICT** (you and the worker both
   changed `do_GET`): `:vsplit server.py`, keep both routes, delete the `<<<<<<<` / `=======` /
   `>>>>>>>` marker lines, `:w`, then `git commit -am "merge tutor/time"`.
5. Restart the server (end of lesson 03) and `curl -i localhost:8000/time`. Look for
   `Content-Type: application/json`.

## Check

`tx tutor check`
