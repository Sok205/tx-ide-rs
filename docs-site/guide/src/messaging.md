# Messaging

Agents talk to each other, and the operator talks to agents, by typing into their terminals.
There is no side channel: a message is text typed into the agent's pane and submitted, exactly as
if someone had typed it. What makes it a message is the envelope around it, which tells the
agent who it is from.

## `tx send`

```sh
tx send worker-1 -- please rebase on main and rerun the tests
```

`tx send [--via NAME] TARGET [--] TEXT…` resolves the target (a name or an id) and calls
`TxCore::send_message`. The caller decides who the sender is; core never guesses. The CLI works
it out from where it runs:

- Inside tx's own tmux server (`$TMUX` names tx's socket) with a `$TX_SESSION_ID` that names a
  known record: the sender is **that agent**. Every agent's session carries `TX_SESSION_ID`, so
  an agent running `tx send` is framed as itself without doing anything.
- Inside tx's server in a view's pane: the sender is **the operator**, typing from that view.
- Anywhere else: the operator, from `--via NAME` when given.

## The envelopes

The engine adapter turns the text into an [`Input`](../internal/tx_core/engine/enum.Input.html);
the default wraps it with [`build_envelope`](../internal/tx_core/messaging/fn.build_envelope.html):

```text
<from-agent session="worker-1">please review my diff</from-agent>
<from-user session="plan.md">tighten section 2</from-user>
<from-user>status?</from-user>
```

`from-agent` names the sending agent's session. `from-user` is the human; its `session` attribute
says where they typed (a view, an nvim buffer), not who they are, and it may be absent. The
shipped COMMON role tells every agent how to read these: treat a `from-agent` message as a peer
(reply if useful, no obligation), and a `from-user` message with the same authority as anything
typed directly into the agent. To reply, an agent runs `tx send <name> -- <text>`; `tx whoami`
gives its own name.

## Delivery

Core types the envelope into the pane with `tmux send-keys -l` (literal, with a trailing `;`
escaped so tmux cannot read it as a command separator), waits 300 ms so the engine registers the
text as input rather than a shortcut, then presses Enter. The whole delivery holds the target's
lock, so two senders never interleave keystrokes. The receipt means "submitted": not read, not
completed, not retried. The pause is the main throughput limit, roughly three messages a second
per recipient. The message also lands in `log.jsonl` as a `message-sent` event.

## Asking the assistant: prefix+/ and the focus envelope

The `assistant` plugin keeps one agent, `tx-assistant`, for managing the IDE itself: starting
workers and views, renaming, tagging, messaging. prefix+/ opens a one-line tmux prompt from any
pane. The key fragment parks the line in the paste buffer `tx-assistant-input` and runs
`tx assistant ask --from-buffer --pane <firing pane>`. The plugin starts the assistant if it is
not running, waits up to 20 seconds for it to leave `starting`, and delivers the line as the
operator, prefixed with a **focus envelope** that describes where they were:

```text
<from-user session="home"><tx-command-prompt session-name='home' window-index='1'
  pane-index='0' pane-id='%3' pane-path='/Users/me/code/app'
  inner-session-name='worker-1' session-kind='view'/> start a reviewer here</from-user>
```

The attributes are the firing pane's session, window and pane position, its id and current
directory, `inner-session-name` when that pane is nest-attached to another session, and
`session-kind` (`view` or `process`, the latter meaning an agent). The TX-ASSISTANT role uses them
to resolve deictic words: "this agent" is the inner session if there is one, else the outer;
"this view" is the outer session; "here" is the pane, and "this directory" (the `--cwd` for "start
a worker here") is `pane-path`. Without an envelope (the pane is gone, or the line came from
`tx assistant ask TEXT` or the home screen's `/`), the message is
`<from-user session="prefix+/">…</from-user>`, and the role tells the assistant not to guess a
target.

The assistant's conventions are deliberately narrow: one operation per line, no clarifying
questions except for a missing target, the bare result as the reply, and never stopping `home`
or itself on a vague "kill this".
