---
name: tx-sessions
description: Use when about to start or delegate to a tx agent — a worker agent, an nvim view, or an ad-hoc shell view. Conventions for tags, naming, and the worker recipe; flag syntax is in `tx help`.
---

# tx agent and view conventions

Syntax is the CLI's job — `tx help` (and `tx ui view` for views) is authoritative. This file
carries only what the flags cannot express.

There are two things you can start:
- an **agent**: an LLM engine (Claude, Codex) with an agent record, its own worktree, roles and
  state — `tx create`;
- a **view**: anything else the operator looks at — an nvim editor, a shell, a diffview — with no
  agent record of its own — `tx ui view open`.

## Tags

Tags are how the operator finds related agents and views later. Give each one tag naming the work
it belongs to:

```bash
tx ui view open build-watch --kind shell --tag wrangler-p1 --cwd <worktree> -- npm run watch
tx ui view open wrangler-p1-nvim --kind editor --parent "$(tx whoami)" --tag wrangler-p1 --cwd <worktree> -- nvim
```

A view takes the same tag as the agent it belongs to, and `--parent <agent>` ties it to that
agent — that is what makes the pair surface together when the operator filters by scope. `tx ui
view ls` lists the open views; `tx ui view close <name>` closes one. Retag an agent with
`tx tag <agent> <tag>…` (it replaces the list) and rename it with `tx rename <agent> <name>`.

Never tag with the kind (`llm` / `nvim` / `shell`); an agent is an agent and a view has its own
`--kind`.

## Naming and lineage

Names are human-readable and say what the agent is for; the tag does the filtering, not the name.
A name already taken gets a suffix (`name-2` …); `tx create` prints the new agent's id, and
`tx ls` shows the name it got. Lineage is the `--parent` flag: pass your own name, so the
operator sees who started whom.

## Delegating to a worker

To delegate work — coding, scoping, planning, research — the standard recipe is:

```bash
tx create <name> --tag <scope> --cwd <project-root> --parent "$(tx whoami)" \
  --engine claude --model <model> --effort <low|medium|high|xhigh|max> \
  --role COMMON --role DEVELOPER --prompt "<task>"
```

- `--role` is what injects the shared conventions into the worker's system prompt — without it
  the worker sees none of them. `tx create` adds nothing by itself: pass `--role COMMON` first,
  then the worker's role (`DEVELOPER` is the usual one; `ORCHESTRATOR` and `QA` also ship). Roles
  resolve from `$TX_DATA_DIR/roles` (the shipped ones) then `$TX_CONFIG_DIR/roles` (the
  operator's own).
- `--cwd` and `--prompt` are required. `--engine` is `claude` or `codex` (`tx engines` lists
  them); left out, it is the operator's `agent.engine` in `config.yaml` (default `claude`). Keep
  passing `--role` explicitly: without it the worker gets the operator's `agent.roles`, which
  need not be the ones the task wants.
- `--model` and `--effort` are placeholders, not recommendations. Omit either one and it comes
  from the operator's `agent.model` / `agent.effort` in `config.yaml` (default Opus 5.5, effort
  high; `null` there leaves it to the engine) — which is what you want unless the operator named a
  tier. With `--engine codex`, pass both: the defaults name a Claude model. Pass them only when the operator asked for a
  specific model or effort. Effort is the engine's word: claude takes
  `low|medium|high|xhigh|max`, codex also `none|minimal`. A numeric effort `1`–`5` from a plan
  means `low`, `medium`, `high`, `xhigh`, `max`.
- `--prompt` must be short and single-line — long or special-char-laden prompts crash tmux input.
  Point at an artifact for anything longer.
- Read-only agents are not in the poc yet: tell an investigation in its prompt not to edit. To
  continue an agent's conversation as a new agent, `tx fork <source> --prompt "<task>" --name
  <name>`; the fork gets its own worktree at the source's committed HEAD.

Every agent gets its own tx-owned worktree of the `--cwd` checkout automatically — never create
one yourself.

## Talking to it and ending it

- `tx send <agent> -- <text>` types a message into a running agent, framed as from you.
- `tx ls` lists agents (`--tag T`, `--parent P`, `--json`); `tx show <agent>` prints the record,
  including its `state` (`starting`, `working`, `idle`, `ended`).
- `tx stop <agent>` ends it once its work is done (`tx respawn <agent>` restarts it with the same
  record, worktree and conversation); an ended agent leaves `tx ls` (`tx ls --ended` lists it)
  and frees its name.
