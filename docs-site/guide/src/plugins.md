# Plugins catalogue

Every bundled plugin, in the order `tx-core-cli/src/plugins.rs::MANIFEST` lists them, with its
verbs and key. Verbs starting with `_` are internal: key bindings, hooks and popups call them.
`tx plugins` shows which are active; the `plugins:` block of `<config>/config.yaml` switches them.

## Engines

**`engine-claude`** registers the Claude Code adapter. It launches a minimal session,
`claude --session-id <tx id> --strict-mcp-config --setting-sources "" --tools
Bash,Read,Edit,Write,Skill,ToolSearch --append-system-prompt <profile> --dangerously-skip-permissions …`
(or `--resume`, or `--resume … --fork-session` for a fork): the profile is appended to Claude
Code's own system prompt, and no setting source is read, so no user or project settings, CLAUDE.md,
auto-memory, MCP servers, claude.ai connectors, or synced or bundled skills reach the session.
Granted skills are delivered as a per-agent plugin named `tx` (`tx:<skill>`) under
`.claude/tx-plugin/`, passed with `--plugin-dir`. An agent created with `--agent` gets its
definition's `tools` instead of Bash, Read, Edit and Write. Every agent also gets the one baseline,
`BASELINE_TOOLS`: `Skill` (so granted skills are listed) and `ToolSearch` (so MCP schemas stay
deferred); `tx definitions` and `tx check` show it, and nothing else is ever added. The definition's
`disallowedTools` go as `--disallowedTools`, and its MCP servers as inline `--mcp-config` JSON.
`tx check --engine claude` lists what every launch carries that no flag removes. The plugin's
own settings (`plugins.engine-claude.settings.claude_settings` in `config.yaml`) are Claude
settings merged into every launch's `--settings`, under tx's own keys; `~/.claude/settings.json`
is never read. Reference skills (`metadata.provided-by: claude-bundled|claude-synced`, frontmatter only) are
delivered without their text passing through tx. A synced skill is a link in the per-agent plugin
to `~/.claude/skills/synced/<account>/<name>/`. Bundled skills stay off (`disableBundledSkills`)
unless the agent lists one. Claude cannot turn them on one by one, so that turns the whole bundle
on, and `tx check` and `tx create` warn with the known names. The built-in `plugin-authoring`
plugin is off unless listed. Before each launch the plugin marks the worktree trusted in Claude's config file
(`$CLAUDE_CONFIG_DIR/.claude.json`, else `~/.claude.json`), so it never stops on the trust dialog. It wires Claude's hooks to `txi hook <Event>`. It keeps
`conversation_id`, `transcript_path`, `model` and `effort` under `ext.claude`, archives each
transcript with its sidecar directory, and restores it for a resume when Claude has cleaned it up.

**`engine-codex`** does the same for Codex: `codex [resume ID | fork ID]` with the profile as
`-c developer_instructions=…`, skills under `.agents/skills/`, hooks as `-c hooks.<Event>=…`
overrides. Every launch passes `--disable apps`, `-c mcp_servers.<name>.enabled=false` for each
server in your `config.toml`, and switches off the `codex_worktrees` server the TUI adds itself; an agent definition's servers arrive as `-c mcp_servers.<name>.…`
(a `${NAME}` secret as `env_vars`, `env_http_headers` or `bearer_token_env_var`, never expanded;
Codex silently drops an http server whose bearer variable is unset, and an unset `env_vars` entry),
its `mcp__<server>__<tool>` cuts as `disabled_tools`, and the built-in tools it leaves out as
switches (`--disable shell_tool`, `-c web_search="disabled"`, …). Codex has no baseline (skills are
files, not a tool); `tx check --engine codex` lists what cannot be separated or removed. Codex picks its own conversation id, so it arrives with the first `SessionStart`;
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

