#!/usr/bin/env bash
# keys: P
# tx key plugin `switch`: prefix+P lists the plugins in a popup; space switches one, Enter writes
# config.yaml and runs `tx update`. Run by `run-shell` from the server's tmux.conf, which
# `tx update` writes.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

TX_BIN=$(tmux show-option -gv @tx-bin 2>/dev/null)
ARGUMENTS=$(tmux show-option -gv @tx-arguments 2>/dev/null)
[ -n "$ARGUMENTS" ] || exit 0
[ -n "$TX_BIN" ] || exit 0
TX="$TX_BIN $ARGUMENTS"

CONF=$(mktemp -t tx-switch-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind P display-popup -E -w 60 -h 30 -x C -y C -T " plugins " "$TX switch"
EOF2

tmux source-file "$CONF"
