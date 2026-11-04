# Plugins catalogue

Every bundled plugin, in the order `tx-core-cli/src/plugins.rs::MANIFEST` lists them, with its
verbs and key. Verbs starting with `_` are internal: key bindings, hooks and popups call them.
`tx plugins` shows which are active; `<config>/plugins.yaml` switches them.

## Engines

**`engine-claude`** registers the Claude Code adapter. It launches a minimal session,
`claude --session-id <tx id> --strict-mcp-config --setting-sources "" --tools
Bash,Read,Edit,Write[,Skill] --append-system-prompt <profile> --dangerously-skip-permissions …`
(or `--resume`, or `--resume … --fork-session` for a fork): the profile is appended to Claude
Code's own system prompt, and no setting source is read, so no user or project settings, CLAUDE.md,
auto-memory, MCP servers, claude.ai connectors, or synced or bundled skills reach the session.
Granted skills are delivered as a per-agent plugin named `tx` (`tx:<skill>`) under
`.claude/tx-plugin/`, passed with `--plugin-dir`. An agent created with `--agent` gets its
definition's tools instead (`--tools <list>`, or `default` for every built-in when the definition
has no list), with `Skill` added when it has skills and `ToolSearch` when it has MCP servers, so
their schemas stay deferred; its `disallowedTools` as `--disallowedTools`; and its MCP servers as
inline `--mcp-config` JSON. It wires Claude's hooks to `txi hook <Event>`. It keeps
`conversation_id`, `transcript_path`, `model` and `effort` under `ext.claude`, archives each
transcript with its sidecar directory, and restores it for a resume when Claude has cleaned it up.

**`engine-codex`** does the same for Codex: `codex [resume ID | fork ID]` with the profile as
`-c developer_instructions=…`, skills under `.agents/skills/`, hooks as `-c hooks.<Event>=…`
overrides. Every launch passes `--disable apps` and `-c mcp_servers.<name>.enabled=false` for each
server in your `config.toml`; an agent definition's servers arrive as `-c mcp_servers.<name>.…`
(a `${NAME}` secret as `env_vars`, `env_http_headers` or `bearer_token_env_var`, never expanded),
its `mcp__<server>__<tool>` cuts as `disabled_tools`, and the built-in tools it leaves out as
switches (`--disable shell_tool`, `-c web_search="disabled"`, …). Codex picks its own conversation id, so it arrives with the first `SessionStart`;
until then a resume or fork is refused. Without a `model` option Codex runs your configured
default.

## The IDE

**`chat`**: `chat ls <agent>` prints the agent's current conversation; `doctor --fix-hooks`
removes the global hooks older tx versions installed; prefix+f forks the agent in the active pane
through `_fork-pane` (a popup asks for the name). Its library holds the transcript readers the
timeline and the home use.

**`ide`** (in `tx-client`): `ui install`, `ui attach` and `ui view open | ls | close`. It publishes
the theme and status line, the navigation keys (`C-h`/`C-j`/`C-k`/`C-l` across nested sessions,
`C-u`/`C-d` to page an agent's scrollback), helper scripts and the nvim runtime every nvim view
gets.

**`labels`**: `whoami` prints the session the terminal runs in. prefix+s refreshes labels and opens
tmux's session tree. Its fragment draws the pane-border label: the nested session's name and tags,
with a green bar while the agent works and the age since its last activity while idle.

**`kill`**: `kill <name>` stops an agent through core or closes a view by name. prefix+X does the
same for the active pane after a confirmation, and refuses the `home` base.

**`picker`**: `pick` is the fzf session picker on prefix+t: running agents and views in one list,
newest activity first, with vim-style normal and search modes, a roles panel on TAB, Ctrl-T to tag
and alt-p to open a pick in a popup. Enter nests the pick into the launching view's pane.

**`edit`**: prefix+e opens a modal form for the session's name, tags and group (`ext.edit`).

**`spawn`**: prefix+n opens the new-agent form (an embedded nvim form): name, tags, project
(defaulting to the pane's directory), prompt, engine, model, effort and role, COMMON always
included. `_new-agent` then creates the agent; the outcome lands on the status line.

**`assistant`**: `assistant warm` starts the `tx-assistant` agent (Claude, `opus`, effort
`medium`, roles COMMON and TX-ASSISTANT, tag `tx-system`) unless it is running, respawning a
stopped one in place under a lock. It runs in a worktree of its own small repository under
`<data>/plugins/assistant/home`. `assistant ask` delivers one line to it; prefix+/ is the key (see
[Messaging](messaging.md)).

**`mockup`**: `mockup start --project DIR | show | wait | stop`. An agent writes a JSON screen
description; a ratatui viewer renders it in a per-agent `mockup-<agent>` view, and `show`
switches the operator there and back. Used while brainstorming, through the `tx-mockup` skill.
No key.

**`tutor`**: `tutor start [LANG]` scaffolds a practice project from a lesson pack and opens a
`tutor-<lang>` view (lesson, cheat sheet, two shells); `check`, `next`, `prev`, `goto N`,
`status`, `hint` and `reset` move through the lessons. The home's `L` starts it. No key.

**`artifacts`**: `artifact create | modify | ls | show | diff | open | path | archive | close | …`
keeps durable, versioned deliverables under `<data>/plugins/artifacts/`. A `document` opens in an
editor view, a `diff` in a diffview view (rebuilt in a temporary worktree when the original is
gone), any other kind through the verb named after it. `group` reads or sets a session's effort
group. prefix+a lists the artifacts an agent touched, plus its parent and children.

**`tour`**: `tour create <stops.lua> --title T` registers a code tour as an artifact of kind
`tour`; `tour open <id>` starts a `tour` view, nvim in the worktree walking the stops as
diagnostics and a quickfix list. No key.

**`timeline`**: `timeline` draws swimlanes of session activity from the transcripts' turn
markers; `timeline health` prints, per agent record, what the timeline can read for it; prefix+T opens a resident timeline view in a popup
(q detaches, it stays warm).

**`home`**: `home` opens the session manager in a popup; prefix+m is its key, and it is the floor
of the `home` view. See [Startup and the home](home.md).

## Retired ids

`plugins.yaml` entries for these are skipped, so an old file keeps loading:

- **`hooks`**: agent state moved into core and the engine adapters (`txi hook`).
- **`history`**: the transcript mirror; core archives conversations through the engines.
- **`menu`**: renamed `home`; an old `menu` entry configures nothing.
