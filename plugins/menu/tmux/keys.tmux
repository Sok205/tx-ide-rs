#!/usr/bin/env bash
# keys: m
# unhook: client-attached[90]
# tx key plugin `menu`: prefix+m opens the tx-ide start menu in a popup on the firing client. The
# first client to attach to a fresh server gets it once on its own: the client-attached hook runs
# `_menu-first` unless the server option @tx-menu-shown is set, and `_menu-first` sets it (a
# client nested in one of the server's own panes never counts). The hook owns index 90 of
# client-attached, so other hooks there stay. Sourced on a live server (`ui install` reloading)
# with a non-nested client already attached, it marks the menu shown: that attach was the first.
# Run by `run-shell` from the server's tmux.conf, which `txc ui install` writes.
#
# Both run in the background (`run-shell -b`) and swallow their output, so a failing menu never
# holds up the attach or drops the client into a run-shell view. The menu learns the firing pane
# and client from `#{pane_id}` / `#{client_name}`, as the other key fragments pass them.
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles the quoting.
# Anything the user re-binds later in their own tmux.conf wins, since tmux is last-write-wins.
set -u

TXC=$(tmux show-option -gv @tx-bin 2>/dev/null)
ARGUMENTS=$(tmux show-option -gv @tx-arguments 2>/dev/null)
[ -n "$ARGUMENTS" ] || exit 0
[ -n "$TXC" ] || exit 0
TX="$TXC $ARGUMENTS"

CONF=$(mktemp -t tx-menu-keys.XXXXXX)
trap 'rm -f "$CONF"' EXIT

cat >>"$CONF" <<EOF2
bind m run-shell -b "$TX _menu-open '#{pane_id}' '#{client_name}' >/dev/null 2>&1 || true"
set-hook -g client-attached[90] {
  if -F '#{==:#{@tx-menu-shown},}' {
    run-shell -b "$TX _menu-first '#{pane_id}' '#{client_name}' >/dev/null 2>&1 || true"
  }
}
EOF2

tmux source-file "$CONF"

pane_ttys=" $(tmux list-panes -a -F '#{pane_tty}' 2>/dev/null | tr '\n' ' ') "
for tty in $(tmux list-clients -F '#{client_tty}' 2>/dev/null); do
  case "$pane_ttys" in
    *" $tty "*) ;;
    *) tmux set-option -g @tx-menu-shown 1; break ;;
  esac
done
