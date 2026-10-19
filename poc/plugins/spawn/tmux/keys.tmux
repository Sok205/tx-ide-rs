#!/usr/bin/env bash
# keys: n
# tx key plugin `spawn`: prefix+n asks for a new agent's name, tags, prompt, project, engine,
# model, effort and role in a popup on the firing client, then creates the agent session through
# `txc _new-agent`. Run by `run-shell` from the server's tmux.conf, which `txc ui install`
# writes; @tx-state names the directory holding bin/tmux-new-agent.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

STATE=$(tmux show-option -gv @tx-state 2>/dev/null)
[ -n "$STATE" ] || exit 0

CONF=$(mktemp -t tx-spawn-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind n run-shell -b "$STATE/bin/tmux-new-agent '#{pane_id}' '#{client_name}'"
EOF2

tmux source-file "$CONF"
