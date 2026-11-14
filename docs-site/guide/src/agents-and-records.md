# Agents and records

An agent is a coding-agent CLI process tx started in a worktree it made, with a record it keeps.
This page covers the record, its state, the lifecycle, and what goes into a launch.

## The record

Every agent has one file, `<data>/agents/<id>/agent.json`, and it is the truth. An
[`Agent`](../internal/tx_core/agent/struct.Agent.html) holds the `id` (a uuid), the `name` (the
tmux session name), the `engine` (`claude` or `codex`), the `roles`, the `working_directory` (its
worktree), the `state`, timestamps (`last_modified_at` on every write, `last_activity_at` only
when the state moves), `tags`, `parent_id` (ancestry only), and
`ext`: plugin- and engine-owned data that core never reads.

tmux holds only the link: the session option `@tx-id` is the record id. Session and pane ids are
looked up through it on every call and never stored, so a rename in tmux breaks nothing. Writes
are atomic under the agent's lock, and every mutation appends a line to `<data>/log.jsonl`.

## State

`state` is an [`LlmEngineState`](../internal/tx_core/state/enum.LlmEngineState.html):
**starting** (the CLI has not reported yet), **idle** (the operator's move: the turn finished or
stopped on an approval or a question), **working**, or **ended**. There is no separate lifecycle
field: an agent is running while its state is not `ended`, which lasts until a respawn.

An idle agent may also say why, in the record's `idle_reason`
([`IdleReason`](../internal/tx_core/state/enum.IdleReason.html)): **finished** (the turn ended)
or **waiting** (the turn stopped mid-way on the operator: an approval, a question, a plan to
accept). The field exists only while the state is `idle`; core drops it in every save that moves
the state elsewhere. Missing on an idle record, it means not known (an older record, an interrupt)
and reads as finished. `state` alone keeps meaning "the operator's move", so a reader that does
not care why needs no change. `tx show` and `tx ls --json` carry `idle_reason`; the `tx ls` text
column stays the single token `idle`, so it splits on whitespace as before. A `state-changed`
line reads `working → idle (waiting)`.

The vocabulary is core's; the rules are the engine adapter's. At launch the adapter wires the
CLI's own hooks to run `txi hook <Event>`. When one fires inside the session, core asks the
adapter's `on_hook` for an [`LlmEngineChange`](../internal/tx_core/state/struct.LlmEngineChange.html)
(a new state, why when it is idle, and new engine data, all optional) and writes it under the
agent's lock. For Claude, `SessionStart`, `Stop` and `StopFailure` mean idle, finished;
`PermissionRequest` and a permission-prompt or elicitation `Notification` mean idle, waiting
(even with permissions skipped, `AskUserQuestion` and `ExitPlanMode` stop on the operator);
`UserPromptSubmit` and the tool hooks mean working; `SessionEnd` means ended, unless its reason
is `clear` or `resume`. Codex is similar: `Stop` and `Interrupt` mean finished, and a
`PreToolUse` of its `request_user_input` question tool means waiting (read from Codex's source,
not yet seen live). Core owns what an adapter cannot see: a new agent starts `starting`, and a
session that vanished reads `ended` after a refresh.

## Lifecycle

Each operation is a method on [`TxCore`](../internal/tx_core/core/struct.TxCore.html) and a
built-in `tx` verb:

- **create** (`tx create NAME --cwd DIR --engine E --prompt T [--role R]…`): worktree, profile,
  launch plan, tmux session, record. A failure undoes every earlier step: no orphans.
- **fork** (`tx fork SOURCE --prompt T`): a new agent that continues the source's conversation in
  a branch, in a new worktree at the source's committed `HEAD`; the source keeps running.
  prefix+f forks the agent in the active pane.
