# tx-ide

tx-ide is an IDE in tmux for a swarm of interactive coding agents. Each agent is an engine's own
CLI, Claude Code or Codex, unwrapped: it runs in a git worktree of its own, with a durable record
on disk, inside a detached session on tx's **own** tmux server. It outlives the command that
started it: attaching shows its native terminal, detaching leaves it running, and an ended agent
respawns into the same worktree and conversation. Agents and the operator message each other with
`tx send`. Every action, whether the operator, a key binding or an agent takes it, goes through one
front end: `tx`.

Everything around the agents is a plugin:

- **The home** (`tx ui attach`, prefix+m): projects, their agents and views, live; one key starts,
  enters, stops or resumes an agent.
- **Agent definitions**: `developer`, `qa`, `orchestrator`, `layout-author` and `claude` ship, each
  with its prompt, tools, skills and MCP servers; yours sit beside them and win by name.
- **The graph** (prefix+g): the agents by lineage, every message between them travelling its edge
  live; send, fork, stop, respawn and open artifacts from it.
- **The timeline** (prefix+T): swimlanes of each agent's working and idle time, and of messages.
- **Artifacts** (prefix+a): durable, versioned deliverables: documents, diffs opened in diffview,
  code tours walked in nvim.
- **Layouts**: a window of agents, commands and shells, saved as YAML and opened in any project.
- **The assistant** (prefix+/): a resident agent that knows tx, asked one line from any pane and
  told which pane.
- **Views**: nvim, shells and diffviews beside the agents, on the same server.
- The session picker (prefix+t), the new-agent form (prefix+n), fork (prefix+f), mockups for
  designing a screen with an agent, a tutor, and `tx gc` for the build output of ended agents.

How it all works is in the guide, <https://sok205.github.io/tx-ide-rs/guide/>, next to the tx-core
API docs. `tx help` lists every verb and flag.

## Install

```sh
git clone <this-repo> ~/tx-ide-rs
~/tx-ide-rs/install --dry-run   # print every change, change nothing
~/tx-ide-rs/install
tx ui attach                    # the IDE: the home
```

