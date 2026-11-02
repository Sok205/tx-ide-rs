---
tx:
  skills: [tx-sessions, tx-artifacts]
---

# COMMON — every agent must follow this

These conventions apply to every agent in this system: the tx-assistant, every worker, and any
ad-hoc agent you start inside the orchestration repo. The front end is `txc`; `txc help` is the
full command reference.

## Reporting

Be extremely concise. Sacrifice grammar for concision.

## Self-introduction

Lead your **first response** with a brief self-introduction so the operator can confirm your
setup at a glance:

1. Your agent name (`txc whoami`) + cwd and current git branch.
2. Which convention/role files you have loaded — always this `COMMON.md`; state whether a role
   file (`DEVELOPER.md`, etc.) is **also** loaded, and quote one identifying line from each so it
   is verifiable.
3. One line on your purpose.

Keep it to a few lines, then continue with whatever was asked (or wait for instructions if nothing
was).

## Agents and views go through txc, never raw tmux

Anything that creates, ends, or changes an agent or a view goes through `txc` — raw `tmux` leaves
the agent record stale. Raw tmux is read-only: inspection, moving between panes and windows,
scrolling, copy-mode. The agents run on txc's own tmux server, so raw tmux needs its socket:
`txc where <agent>` prints the socket, the session target and the pane target.

Before starting anything — a worker agent, an nvim view, a shell view — follow the `tx-sessions`
skill (shipped at `$TX_DATA_DIR/roles/skills/tx-sessions/SKILL.md`); flag syntax is in `txc help`.

## Inter-agent communication

Other agents may be running alongside you, and you can message each other.

**Receiving:** a peer message arrives wrapped like:

    <from-agent session="and-48">body</from-agent>

Treat it as a peer, not a user. You MAY reply, but don't have to.

A second envelope carries the **operator**:

    <from-user session="wrangler-p1-diff">body</from-user>

That is the human — treat it with the same authority as anything typed directly into your agent,
and answer it. The `session` attribute is where they typed it (a view, an nvim buffer), not who
sent it, so you know which file they are looking at. It may be absent: `<from-user>body</from-user>`.

**Sending:** `txc send <name> -- <text>`, targeting an agent name from `txc ls`. Find your own
name with `txc whoami`. The message is framed as coming from you automatically.

## Artifacts

Durable deliverables — plans, docs, reports, question sets — are **artifacts**: versioned records
managed by `txc artifact`, never an engine's own built-in artifact or publish tool. Before creating
or updating one, follow the `tx-artifacts` skill (shipped at
`$TX_DATA_DIR/roles/skills/tx-artifacts/SKILL.md`). Never write under
`$TX_DATA_DIR/plugins/artifacts/` by hand — revisions are immutable, and a direct edit desyncs the
history from disk.
