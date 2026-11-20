# Plugins catalogue

Every plugin tx ships, with its verbs and key. Each is a directory under `<data>/shipped/` with
a manifest and its program; `tx` links none of them. Verbs starting with `_` are internal: key
bindings, hooks and popups call them. `tx plugins` lists the installed ones in plugin order (by
id) with their status; the `plugins:` block of `<config>/config.yaml` switches them. Which of
them an install has is decided by tier: the preset `headless` installs tier `core`, `ide` adds
tier `ide`, and `full`, the default, adds tier `extra`.

| Plugin | Tier | Program |
|---|---|---|
| `engine-claude` | core | `tx-engine-claude` |
| `engine-codex` | core | `tx-engine-codex` |
| `artifacts` | core | `tx-artifacts` |
| `chat` | core | `tx-chat` |
| `kill` | core | `tx-kill` |
| `labels` | core | `tx-labels` |
| `setup` | core | `tx-setup` |
| `assistant` | ide | `tx-assistant` |
| `edit` | ide | `tx-edit` |
| `graph` | ide | `tx-graph` |
| `home` | ide | `tx-home` |
| `home-themes` | ide | none: hosted by `tx-home` |
| `ide` | ide | `tx-ide` |
| `layouts` | ide | `tx-layouts` |
| `picker` | ide | `tx-picker` |
| `spawn` | ide | `tx-spawn` |
| `timeline` | ide | `tx-timeline` |
| `mockup` | extra | `tx-mockup` |
| `tour` | extra | `tx-tour` |
| `tutor` | extra | `tx-tutor` |

## Engines

Each engine is the program `tx-engine-<kind>` of its plugin. `tx` starts it when a command needs
the engine (a create, a fork, a respawn, a stop) and talks to it over the engine protocol; the
same program is the hook command of every agent it launches (`tx-engine-<kind> --tx-hook <Event>`),
so a hook is one process.

What an engine adds to the definitions is the engine's to say, not `tx`'s. The `# claude adds to
every agent's tools: …` line of `tx harness ls` and `tx harness check` is the manifest's `notes`, printed
without starting the program. The rest is asked of each active engine, over the protocol for a
bundle: the bundled-skill warning of `tx harness check` and `tx create --agent` (`warnings`); the
`<kind>: always present:` lines, the problems and the per-agent notes of `tx harness check --engine
<kind>` (`check`); and each engine's section of `tx harness ls tools`, with the `mcp <server>:`
sections of the first engine that can list MCP tools (`tools`; here Codex, through
`codex app-server`). A disabled engine prints nothing there, and `tx harness check --engine` refuses it.

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
deferred); `tx harness ls` and `tx harness check` show it, and nothing else is ever added. The definition's
`disallowedTools` go as `--disallowedTools`, and its MCP servers as inline `--mcp-config` JSON.
`tx harness check --engine claude` lists what every launch carries that no flag removes. The plugin's
own settings (`plugins.engine-claude.settings.claude_settings` in `config.yaml`) are Claude
settings merged into every launch's `--settings`, under tx's own keys; `~/.claude/settings.json`
is never read. Reference skills (`metadata.provided-by: claude-bundled|claude-synced`, frontmatter only) are
delivered without their text passing through tx. A synced skill is a link in the per-agent plugin
to `~/.claude/skills/synced/<account>/<name>/`. Bundled skills stay off (`disableBundledSkills`)
unless the agent lists one. Claude cannot turn them on one by one, so that turns the whole bundle
on, and `tx harness check` and `tx create` warn with the known names. The built-in `plugin-authoring`
plugin is off unless listed. Before each launch the plugin marks the worktree trusted in Claude's config file
(`$CLAUDE_CONFIG_DIR/.claude.json`, else `~/.claude.json`), so it never stops on the trust dialog. It wires Claude's hooks, through `--settings`, to its own program. It keeps
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
files, not a tool); `tx harness check --engine codex` lists what cannot be separated or removed. Codex picks its own conversation id, so it arrives with the first `SessionStart`;
until then a resume or fork is refused. Without a `model` option Codex runs your configured
default.

## The IDE

**`chat`**: `chat ls <agent>` prints the agent's current conversation; `doctor --fix-hooks`
removes the global hooks older tx versions installed; prefix+f forks the agent in the active pane
through `_fork-pane` (a popup asks for the name). Its library holds the transcript readers the
timeline and the home use. Tier `core`; its verbs run as the program `tx-chat`
(`plugins/chat/plugin.yaml`).

