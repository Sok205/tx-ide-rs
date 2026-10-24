#!/usr/bin/env bash
# keys: X
# tx key plugin `kill`: prefix+X confirms, then stops the tx session in the active pane through
# `txc kill`, so the record stays honest. Run by `run-shell` from the server's tmux.conf, which
# `txc ui install` writes; @tx-state names the directory holding bin/tmux-kill-tx-session.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

STATE=$(tmux show-option -gv @tx-state 2>/dev/null)
[ -n "$STATE" ] || exit 0

CONF=$(mktemp -t tx-kill-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind X run-shell "$STATE/bin/tmux-kill-tx-session '#{pane_id}' '#{session_name}'"
EOF2

tmux source-file "$CONF"
