#!/usr/bin/env bash
# keys: T
# tx key plugin `timeline`: prefix+T attaches the resident timeline in a popup (`_timeline-open`);
# q detaches it while the resident (and its transcript cache) stays warm. On a lane, through
# tx-client's `show`: Enter focuses the pane showing its agent, a picks a pane to attach it into
# (attach-to, the `tx-attach-to` table in keys/ide.tmux), p opens it in a popup. Meant for the
# home view.
# Run by `run-shell` from the server's tmux.conf, which `tx update` writes.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

TX_BIN=$(tmux show-option -gv @tx-bin 2>/dev/null)
ARGUMENTS=$(tmux show-option -gv @tx-arguments 2>/dev/null)
[ -n "$ARGUMENTS" ] || exit 0
[ -n "$TX_BIN" ] || exit 0
TX="$TX_BIN $ARGUMENTS"

CONF=$(mktemp -t tx-timeline-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind T run-shell -b "$TX _timeline-open '#{pane_id}' '#{client_name}' >/dev/null 2>&1 || true"
EOF2

tmux source-file "$CONF"
