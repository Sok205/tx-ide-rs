#!/usr/bin/env bash
# keys: T
# tx key plugin `timeline`: prefix+T attaches the resident timeline in a popup; q detaches it
# while the resident (and its transcript cache) stays warm. Run by `run-shell` from the server's
# tmux.conf, which `txc ui install` writes.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

TXC=$(tmux show-option -gv @tx-bin 2>/dev/null)
STATE=$(tmux show-option -gv @tx-state 2>/dev/null)
[ -n "$STATE" ] || exit 0
[ -n "$TXC" ] || exit 0
TX="$TXC --state-dir $STATE"

CONF=$(mktemp -t tx-timeline-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind T display-popup -E -w 118 -h 30 -x C -y 1 -T " timeline " "$TX _timeline-attach"
EOF2

tmux source-file "$CONF"