**`ide`** (`plugins/ide`, package `tx-ide`): its updater (run by `tx update`), `ui attach` and
`ui view open | ls | close`. Tier `ide` (it needs `tmux` and `nvim`); its program is `tx-ide`
(`plugins/ide/plugin.yaml`): its verbs run as that program and `tx update` runs its updater as
`tx-ide --tx-update`.
The updater composes the server config, `<data>/tmux.conf`: it runs the key fragments
`tx update` published under `<data>/keys` (`ide`'s first, the others by name), names the `tx`
binary (`@tx-bin`, and `TX_BIN` in the server's environment, which a plugin program started from
a key runs tx with), and reloads a running server, unbinding what a fragment that is gone had
declared. It writes no resources: `tx update` publishes them itself, the `ide`'s like
every plugin's. It publishes the theme and status line, the navigation keys (`C-h`/`C-j`/`C-k`/`C-l` across nested sessions,
`C-u`/`C-d` to page an agent's scrollback), helper scripts and the nvim runtime every nvim view
gets.
When the plugin is disabled, or leaves the shipped plugins, `tx update` retires it: the same
program, told `TX_PLUGIN_RETIRE=1`, writes core's default `tmux.conf` back, takes out of a running
server every key, option and hook recorded in `<data>/keys/.installed`, and removes the record
(see [The tmux layer and views](tmux-and-views.md#tx-update-composing-the-configuration)).
`ui attach` hands the `home` view's pane to the `home` plugin through its verb `_home-install`,
which from `tx-ide` runs the program `tx-home`.

**`labels`**: `whoami` prints the session the terminal runs in. prefix+s refreshes labels and opens
tmux's session tree. Its fragment draws the pane-border label: the nested session's name and tags,
with a green bar while the agent works and the age since its last activity while idle.
Tier `core`; its program is `tx-labels` (`plugins/labels/plugin.yaml`). The
fragment asks `_label-program` once, when the server's configuration loads, and has tmux run that
program for the border label directly, not through `tx`.

**`kill`**: `kill <name>` stops an agent through core or closes a view by name. prefix+X does the
same for the active pane after a confirmation, and refuses the `home` base.
Tier `core`; its program is `tx-kill`.

**`picker`**: `pick` is the fzf session picker on prefix+t: running agents and views in one list,
newest activity first, with vim-style normal and search modes, a roles panel on TAB, Ctrl-T to tag
and alt-p to open a pick in a popup. Enter nests the pick into the launching view's pane.
Tier `ide` (it requires `ide` and needs `fzf`); its program is `tx-picker`. The picker's own verbs that fzf runs as children (the reload once a
second, the key handler, the roles) run `tx-picker` directly, one process each, not through `tx`;
`kill`, `_edit-tag` and the popup of alt-p start with `tx`.

**`edit`**: prefix+e opens a modal form for the session's name, tags and group (`ext.edit`).
Tier `ide` (it requires `ide` and needs `fzf`); its program is `tx-edit`.

**`spawn`**: prefix+n opens the new-agent form (an embedded nvim form): name, tags, project
(defaulting to the pane's directory), prompt, engine, model, effort and role, COMMON always
included. `_new-agent` then creates the agent; the outcome lands on the status line.
Tier `ide` (it requires `ide` and needs `nvim`); its program is `tx-spawn`. The form's engine list is every active engine.

**`assistant`**: `assistant warm` starts the `tx-assistant` agent (Claude, `opus`, effort
`medium`, roles COMMON and TX-ASSISTANT, tag `tx-system`) unless it is running, respawning a
stopped one in place under a lock. It runs in a worktree of its own small repository under
`<data>/plugins/assistant/home`. `assistant ask` delivers one line to it; prefix+/ is the key (see
[Messaging](messaging.md)).
Tier `ide`; it runs as the program `tx-assistant` (`plugins/assistant/plugin.yaml`), which
starts the assistant through the Claude engine's program (`engine-claude`).

**`mockup`**: `mockup start --project DIR | show | wait | stop`. An agent writes a JSON screen
description; a ratatui viewer renders it in a per-agent `mockup-<agent>` view, and `show`
switches the operator there and back. Used while brainstorming, through the `tx-mockup` skill.
Its verbs run as the program `tx-mockup` (`plugins/mockup/plugin.yaml`).
No key.

**`tutor`**: `tutor start [LANG]` scaffolds a practice project from a lesson pack and opens a
`tutor-<lang>` view (lesson, cheat sheet, two shells); `check`, `next`, `prev`, `goto N`,
`status`, `hint` and `reset` move through the lessons. The home's `L` starts it. No key.
Tier `extra`; it ships as a bundle (`plugins/tutor/plugin.yaml`) and runs as the program
`tx-tutor`, which carries the lesson packs itself: they are materialised under
`<data>/plugins/tutor/packs` and published as no resource.

**`artifacts`**: `artifact create | modify | ls | show | diff | open | path | archive | close | …`
keeps durable, versioned deliverables under `<data>/plugins/artifacts/`. A `document` opens in an
editor view, a `diff` in a diffview view (rebuilt in a temporary worktree when the original is
gone). Any other kind is opened by the plugin whose verb is named after it, as `<kind> open <id>`:
a `tour` by `tour`, run as that plugin's program where it is a bundle. `group` reads or sets a
session's effort group. prefix+a lists the artifacts an agent touched, plus its parent and
children. Tier `core`. Its verbs run as the program `tx-artifacts` (`plugins/artifacts/plugin.yaml`).

**`tour`**: `tour create <stops.lua> --title T` registers a code tour as an artifact of kind
`tour`; `tour open <id>` starts a `tour` view, nvim in the worktree walking the stops as
diagnostics and a quickfix list; `artifact open` and prefix+a run it for a tour. No key. Tier
`extra`. Its verb runs as the program `tx-tour` (`plugins/tour/plugin.yaml`).

**`timeline`**: `timeline` draws swimlanes of session activity from the transcripts' turn
markers; `timeline health` prints, per agent record, what the timeline can read for it; prefix+T opens a resident timeline view in a popup
(q detaches, it stays warm). On a lane, as prefix+a: Enter focuses the pane showing its agent,
a picks a pane to attach the agent into (h/j/k/l, arrows, a pane number or a click, under a gold border, starting
on the pane prefix+T was pressed in; Enter attaches, Esc cancels), p opens it in a popup.
Tier `ide`. Its verbs run as the program `tx-timeline` (`plugins/timeline/plugin.yaml`).

**`graph`**: `graph` draws the agents as a graph: columns by lineage depth, dashed edges for
lineage, and solid edges for the messages agents (and you, the node "you") sent each other in the
window (`--last`, default 1h), brighter the busier, with the count on the edge. Running agents,
agents active in the window and the ancestors that connect them are shown; `--tag` and `--group`
narrow it, and `g` cycles the group. It is live: each new message travels its edge as a pulse.
From 130 columns a sidebar shows the selected agent. Keys act through `tx`: `s` sends a line as
you, `f` forks, `x` stops (after `y`), `r` respawns an ended agent, `a` opens its artifacts, Enter
jumps to its pane; `?` lists them. prefix+g opens a resident graph in a popup (q detaches, it
stays warm). `--snapshot` prints one frame as text. Tier `ide`. It ships as a bundle
(`plugins/graph/plugin.yaml`): its verbs run as the program `tx-graph`.

**`home`**: `home` opens the session manager in a popup; prefix+m is its key, and it is the floor
of the `home` view. See [Startup and the home](home.md).
Tier `ide` (it requires `ide` and needs `git`); it ships as a bundle (`plugins/home/plugin.yaml`)
and runs as the program `tx-home`, which also hosts `home-themes`.

**`home-themes`**: the animated backdrops of the home, chosen by `settings.theme` (default
`koi`); `b` in the home cycles them, and the choice is kept in `<data>/plugins/home-themes/theme`.
Tier `ide` (it requires `home`). Its bundle is a manifest alone
(`plugins/home-themes/plugin.yaml`): it has no program of its own. The home's program `tx-home`
hosts it and registers it when it is installed and enabled; without it the home is plain. No
verb, no key.

**`layouts`** (tier `ide`; program `tx-layouts`):
`layout ls | open NAME | save NAME | path NAME | check NAME` opens a saved pane layout, one YAML file per
layout under `<config>/layouts/` (see [Files and directories](files.md)), as one view. `ls` lists
the names, and `path NAME` prints the file to edit, existing or not. `check NAME` reads and
validates the file as `open` does, with the same errors, but starts no tmux and no agent: it
prints `NAME ok: N panes` and exits 0, or prints the error and exits 1. It does not check that
the `agent:` definitions exist (`tx harness ls agents` lists them). `open NAME [--cwd DIR]
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

**`setup`**: `setup --bundles DIR [--preset NAME | --plugins a,b,c | --upgrade] [--list]` chooses
which of the shipped plugins an install takes: by preset (`headless`, `ide`, `full`, or one of
yours in `<config>/presets/`), by id, or on a terminal from a checklist. It records the result in
`<data>/shipped/.selection`, which `tx update --bundles` lays out, and seeds `config.yaml` when
there is none (see [Files and directories](files.md)). It is the program `tx-setup`
(`plugins/setup/plugin.yaml`): `tx` itself carries none of its checklist, and it opens no core. No key.

## Retired ids

`config.yaml` entries for these are skipped, so an old file keeps loading:

- **`hooks`**: agent state moved into core and the engine adapters (`txi hook`).
- **`history`**: the transcript mirror; core archives conversations through the engines.
- **`menu`**: renamed `home`; an old `menu` entry configures nothing.
