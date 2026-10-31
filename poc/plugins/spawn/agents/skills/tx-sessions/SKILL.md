---
name: tx-sessions
description: Use when about to start or delegate to a txc agent — a worker agent, an nvim view, or an ad-hoc shell view. Conventions for tags, naming, and the worker recipe; flag syntax is in `txc help`.
---

# txc agent and view conventions

Syntax is the CLI's job — `txc help` (and `txc ui view` for views) is authoritative. This file
carries only what the flags cannot express.

There are two things you can start:
- an **agent**: an LLM engine (Claude, Codex) with an agent record, its own worktree, roles and
  state — `txc create`;
- a **view**: anything else the operator looks at — an nvim editor, a shell, a diffview — with no
  agent record of its own — `txc ui view open`.

## Tags

Tags are how the operator finds related agents and views later. Give each one tag naming the work
it belongs to:

```bash
txc ui view open build-watch --kind shell --tag wrangler-p1 --cwd <worktree> -- npm run watch
txc ui view open wrangler-p1-nvim --kind editor --parent "$(txc whoami)" --tag wrangler-p1 --cwd <worktree> -- nvim
```

A view takes the same tag as the agent it belongs to, and `--parent <agent>` ties it to that
agent — that is what makes the pair surface together when the operator filters by scope. `txc ui
view ls` lists the open views; `txc ui view close <name>` closes one. Retag an agent with
`txc tag <agent> <tag>…` (it replaces the list) and rename it with `txc rename <agent> <name>`.

Never tag with the kind (`llm` / `nvim` / `shell`); an agent is an agent and a view has its own
`--kind`.

## Naming and lineage

Names are human-readable and say what the agent is for; the tag does the filtering, not the name.
A name already taken gets a suffix (`name-2` …); `txc create` prints the new agent's id, and
`txc ls` shows the name it got. Lineage is the `--parent` flag: pass your own name, so the
operator sees who started whom.

## Delegating to a worker

To delegate work — coding, scoping, planning, research — the standard recipe is:

```bash
txc create <name> --tag <scope> --cwd <project-root> --parent "$(txc whoami)" \
  --engine claude --model <model> --effort <low|medium|high|xhigh|max> \
  --role COMMON --role DEVELOPER --prompt "<task>"
```

- `--role` is what injects the shared conventions into the worker's system prompt — without it
  the worker sees none of them. `txc create` adds nothing by itself: pass `--role COMMON` first,
  then the worker's role (`DEVELOPER` is the usual one; `ORCHESTRATOR` and `QA` also ship). Roles
  resolve from `$TX_DATA_DIR/roles` (the shipped ones) then `$TX_CONFIG_DIR/roles` (the
  operator's own).
- `--engine` and `--prompt` are required. `--engine` is `claude` or `codex` (`txc engines` lists
  them).
- `--model` and `--effort` are placeholders, not recommendations. Omit either one and the engine
  picks it up from its own configuration (for claude, the `model` in `settings.json`) — which is
  what you want unless the operator named a tier. Pass them only when the operator asked for a
  specific model or effort. Effort is the engine's word: claude takes
  `low|medium|high|xhigh|max`, codex also `none|minimal`. A numeric effort `1`–`5` from a plan
  means `low`, `medium`, `high`, `xhigh`, `max`.
- `--prompt` must be short and single-line — long or special-char-laden prompts crash tmux input.
  Point at an artifact for anything longer.
- Read-only agents are not in the poc yet: tell an investigation in its prompt not to edit. To
  continue an agent's conversation as a new agent, `txc fork <source> --prompt "<task>" --name
  <name>`; the fork gets its own worktree at the source's committed HEAD.

Every agent gets its own txc-owned worktree of the `--cwd` checkout automatically — never create
one yourself.

## Talking to it and ending it

- `txc send <agent> -- <text>` types a message into a running agent, framed as from you.
- `txc ls` lists agents (`--tag T`, `--parent P`, `--json`); `txc show <agent>` prints the record,
  including its `state` (`starting`, `working`, `idle`, `ended`).
- `txc stop <agent>` ends it (`txc respawn <agent>` restarts it with the same record, worktree and
  conversation); `txc archive <agent>` stops it and hides it from `txc ls` once its work is done.
