#!/usr/bin/env bash
# keys: i
# tx key plugin `details`: prefix+i shows the spawn configuration of the tx session in the active
# pane — record, prompt, tools, MCP servers, skills — in a popup over that pane; q or Esc closes
# it. Replaces tmux's own prefix+i (display-message). Run by `run-shell` from the server's
# tmux.conf, which `tx ui install` writes; @tx-data names the directory holding
# bin/tmux-details-tx-session.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

DATA=$(tmux show-option -gv @tx-data 2>/dev/null)
[ -n "$DATA" ] || exit 0

CONF=$(mktemp -t tx-details-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind i run-shell -b "$DATA/bin/tmux-details-tx-session '#{pane_id}' '#{client_name}'"
EOF2

tmux source-file "$CONF"
