#!/usr/bin/env bash
# keys: s
# unset: pane-border-format
# unhook: after-new-window
# tx key plugin `labels`: prefix+s lists sessions with their names and tags (choose-tree), and
# each pane border carries the nested session's label. Run by `run-shell` from the server's
# tmux.conf, which `txc ui install` writes. The border's look (style, lines, indicators) is the
# `ide` plugin's; this fragment owns only the format and the hook that turns borders on in views.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles multi-command
# binds correctly. Anything the user re-binds later in their own tmux.conf wins, since tmux is
# last-write-wins.
set -u

TXC=$(tmux show-option -gv @tx-bin 2>/dev/null)
ARGUMENTS=$(tmux show-option -gv @tx-arguments 2>/dev/null)
[ -n "$ARGUMENTS" ] || exit 0
[ -n "$TXC" ] || exit 0
TX="$TXC $ARGUMENTS"

CONF=$(mktemp -t tx-labels-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

# tmux caches a #() job by its command text and re-runs it at most once a second, so a label
# change waits for the next second. `@tx-label-rev` is part of the command: txc bumps it after
# an edit (Ui::refresh_labels), which makes a new job that runs on the next redraw.
# A new window in a VIEW gets window-top pane borders (so nested panes carry a labelled border).
# A view is marked by the @tx-view session option, so the hook reads it directly with `if-shell
# -F` (non-empty/non-zero = true).
cat >>"$CONF" <<'EOF2'
set -g pane-border-format " [#P] #(#{@tx-bin} #{@tx-arguments} _pane-label #D #{@tx-label-rev}) "
set-hook -g after-new-window "if-shell -F '#{@tx-view}' 'setw pane-border-status top'"
EOF2

cat >>"$CONF" <<EOF2
bind s {
  run-shell "$TX _relabel"
  choose-tree -Zs -F '#{?session_format,#{?@tx-name,#{@tx-name}  ,}#{session_windows}w#{?session_attached, (attached),},#{?window_format,#{window_index}: #{window_name},#{pane_current_command}}}'
}
EOF2

tmux source-file "$CONF"
