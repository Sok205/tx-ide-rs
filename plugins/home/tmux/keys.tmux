#!/usr/bin/env bash
# keys: m
# tx key plugin `home`: prefix+m opens the tx-ide home (sessions, history, the assistant) in a
# popup on the firing client. The home is also the floor of the `home` view's pane, where
# `tx ui attach` lands; the popup is the same screen from anywhere else. Run by `run-shell` from
# the server's tmux.conf, which `tx ui install` writes.
#
# The bind runs in the background (`run-shell -b`) and swallows its output, so a failing home
# never drops the client into a run-shell view. The home learns the firing pane and client from
# `#{pane_id}` / `#{client_name}`, as the other key fragments pass them.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

TX_BIN=$(tmux show-option -gv @tx-bin 2>/dev/null)
ARGUMENTS=$(tmux show-option -gv @tx-arguments 2>/dev/null)
[ -n "$ARGUMENTS" ] || exit 0
[ -n "$TX_BIN" ] || exit 0
TX="$TX_BIN $ARGUMENTS"

CONF=$(mktemp -t tx-home-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind m run-shell -b "$TX _home-open '#{pane_id}' '#{client_name}' >/dev/null 2>&1 || true"
EOF2

tmux source-file "$CONF"
