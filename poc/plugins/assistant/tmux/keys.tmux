#!/usr/bin/env bash
# keys: /
# tx key plugin `assistant`: prefix+/ asks the tx-assistant from any pane. A command prompt takes
# one line; the bind parks it in the paste buffer `tx-assistant-input` (no shell quoting of the
# typed text) and `txc assistant ask --from-buffer --pane <firing pane>` reads it, warms the
# assistant and delivers it as the operator typing in that pane's view, naming the session nested
# in the pane (run-shell expands `#{pane_id}` against the client that pressed the key). Run by
# `run-shell` from the server's tmux.conf, which `txc ui install` writes.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the `{ }`
# block. Anything the user re-binds later in their own tmux.conf wins, since tmux is
# last-write-wins.
set -u

TXC=$(tmux show-option -gv @tx-bin 2>/dev/null)
ARGUMENTS=$(tmux show-option -gv @tx-arguments 2>/dev/null)
[ -n "$ARGUMENTS" ] || exit 0
[ -n "$TXC" ] || exit 0
TX="$TXC $ARGUMENTS"

CONF=$(mktemp -t tx-assistant-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind '/' command-prompt -p "tx-assistant>" {
  set-buffer -b tx-assistant-input "%%"
  run-shell -b "$TX assistant ask --from-buffer --pane '#{pane_id}'"
}
EOF2

tmux source-file "$CONF"
