#!/usr/bin/env bash
# keys: e
# tx key plugin `edit`: prefix+e edits the name, tags and group of the tx session in the active
# pane, in a popup over that pane. Run by `run-shell` from the server's tmux.conf, which
# `txc ui install` writes; @tx-state names the directory holding bin/tmux-edit-tx-session.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

STATE=$(tmux show-option -gv @tx-state)

CONF=$(mktemp -t tx-edit-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind e run-shell -b "$STATE/bin/tmux-edit-tx-session '#{pane_id}' '#{client_name}'"
EOF2

tmux source-file "$CONF"
