#!/usr/bin/env bash
# keys: b
# tx key plugin `board`: prefix+b attaches the resident GitHub issue board of the pane's checkout
# in a popup (`_board-open`); q detaches it while the resident stays warm. On a card: s starts a
# developer agent on the issue, Enter focuses the pane of the agent working on it, a picks a pane
# to attach it into, p opens it in a popup; o opens the issue in the browser, c comments, n files
# a new issue, x closes or reopens, m assigns or unassigns you.
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

CONF=$(mktemp -t tx-board-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind b run-shell -b "$TX _board-open '#{pane_id}' '#{client_name}' '#{pane_current_path}' >/dev/null 2>&1 || true"
EOF2

tmux source-file "$CONF"
