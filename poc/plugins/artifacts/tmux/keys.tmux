#!/usr/bin/env bash
# keys: a
# tx key plugin `artifacts`: prefix+a opens the artifacts menu of the tx session the key fired
# in. While the pane hosts a `tmux` client the key is re-sent one level down, so the bind that
# finally runs sees the NESTED session: `#{session_name}` is that session and `#{client_name}`
# the nested client, and the menu paints inside the pane. A pick re-hosts that pane on the
# artifact's nvim view; in a view the key goes back to the controlling session. The prefix is
# read at compose time (run-shell runs after the theme set it), as for the nav keys. Run by
# `run-shell` from the server's tmux.conf, which `txc ui install` writes; @tx-state names the
# directory holding bin/tmux-artifact-menu.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

STATE=$(tmux show-option -gv @tx-state 2>/dev/null)
[ -n "$STATE" ] || exit 0
menu_prefix=$(tmux show-option -gv prefix 2>/dev/null || echo C-b)

CONF=$(mktemp -t tx-artifacts-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind a if -F '#{==:#{pane_current_command},tmux}' {
  send-keys $menu_prefix a
} {
  run-shell -b "$STATE/bin/tmux-artifact-menu '#{session_name}' '#{client_name}'"
}
EOF2

tmux source-file "$CONF"
