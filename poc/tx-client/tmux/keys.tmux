#!/usr/bin/env bash
# keys: C-h C-j C-k C-l
# tx key plugin `ide`: the IDE's root-table keys and global look. Run by `run-shell` from the
# server's tmux.conf, which `txc ui install` writes (first, before the other plugins' fragments).
#
#   C-u / C-d       PageUp / PageDown in agent panes (Claude Code / Codex)
#   C-h/j/k/l       seamless nav: nvim splits <-> panes <-> nested sessions (bin/tmux-nav)
#   M-1..9          select-pane
#   User0..8        select-window (the terminal must send ESC W <digit>)
#   pane borders    colours, heavy lines, both indicators; the border format is `labels`'
#
# The status bar, status-right included, is conf/tmux.conf's.
#
# It binds nothing in the prefix table except nav's prefix+C-h/j/k/l escapes, which are part of
# the nav keys. Composes a tmux.conf fragment and source-file's it, so tmux's own parser handles
# multi-command binds correctly (shell-quoted `;` confuses tmux when `bind-key` is invoked
# directly). Anything the user re-binds later in their own tmux.conf wins, since tmux is
# last-write-wins.
set -u

# The data directory, handed over by the server's own options (written by `txc ui install`);
# the helper scripts live in <data>/bin.
DATA=$(tmux show-option -gv @tx-data 2>/dev/null)
[ -n "$DATA" ] || exit 0
DIR="$DATA/bin"
# Navigator for the nav-keys edge path.
NAV="$DIR/tmux-nav"

CONF=$(mktemp -t tx-ide-bindings.XXXXXX)
trap 'rm -f "$CONF"' EXIT

# --- Pane border look, agent scroll intercept ---
# Pane borders: the look only; `labels` owns the format and the new-window hook. Hex literals
# duplicate shared/palette.sh:
#   ACCENT_HEX       #7aa2f7   (active border)
#   BORDER_DIM_HEX   #3b4261   (inactive border)
# Agent scroll: inside an agent (Claude Code / Codex) pane, C-u/C-d scroll the TUI buffer
# (PageUp/PageDown). Outside one, they pass through (default copy-mode-vi behavior). Match the
# literal "claude" or "codex" command, or a version-string-shaped command (an agent TUI shows its
# version while loading: "2.1.138"). Kept in LOCKSTEP with the Python agent-pane predicates
# (spawn.infer_role / reconcile._is_agent_command) — same set of engine binaries, enforced by
# review across the boundary.
cat >>"$CONF" <<EOF2
set -g pane-border-style "fg=#3b4261"
set -g pane-active-border-style "fg=#7aa2f7,bold"
set -g pane-border-lines heavy
set -g pane-border-indicators both
set -g pane-border-status off
bind -n C-u if -F '#{||:#{==:#{pane_current_command},claude},#{||:#{==:#{pane_current_command},codex},#{m:[0-9]*.[0-9]*.[0-9]*,#{pane_current_command}}}}' 'send-keys PageUp' 'send-keys C-u'
bind -n C-d if -F '#{||:#{==:#{pane_current_command},claude},#{||:#{==:#{pane_current_command},codex},#{m:[0-9]*.[0-9]*.[0-9]*,#{pane_current_command}}}}' 'send-keys PageDown' 'send-keys C-d'
EOF2

# --- Nav keys (C-h/j/k/l: nvim splits <-> panes <-> nested sessions) ---
# Root binds are evaluated per client, and tx nests as a client-in-a-pane on the SAME
# server — so send-keys into a nested (`tmux`) pane re-fires this bind one level down,
# recursing to any depth. pane_current_command is exact at that boundary for the same
# reason: the pane's process IS `tmux` or `nvim`. nvim moves its own splits and calls
# bin/tmux-nav at the tabpage edge; the bind calls it at a session edge, #{client_tty}
# pinning the first hop. Copy-mode and popups never see root binds.
# Cost: these keys no longer reach shells/agent TUIs. prefix+C-h/j/k/l sends the
# literal key, re-wrapped per nest level so the innermost non-tmux pane gets the bare
# key. The prefix is read at compose time (run-shell runs after the user's conf set
# it); a `none` prefix skips the escape binds.
nav_pass='#{||:#{==:#{pane_current_command},nvim},#{==:#{pane_current_command},tmux}}'
nav_nested='#{==:#{pane_current_command},tmux}'
nav_prefix=$(tmux show-option -gv prefix 2>/dev/null || echo C-b)
while read -r key direction edge; do
  cat >>"$CONF" <<EOF2
bind -n C-$key if -F '$nav_pass' {
  send-keys C-$key
} {
  if -F '#{$edge}' {
    run-shell -b "$NAV $direction #{pane_id} #{client_tty}"
  } {
    select-pane -$direction
  }
}
EOF2
  # Q24: tmux reports a disabled prefix as `None`; compare case-insensitively (bash 3.2: no ${,,}).
  if [ "$(printf %s "$nav_prefix" | tr '[:upper:]' '[:lower:]')" != none ]; then
    printf "bind C-%s if -F '%s' 'send-keys %s C-%s' 'send-keys C-%s'\n" \
      "$key" "$nav_nested" "$nav_prefix" "$key" "$key" >>"$CONF"
  fi
done <<'NAVSPEC'
h L pane_at_left
j D pane_at_bottom
k U pane_at_top
l R pane_at_right
NAVSPEC

# --- Pane keys (M-1..9 → select-pane) ---
# Terminal must produce M-N. Configure your terminal (see setup/iterm.sh).
# Default tmux: prefix M-1..7 trigger layout cycling — unbind so a stray
# prefix+digit doesn't reflow your panes.
for n in 1 2 3 4 5 6 7 8 9; do
  printf 'bind -n M-%s select-pane -t %s\n' "$n" "$n" >>"$CONF"
done
for n in 1 2 3 4 5 6 7; do
  printf 'unbind -T prefix M-%s\n' "$n" >>"$CONF"
done

# --- Window keys (User0..8 → select-window) ---
# Define user-key escape sequences ESC+'W'+digit, then bind. Terminal must
# emit ESC+'W'+digit on Cmd+Opt+N (or equivalent).
i=0
for n in 1 2 3 4 5 6 7 8 9; do
  printf 'set -s user-keys[%d] "\\033W%s"\n' "$i" "$n" >>"$CONF"
  printf 'bind -n User%d select-window -t %s\n' "$i" "$n" >>"$CONF"
  i=$((i + 1))
done

tmux source-file "$CONF"