Needs Rust 1.92+, tmux, git, fzf, curl, at least one engine CLI (`claude`, the default engine and
the assistant's, or `codex`) and `nvim` for views; `install` also needs `jq`. It installs `tx` and
`txi` with cargo, runs `tx ui install` (the IDE's tmux config, its keys, and the shipped roles and
definitions, under the data directory), links `~/.config/nvim` and `~/.tmux.conf` to this
repository's `config/nvim` and `config/tmux.conf` (moving what was there to `<path>.bak.<stamp>`),
and writes `~/.config/tx-ide/config.yaml` with every default spelled out, once. It is safe to
re-run. After rebuilding `tx` by hand, run `tx ui install` again: the tmux config names the
binary's path.

Coming from the previous `tx` (removed on 2026-11-02; its last tree is commit `7607347`),
`install` also switches the machine once: the old install stays runnable as `tx-old`, its hook
entries leave `~/.claude/settings.json` and `~/.codex/hooks.json` after a backup, and
`tx doctor --fix-hooks` removes what older versions wired.

## First steps

The prefix is `C-a`.

```sh
tx ui attach
```

lands on the home, the floor of the `home` view: projects, their agents and views. The first
attach also starts the assistant in the background.

- **Start an agent**: `n` in the home, or prefix+n anywhere, opens the new-agent form; from a
  shell, `tx create NAME --cwd DIR --prompt TEXT`. The agent runs in its own worktree of DIR's
  checkout at `HEAD`, as the `developer` definition unless told otherwise, and makes its own
  branch there.
- **Go in and out**: Enter on an agent nests its terminal into the home's pane. `C-a C-a d`
  detaches the nested agent and the home comes back; the agent keeps running. `C-a d` detaches the
  terminal itself; `tx ui attach` brings everything back as it was.
- **Talk**: type into the agent, or `tx send NAME -- TEXT` from anywhere (run inside an agent, the
  message is signed as that agent). prefix+/ asks the assistant one line from any pane.
- **Watch**: prefix+g the graph of agents and their messages, prefix+T the timeline, prefix+t the
  picker, prefix+a the artifacts an agent touched.
- **Finish**: `x` then `y` in the home, or `tx stop NAME`, ends an agent; Enter on an ended agent,
  or `tx respawn NAME`, resumes it in the same worktree and conversation. `tx gc` deletes the build
  output of agents ended more than a day ago.

## Keys

| keys | does |
|---|---|
| prefix+m | the home in a popup |
| prefix+t | the session picker |
| prefix+n | new agent: the form, then the agent in the pane's directory |
| prefix+f | fork the agent in the pane into a new worktree |
| prefix+e | edit the agent's name, tags and group |
| prefix+X | stop the agent in the pane, or close the view, after a y/n prompt |
| prefix+/ | ask the assistant one line |
| prefix+g | the agent graph |
| prefix+T | the timeline |
| prefix+a | in an agent: the artifacts it touched, its parent and children; in a view: back to its agent |
| prefix+s | refresh the labels, then `choose-tree` |
| prefix+\| / prefix+- | split right / below |
| C-h / C-j / C-k / C-l | move between nvim splits and panes, out of nested sessions too |
| C-u / C-d | page an agent's scrollback |
| C-a C-a d | leave a nested agent or view; it keeps running |

In the home: j/k move, Enter attaches (or resumes an ended agent), → unfolds an agent's detail, n
starts an agent, x stops one, r resumes one, a its artifacts, / asks the assistant, f finds, w the
saved layouts, t the timeline, b the next backdrop, p the plugins, ? every plugin's keys, q closes
the popup. The graph lists its keys on `?`.

## Commands

| verb | does |
|---|---|
| `tx create NAME --cwd DIR --prompt T` | start an agent in its own worktree of DIR's checkout; `--agent`, `--engine`, `--model`, `--effort`, `--tag`, `--parent` and more override `config.yaml` |
| `tx ls [--ended] [--json]` | the running agents, or the ended ones |
| `tx send NAME -- TEXT` | type a message into an agent, in an envelope saying who sent it |
| `tx fork NAME --prompt T` | a new agent continuing NAME's conversation, in its own worktree |
| `tx stop NAME`, `tx interrupt NAME` | end the agent its engine's way; abort its running turn |
| `tx respawn NAME` | start an ended agent again, in the same worktree and conversation |
| `tx show NAME`, `tx where NAME`, `tx attach NAME` | its record as JSON; its tmux socket, session and pane; attach this terminal to it |
| `tx rename NAME NEW`, `tx tag NAME TAG…` | rename it; replace its tags |
| `tx gc [--older-than 1d] [--dry-run]` | delete the build output (`worktree.build_outputs`) in the worktrees of agents that ended longer ago than that |
| `tx definitions`, `tx check` | list and validate the agent definitions, skills and MCP servers |
| `tx engines`, `tx plugins` | the enabled engines; every plugin and whether it is on |

Plugins add `ui`, `home`, `pick`, `kill`, `whoami`, `chat`, `doctor`, `assistant`, `artifact`,
`group`, `tour`, `timeline`, `graph`, `layout`, `mockup` and `tutor`. `tx help` prints every verb
with its flags; verbs starting with `_` are internal, run by key bindings and hooks.

## Agent definitions

An agent's system prompt is its **roles**, prompt text agents share (`COMMON` under every shipped
definition but `claude`, `TX-ASSISTANT` for the assistant), then its **agent definition**: Claude's
agent format, `name`, `description`, `tools`, `mcpServers`, `disallowedTools`, `skills` and
`roles`, with the prompt as the body.

