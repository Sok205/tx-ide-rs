# Introduction

tx-ide is a plain core for running a swarm of interactive coding agents in tmux. Each agent is an
ordinary engine process, Claude Code or Codex, running in its own git worktree, with a durable
record on disk, inside a detached session on tx's **own** tmux server. Agents keep running after
the command that launched them exits. Attaching shows the agent's native terminal; detaching
leaves it running. Nothing is wrapped or re-rendered: what you see is the engine's own UI.

This guide explains how the pieces fit. It is written for a developer or power user who wants to
understand the system, not just use it. The repository's `README.md` is the full reference (every
flag, every key, every file); this guide is the map you read before it. The API documentation of
the `tx-core` library sits next to this guide, in an [internal](../internal/tx_core/index.html)
build (every item, private and hidden ones included) and a [public](../public/tx_core/index.html)
one.

## The mental model in one paragraph

An **agent** is a coding-agent CLI process started by tx in a worktree it made for it; its
**record** (`<data>/agents/<id>/agent.json`) is the truth about it, and the tmux server only holds
a link from the live session back to that record. A **view** is any other tmux session on the
same server (an nvim editor, a shell, a diffview, the timeline, the home screen); views have no
record and live in tmux options only. **Plugins** provide everything beyond the core: the engine
adapters, the key bindings, the picker, the home screen, artifacts, the assistant. **`tx`** is the
one front end; every action an operator, a key binding or an agent takes goes through it.

## What is in the box

- `tx-core`, the library: agent records, the lifecycle (create, fork, respawn, stop),
  messaging, the plugin host. It never parses arguments and never reads the environment.
- Two engine adapters, for Claude Code and Codex, which turn a launch request into a command line
  and turn the engine's hook events into agent states.
- `tx-client`, the tmux layer: views, the composed `tmux.conf`, and `tx ui attach`.
- A set of plugins for the IDE itself: the home screen, the session picker, the new-agent form,
  the timeline, artifacts and code tours, the assistant, mockups and a tutor.
- Two binaries: `tx`, the CLI people type, and `txi`, the internal entry point the engines' hooks
  run.
- Shipped roles (`COMMON`, `DEVELOPER`, `ORCHESTRATOR`, `QA`, `TX-ASSISTANT`) and skills, which
  every agent's system prompt is built from.

## Install

You need Rust 1.92 or newer, tmux, git, jq, fzf and curl, at least one engine CLI (`claude` or
`codex`) and `nvim` for views. Then:

```sh
git clone <this-repo> ~/tx-ide-rs
~/tx-ide-rs/install --dry-run   # print every change, change nothing
~/tx-ide-rs/install             # cargo install tx and txi, tx ui install, config links
tx ui attach                    # the IDE: lands on the home screen
```

The install script is idempotent; a second run finds nothing to do. Re-run `tx ui install` after
you rebuild `tx`, since the tmux configuration names the binary's path.

Once attached, the tmux prefix is `C-a`. The home screen lists your projects, their agents and
their views; `n` starts a new agent, Enter goes into one, `?` shows every key.

## Where to go next

- [Architecture](architecture.md): the crates, the plugin model and the rules plugins follow.
- [Agents and records](agents-and-records.md): what an agent is on disk, how its state moves, worktrees,
  roles and engines.
- [Messaging](messaging.md): how text reaches an agent, and the envelopes that say who sent it.
- [The tmux layer and views](tmux-and-views.md): tx's own server, `ui install`, views and keys.
- [Startup and the home](home.md): what happens between `tx ui attach` and the first screen.
- [Plugins catalogue](plugins.md): one paragraph per bundled plugin.
- [Files and directories](files.md): the five XDG directories and what lives where.
- [Developing](developing.md): building, testing, and adding a plugin.

Behaviour described here is as of the `feat/home-plugin` branch; where something is recent it
says so.
