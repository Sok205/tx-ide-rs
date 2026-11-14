#!/usr/bin/env bash
# keys: g
# tx key plugin `graph`: prefix+g attaches the resident agent graph in a popup that fills most of
# the screen; q detaches it while the resident (its log position and transcript reads) stays
# warm, and Enter jumps to the selected agent. Run by `run-shell` from the server's tmux.conf,
# which `tx ui install` writes.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

TX_BIN=$(tmux show-option -gv @tx-bin 2>/dev/null)
ARGUMENTS=$(tmux show-option -gv @tx-arguments 2>/dev/null)
[ -n "$ARGUMENTS" ] || exit 0
[ -n "$TX_BIN" ] || exit 0
TX="$TX_BIN $ARGUMENTS"

CONF=$(mktemp -t tx-graph-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind g display-popup -E -w 92% -h 90% -T " graph " "$TX _graph-attach"
EOF2

tmux source-file "$CONF"