**`graph`**: `graph` draws the agents as a graph: columns by lineage depth, dashed edges for
lineage, and solid edges for the messages agents (and you, the node "you") sent each other in the
window (`--last`, default 1h), brighter the busier, with the count on the edge. Running agents,
agents active in the window and the ancestors that connect them are shown; `--tag` and `--group`
narrow it, and `g` cycles the group. It is live: each new message travels its edge as a pulse.
prefix+g opens a resident graph in a popup (q detaches, it stays warm; Enter jumps to the
selected agent). `--snapshot` prints one frame as text.

**`home`**: `home` opens the session manager in a popup; prefix+m is its key, and it is the floor
of the `home` view. See [Startup and the home](home.md).

**`layouts`**: `layout ls | open NAME | save NAME | path NAME | check NAME` opens a saved pane layout, one YAML file per
layout under `<config>/layouts/` (see [Files and directories](files.md)), as one view. `ls` lists
the names, and `path NAME` prints the file to edit, existing or not. `check NAME` reads and
validates the file as `open` does, with the same errors, but starts no tmux and no agent: it
prints `NAME ok: N panes` and exits 0, or prints the error and exits 1. It does not check that
the `agent:` definitions exist (`tx definitions agents` lists them). `open NAME [--cwd DIR]
[--agent A] [--tag T]… [--detach]` reads the file's `panes:`, one record per pane in build order:

```yaml
panes:
  - agent: developer          # pane 0: the window itself
  - agent: qa
    model: claude-sonnet-5-5
    effort: 2
    prompt: Review every commit the developer makes.
    split: right
    of: 0
    size: 50%
  - command: nvim .
    split: below
    of: 1
    size: 40%
  - split: below              # nothing chosen: a shell
    of: 0
    size: 30%
```

A pane holds one of `agent: <definition>` (a new agent, as `tx create --agent` starts it),
`attach: <agent>` (an existing agent, a shell when it is not running) or `command: <words>`, and
nothing means a shell. `prompt`, `model`, `effort`, `engine`, `tags` and `name` are valid only next
to `agent:`. An unset model or effort takes the `agent:` block of `config.yaml`, as `tx create`
does (the model only when the engine is the file's), and otherwise the engine's default. An unset
prompt is "Introduce yourself, then wait for instructions." Tags are `--tag`, else the tags of the
`--agent` agent, else `agent`; the view gets the same tags. Every pane after the first has `split`
(`right` or `below`), `of` (the index of an earlier pane) and `size` (`N%` of the pane it splits,
or a whole number of cells). An unknown key, an agent key on another pane, or an `of` that is not
an earlier pane is an error naming the file.

`open` starts the agents first, with `--agent` shown in the first `agent:` pane in place of a new
one. It then opens the view `<layout>-<project>` and splits each later pane off its `of`, with
commands and shells in the first shown agent's worktree. A failure closes the view and stops the
agents it started. Opening the same layout for the same `--cwd` again shows the view already built.
Inside tx's server it switches your terminal to the view; with `--detach` it only prints the
`… is ready:` line; from a terminal outside it attaches. No key.

`save NAME [--view V] [--force]` goes the other way: it writes the current window of a view you
arranged by hand to `<config>/layouts/NAME.yaml` and prints `<view> saved to <path>`. V defaults
to the session you run it in, inside tx's server, and must be a view. An existing file is refused
unless `--force`. A nested agent becomes `agent: <its definition>` (never `attach:`, so the layout
works in any project); a nested session that runs no agent definition is saved as a shell, with a
`tx layout: pane … saved as a shell` warning on stderr. A command becomes `command: <program
name>`, a shell `{}`, and each pane's `split`,
`of` and `size` (always `N%`) come from the panes' rectangles, so the layout keeps its proportions
on any terminal size. No model or effort is written; add them by hand.

## Retired ids

`config.yaml` entries for these are skipped, so an old file keeps loading:

- **`hooks`**: agent state moved into core and the engine adapters (`txi hook`).
- **`history`**: the transcript mirror; core archives conversations through the engines.
- **`menu`**: renamed `home`; an old `menu` entry configures nothing.
