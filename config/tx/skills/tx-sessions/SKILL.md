---
name: tx-sessions
description: Use when about to start or delegate to a tx agent — a worker agent, an nvim view, an ad-hoc shell view, or a saved layout (`tx layout`). Conventions for tags, naming, and the worker recipe; flag syntax is in `tx help`.
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
  --engine claude --model <model> --effort <1-5> \
  --agent developer --prompt "<task>"
```

- `--agent` picks the worker's agent definition: its prompt, tools, skills, and the roles it
  runs on (the shared conventions, COMMON). `developer` is the usual one; `qa` and `orchestrator`
  also ship, and `tx definitions agents` lists every one, the operator's own included. Leave
  `--role` out: the definition names its roles, and any `--role` replaces them.
- `--cwd` and `--prompt` are required. `--engine` is `claude` or `codex` (`tx engines` lists
  them); left out, it is the operator's `agent.engine` in `config.yaml` (default `claude`). Keep
  passing `--agent` explicitly: without it the worker gets the operator's `agent.definition`,
  which need not be the one the task wants.
- `--model` and `--effort` are placeholders, not recommendations. Omit either one and it comes
  from the operator's `agent.model` / `agent.effort` in `config.yaml` (default Opus 5.5, effort
  3; `null` there leaves it to the engine) — which is what you want unless the operator named a
  tier. With `--engine codex`, pass both: the defaults name a Claude model. Pass them only when the operator asked for a
  specific model or effort. Effort is 1–5 (or the engine's own word): `1` low, `2` medium,
  `3` high, `4` xhigh, `5` max for every engine, so a plan's effort passes straight through
  (`--effort 3`); codex also takes the words `none|minimal`.
- `--prompt` must be short and single-line — long or special-char-laden prompts crash tmux input.
  Point at an artifact for anything longer.
- Read-only agents are not in the poc yet: tell an investigation in its prompt not to edit. To
  continue an agent's conversation as a new agent, `tx fork <source> --prompt "<task>" --name
  <name>`; the fork gets its own worktree at the source's committed HEAD.

Every agent gets its own tx-owned worktree of the `--cwd` checkout automatically — never create
one yourself.

## Starting a layout

A layout is a saved arrangement of panes — agents, commands, shells — opened as one view by a
single command. When the operator asks for one ("open my pair layout here"), run exactly one
`tx layout open`; don't rebuild it from `tx create` and `tx ui view open` calls:

```bash
tx layout ls                                   # the operator's layouts
tx layout open pair --cwd <project> --tag <scope>
tx layout open pair --cwd <project> --agent <agent> --tag <scope>   # around an existing agent
```

- `--cwd` is the project the operator means — "here" is the cwd of the pane they are looking at
  (the focus envelope's `pane-path`), not yours.
- Always pass `--tag`. With no scope named, take the tags of the agent or view in focus (`tx ls`,
  `tx ui view ls`); if it has none, or only `agent`, use the project directory's name.
- `--agent` puts an existing agent in the layout's first `agent:` pane instead of a new one (a
  shell if it has ended). Without `--tag`, the layout then takes that agent's tags; with
  neither, the tag is `agent`.
- Inside tx's server, `open` switches the operator's terminal to the view; `--detach` only prints
  the `… is ready:` line. Opening the same layout for the same `--cwd` again shows the view
  already built rather than a second one.
- A failing open (an unknown definition, a bad file) leaves no view and no agent behind, and its
  message names the file or definition: report it, don't retry with other flags.

Each layout is `<config>/layouts/<name>.yaml` (name `[A-Za-z0-9_.-]+`). `tx layout path <name>`
prints the file, existing or not; edit it there to make or change one. To keep a view the
operator arranged by hand, `tx layout save <name> --view <view>` writes it (without `--view`,
the view you run in); it refuses an existing file unless `--force`, so ask before overwriting:

```yaml
panes:
  - agent: developer          # pane 0: the window itself
  - agent: qa                 # model, effort, engine, prompt, tags, name: optional
    model: claude-sonnet-5-5
    effort: 2
    split: right              # every later pane: split right|below …
    of: 0                     # … of an earlier pane, by its position in this list …
    size: 50%                 # … taking N% of it, or a whole number of cells
  - command: nvim .
    split: below
    of: 1
    size: 40%
  - split: below              # no content: a shell
    of: 0
    size: 30%
```

A pane holds one of `agent: <definition>` (a new agent, as `tx create --agent`), `attach: <agent>`
(an existing one; a shell when it is not running) or `command: <words>`; none is a shell.
Commands and shells start in the worktree of the first agent the view shows, else in `--cwd`.
An `agent:` pane's model and effort follow the same chain as `tx create`: the pane's own key,
else the operator's `agent.model` / `agent.effort` in `config.yaml`, else the engine's default.
`save` writes `agent: <definition>` for every nested agent (never `attach:`), the program name
for a command, `{}` for a shell, and no model or effort: add those by hand.

## Talking to it and ending it

- `tx send <agent> -- <text>` types a message into a running agent, framed as from you.
- `tx ls` lists agents (`--tag T`, `--parent P`, `--json`); `tx show <agent>` prints the record,
  including its `state` (`starting`, `working`, `idle`, `ended`).
- `tx stop <agent>` ends it once its work is done (`tx respawn <agent>` restarts it with the same
  record, worktree and conversation); an ended agent leaves `tx ls` (`tx ls --ended` lists it)
  and frees its name.
