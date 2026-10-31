#!/usr/bin/env bash
# keys: f
# tx key plugin `chat`: prefix+f forks the tx session in the active pane (a popup asks for the
# new name). Run by `run-shell` from the server's tmux.conf, which `txc ui install` writes;
# @tx-data names the directory holding bin/tmux-fork-tx-session.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

DATA=$(tmux show-option -gv @tx-data 2>/dev/null)
[ -n "$DATA" ] || exit 0

CONF=$(mktemp -t tx-chat-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind f run-shell -b "$DATA/bin/tmux-fork-tx-session '#{pane_id}' '#{client_name}'"
EOF2

tmux source-file "$CONF"