- **respawn** (`tx respawn TARGET`): starts an ended agent again in the same record, worktree and
  conversation (the engine's `resume`).
- **stop** (`tx stop TARGET`): types the engine's exit command (`/exit`, `/quit`), waits, and
  kills what is left after a grace period.

- **gc** (`tx gc [--older-than 1d] [--dry-run]`): deletes the build output named in
  `worktree.build_outputs` (default `[target]`) in the worktrees of agents ended longer ago than
  the threshold, and prints the size freed per worktree and in total. The end time is the
  record's `last_modified_at`, which is never earlier than the end. A worktree a running agent
  uses, or one outside `<data>/worktrees`, is skipped. A configured path that is a symlink or
  lies under one (never followed), or under which git tracks a file, is left alone with a
  warning. A path that cannot be deleted is warned about and the rest go on; `gc` then exits 1.
  The worktree, branch, record and conversation stay, so a respawn works and rebuilds.

There is no archive: an ended agent is finished, `tx ls --ended` lists it, and a respawn brings
it back. There is no refresh verb either: `tx ls` and `tx show` first mark a record whose tmux
session is gone as ended, then read.

`tx interrupt`, `tx rename` and `tx tag` round it out.

## Worktrees

Every agent runs in its own worktree of the checkout it was given:
`<data>/worktrees/<repo-key>/<repo>--<name>`, made with `git worktree add --detach … HEAD`. The
`<repo-key>` is the repository's name plus eight hex digits of a hash of its git common
directory, so same-named repositories stay apart. The agent's name is the first of `name`,
`name-2`, … `name-99` not held by a running record. A `--cwd` that is not a git checkout fails
with git's message. The worktree is detached; the agent makes its own branch. Its build output
(`target/` for Rust) stays after the agent ends until `tx gc` deletes it.

## Roles, profiles and skills

A **role** is a Markdown file, `<NAME>.md`, whose text becomes part of the agent's system prompt.
A **profile** is the list of roles an agent was launched with, resolved into one text and written
to `<state>/profiles/<id>.md`. Roles resolve from `<data>/roles` (the ones tx ships, written by
`tx ui install`) and then `<config>/roles` (yours, overriding a shipped role of the same name);
every directory's `<NAME>.local.md` extends the base. A role may open with frontmatter granting
**skills** (`skills: [tx-sessions, tx-artifacts]`); each resolves to a `skills/<name>/` directory
under `<data>/definitions` (the shipped ones) or `<config>/definitions` (yours, which win), which
the engine links into the worktree (`.claude/skills/` or `.agents/skills/`).

Roles are the prompt text several agents share; the personas are agent definitions (below). The
shipped roles are **COMMON** (be concise, introduce yourself, change agents and views only through
`tx`, read the message envelopes, use artifacts for deliverables), which every shipped definition
names in its `roles`, and **TX-ASSISTANT**, which the assistant runs on. `tx create` runs the
`--role`s given; without any, it runs `config.yaml`'s `agent.roles`, which by default is empty:
the definition's `roles`.

## Agent definitions

An **agent definition** is `agents/<name>.md` under `<data>/definitions` (shipped) or
`<config>/definitions` (yours, which win by name): Claude's agent format, reduced to `name`,
`description`, `tools` (required: the built-in tools, in Claude's names), `mcpServers`,
`disallowedTools`, `skills` and `roles`, with the prompt as the body. `roles` names the shared
prompt layers (such as COMMON) the agent runs on when the launch names none; their text comes
before the body, and their skill grants join `skills`, each skill once. Tools and MCP servers come
only from the definition.
`mcp.json` beside it holds the MCP servers in Claude's `.mcp.json` format, and `skills/` the
skills. `tx definitions [agents|skills|mcp]` lists them, and `tx definitions tools` the names `tools`
can use: Claude's built-ins (read once per Claude version from a headless launch's `system/init`
against a dead API address, so no model call; cached under `<cache>/tools/`), the Claude names
Codex switches off, and each MCP server's tools (asked through `codex app-server`); `tx check` validates them, and
`tx check --engine claude|codex` adds what that engine adds to each agent or cannot remove.

`tx create NAME --agent A [--skill S]... [--mcp M]... [--tool T]...` runs definition `A`: its
prompt after any `--role` text, its skills plus the `--skill` extras, its tools plus the `--tool`
extras, and exactly its MCP servers plus the `--mcp` extras — nothing from the engine's own
config. The record keeps `definition: {agent, skills, mcp_servers, tools}`, and every launch
resolves it again, so a respawn keeps the extras and picks up an edited definition. With `--agent`
the default roles of `config.yaml` do not apply: a launch without `--role` runs the definition's
`roles` (resolved again at every launch, like the rest of it), and `--role` replaces them. Without it, the launch runs `config.yaml`'s
`agent.definition` (default `developer`). The shipped definitions are **developer** (short chat
with reasoning in artifacts, full variable names, branch in your worktree, no PR or merge unasked;
the worker tools and the tx skills), **qa** (verifies one PR in a sandbox and reports evidence),
**orchestrator** (runs a multi-team programme, never writes code) and **claude** (plain Claude
Code); the first three run on COMMON. `definition: null` leaves roles alone with the engine's base
tools.
A record from before definitions adopts the default at its next respawn. A secret an MCP server names (`${NAME}`) is taken
from the environment of the `tx` that launches the agent (create or respawn) and set in the
session; when it is unset the launch is refused, on both engines.

## Engines

An [`LlmEngine`](../internal/tx_core/engine/trait.LlmEngine.html) is a pure function from a
launch request to a [`LaunchPlan`](../internal/tx_core/engine/struct.LaunchPlan.html): program,
arguments, environment and workspace actions (such as skill symlinks). Core applies the plan; the
adapter does no I/O, so it is testable without tmux. The kinds are a closed set, `claude` and
`codex`; model and effort are launch options each adapter validates.

## The conversation

Each adapter records the conversation in its own `ext` key (`ext.claude`, `ext.codex`):
`conversation_id` and `transcript_path`, reported by the `SessionStart` hook. Core asks the engine
to archive the transcript under `<data>/agents/<id>/archive/`, so a respawn can restore it even
after the CLI has cleaned it up. Readers ask core: `TxCore::conversation(id)` returns what the
engine names, and `tx chat ls <agent>` prints it.