| definition | what it is |
|---|---|
| `developer` | the default: writes code with the operator; short chat, the reasoning in artifacts |
| `qa` | verifies one PR in a sandbox, the way a person would use the product; never edits the repository |
| `orchestrator` | runs a multi-team programme of implementers and QA agents; never writes code |
| `layout-author` | writes one saved layout from a description; the home's layout list starts it |
| `claude` | plain Claude Code: every built-in tool, no prompt of its own |

`tx create NAME --agent qa …` picks one; `--skill`, `--mcp` and `--tool` add to it. The shipped
definitions, the tx skills (`tx-sessions`, `tx-artifacts`, `tx-diff-review`, `tx-code-tours`,
`tx-mockup`, `tx-agents`) and `mcp.json` live in `config/tx/` and are installed under
`<data>/definitions`. Yours go in `<config>/definitions/` (`agents/<name>.md`,
`skills/<name>/SKILL.md`, `mcp.json`) and win by name, as your roles do in `<config>/roles/`.
`tx definitions` lists what resolves; `tx check` validates it.

## Configuration

One file, `~/.config/tx-ide/config.yaml` (`tx help` prints the path in use). `install` writes it
once with every default spelled out; after that it is yours. Every key is optional:

```yaml
agent:                         # defaults for `tx create`; a flag given replaces its key
  engine: claude               # claude | codex
  roles: []                    # empty: the definition's own `roles`
  definition: developer        # the agent definition without --agent; null: roles alone
  model: claude-opus-5-5       # null: the engine's own default
  effort: 3                    # 1-5: low, medium, high, xhigh, max (or the engine's own word)

worktree:
  build_outputs: [target]      # what `tx gc` may delete in an ended agent's worktree

plugins:                       # one entry per plugin (`tx plugins`); a missing entry is on
  engine-codex: { enabled: true }               # `enabled: false` switches a plugin off
  assistant:
    settings: { model: opus, effort: 2 }        # what the assistant runs on
  artifacts:
    settings: { editor: nvim }                  # what an artifact's view runs
  home-themes:
    settings: { theme: koi }                    # the home's backdrop; `b` in the home picks another
```

`model` applies only to `agent.engine`: with `--engine codex`, Codex runs its own default.
`engine-claude` also takes `claude_settings`, Claude settings merged into every launch.

## Where files live

Five directories, XDG style (on macOS too), named after the application: `tx-ide`, or
`TX_APPNAME`, which renames all five as `NVIM_APPNAME` does. Each also takes a flag
(`--config-dir` …) and a variable (`TX_CONFIG_DIR` …), which every agent's session carries.

| directory | default | holds |
|---|---|---|
| config | `~/.config/tx-ide` | `config.yaml`; your `roles/`, `definitions/` and `layouts/` |
| data | `~/.local/share/tx-ide` | the agent records with their conversation archives, `log.jsonl`, the agents' `worktrees/`, plugin data, the shipped roles and definitions, the IDE's tmux config |
| state | `~/.local/state/tx-ide` | locks, each agent's assembled prompt |
| cache | `~/.cache/tx-ide` | Claude's built-in tool list, per Claude version |
| run | `$XDG_RUNTIME_DIR/tx-ide`, else `$TMPDIR/tx-ide.$USER` | `tmux.sock`, tx's own tmux server |

## Development

```sh
cargo test --workspace                                  # needs tmux and git; private sockets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --features fuzz                  # the slow property-based suites too
cargo run -p tx-core-cli -- help                        # the tx binary
```

One Cargo workspace: `tx-core`, the library (records, lifecycle, messaging, definitions, the
plugin host; it never parses arguments or reads the environment); `tx-client`, the tmux and nvim
layer and the `ide` plugin; one crate per plugin under `plugins/`; and `tx-core-cli`, which builds
`tx` and `txi` (the entry point the engines' hooks run). The architecture, writing an engine or a
plugin, and every file are in the guide; it and the API docs are built from `docs-site/` (see
`docs-site/README.md`).
