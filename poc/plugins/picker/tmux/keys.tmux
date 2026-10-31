#!/usr/bin/env bash
# keys: t
# tx key plugin `picker`: prefix+t opens the tx session picker in a popup. `txc pick` finds the
# pane to nest into by asking tmux directly (`display-message -p #{pane_id}`), so the bind plumbs
# nothing through. Run by `run-shell` from the server's tmux.conf, which `txc ui install` writes.
#
# The popup is `-w 118`: wide enough for NAME + the 25-col LOCATION + STARTED/IDLE/ROLE + tag
# chips without wrapping, with ~5 cols of extra NAME headroom. Keep it in lockstep with
# `LOCATION_W` and `picker_namew` in tx-client's render.rs.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

TXC=$(tmux show-option -gv @tx-bin 2>/dev/null)
ARGUMENTS=$(tmux show-option -gv @tx-arguments 2>/dev/null)
[ -n "$ARGUMENTS" ] || exit 0
[ -n "$TXC" ] || exit 0
TX="$TXC $ARGUMENTS"

CONF=$(mktemp -t tx-picker-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind t display-popup -E -w 118 -h 30 -x C -y 1 -T " tx " "$TX pick"
EOF2

tmux source-file "$CONF"
