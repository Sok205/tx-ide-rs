#!/usr/bin/env bash
# tx-ide tmux fragment. Invoked via `run-shell` from ~/.tx-ide/tmux.conf,
# which is sourced from the user's ~/.tmux.conf by install.sh.
#
# Reads @tx-ide-* options to decide which view features to enable:
#   @tx-ide-popups          on|off   prefix+t (tx), prefix+T (timeline), prefix+e (edit), prefix+/ (assistant), prefix+X (kill),
#                                    prefix+a (artifacts menu of the nested session; back from a view),
#                                    prefix+n (new agent; replaces tmux's next-window on that key),
#                                    prefix+f (fork the pane's session; replaces tmux's find-window)
#   @tx-ide-pane-borders    on|off   pane-border-format integration + colors
#   @tx-ide-agent-scroll    on|off   C-u/C-d → PageUp/PageDown in agent panes
#   @tx-ide-nav-keys        on|off   C-h/j/k/l seamless nav: nvim splits ↔ panes ↔ nested sessions
#   @tx-ide-pane-keys       on|off   M-1..9 → select-pane
#   @tx-ide-window-keys     on|off   User0..8 → select-window (terminal must send)
#   @tx-ide-session-labels  on|off   prefix+s shows each session's name + tags (choose-tree)
#   @tx-ide-palette         tokyonight-night|off   color overrides
#
# Composes a tmux.conf fragment and source-file's it, so tmux's own parser
# handles multi-command binds correctly (shell-quoted `;` confuses tmux when
# `bind-key` is invoked directly). Anything the user re-binds later in their
# own tmux.conf wins, since tmux is last-write-wins.
set -u

# The poc dir (this script lives in <DIR>/tmux) and the txc invocation, both handed over by
# the server's own options (written by `txc ui install`).
DIR=$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd -P)
TXC=$(tmux show-option -gv @tx-bin)
STATE=$(tmux show-option -gv @tx-state)
TX="$TXC --state-dir $STATE"
# Navigator for the nav-keys edge path.
NAV="$DIR/bin/tmux-nav"
# Confirm-then-kill the prefix+X bind runs on the active pane's tx session.
KILL="$DIR/bin/tmux-kill-tx-session"
# Editor for the focused session's name and tags.
EDIT="$DIR/bin/tmux-edit-tx-session"

# A bind is emitted only when its verb exists (plugin verbs and built-ins both list as
# two-space-indented lines in `txc help`).
HAVE=$("$TXC" --state-dir "$STATE" help 2>/dev/null)
has_verb() { grep -q "^  $1\b" <<<"$HAVE"; }

option() {
  tmux show-option -gv "$1" 2>/dev/null || printf '%s' "$2"
}

popups=$(option @tx-ide-popups on)
pane_borders=$(option @tx-ide-pane-borders on)
agent_scroll=$(option @tx-ide-agent-scroll on)
nav_keys=$(option @tx-ide-nav-keys on)
pane_keys=$(option @tx-ide-pane-keys on)
window_keys=$(option @tx-ide-window-keys on)
session_labels=$(option @tx-ide-session-labels on)
palette=$(option @tx-ide-palette tokyonight-night)

CONF=$(mktemp -t tx-ide-bindings.XXXXXX)
trap 'rm -f "$CONF"' EXIT

# --- Popups (prefix+t / prefix+T / prefix+/) ---
# prefix+t opens the tx picker; prefix+/ forwards a line to the tx-assistant. tx attach figures
# out the pane to glue into by asking tmux directly (`display-message -p #{pane_id}`), so the bind
# doesn't need to plumb anything through. There is no mailbox anymore, so prefix+m is left bound to
# its native `select-pane -m` (tx-ide no longer overrides it).
# prefix+T attaches a resident timeline in a popup; q detaches while its cache stays warm.
#
# The picker popup is `-w 118`: wide enough for NAME + the 25-col LOCATION + STARTED/IDLE/ROLE + tag
# chips without wrapping, with ~5 cols of extra NAME headroom. Keep it in lockstep with
# `_NAMEW_OVERHEAD` (lib/tx/render.py): that budget assumes LOCATION is 25, and the popup funds those
# columns plus the extra NAME room so the NAME column isn't squeezed.
if [ "$popups" = on ]; then
  cat >>"$CONF" <<EOF
bind t display-popup -E -w 118 -h 30 -x C -y 1 -T " tx " "$TX pick"
EOF
  if has_verb _timeline-attach; then
    cat >>"$CONF" <<EOF
bind T display-popup -E -w 118 -h 30 -x C -y 1 -T " timeline " "$TX _timeline-attach"
EOF
  fi
  if has_verb send-user-message; then
    cat >>"$CONF" <<EOF
bind '/' command-prompt -p "tx-assistant>" {
  set-buffer -b tx-assistant-input "%%"
  run-shell -b "$TX send-user-message --from-buffer"
}
EOF
  fi
  # prefix+X: kill the active pane's tx session behind a stock-style confirm-before prompt
  # (the tmux-native sibling of prefix+x / prefix+&, but through `txc kill` so the record
  # stays honest). The helper resolves the nested-vs-outer target and display name.
  cat >>"$CONF" <<EOF
bind X run-shell "$KILL '#{pane_id}' '#{session_name}'"
bind e run-shell -b "$EDIT '#{pane_id}' '#{client_name}'"
EOF
  # run-shell expands the pane/client formats before the popup starts; display-popup's
  # shell-command does not, so the popup runs from a backgrounded run-shell.
  if has_verb _fork-pane; then
    cat >>"$CONF" <<EOF
bind f run-shell -b "tmux display-popup -c '#{client_name}' -E -w 80 -h 4 -x C -y C -b heavy -S 'fg=yellow,bold' -T ' Fork session ' \"$TX _fork-pane '#{pane_id}' '#{client_name}'\""
EOF
  fi
  if has_verb _new-agent; then
    cat >>"$CONF" <<EOF
bind n run-shell -b "tmux display-popup -c '#{client_name}' -E -w 94 -h 23 -x C -y C -b heavy -S 'fg=yellow,bold' -T ' New agent ' \"$TX _new-agent '#{pane_id}' '#{client_name}'\""
EOF
  fi
  # prefix+a: artifacts menu. While the pane hosts a `tmux` client the key is re-sent one
  # level down, so the bind that finally runs sees the NESTED session: `#{session_name}` is the tx
  # id and the menu paints inside the pane. Prefix read at compose time, as for the nav keys.
  if has_verb _artifact-menu; then
    menu_prefix=$(tmux show-option -gv prefix 2>/dev/null || echo C-b)
    cat >>"$CONF" <<EOF
bind a if -F '#{==:#{pane_current_command},tmux}' {
  send-keys $menu_prefix a
} {
  run-shell -b "$TX _artifact-menu '#{session_name}' '#{client_name}'"
}
EOF
  fi
fi

# --- Status bar resources ---
printf 'set -g status-right "#[fg=#a9b1d6,bg=#3b4261]#(%s/bin/tmux-system-resources)  %%H:%%M:%%S  #S "\n' "$DIR" >>"$CONF"

# --- Pane borders ---
# Border style + format integrating tmux-pane-session-name. Hex literals
# duplicate shared/palette.sh:
#   ACCENT_HEX       #7aa2f7   (active border)
#   BORDER_DIM_HEX   #3b4261   (inactive border)
if [ "$pane_borders" = on ]; then
  if [ "$palette" = tokyonight-night ]; then
    cat >>"$CONF" <<'EOF'
set -g pane-border-style "fg=#3b4261"
set -g pane-active-border-style "fg=#7aa2f7,bold"
EOF
  fi
  # tmux caches a #() job by its command text and re-runs it at most once a second, so a label
  # change waits for the next second. `@tx-label-rev` is part of the command: txc bumps it after
  # an edit (Ui::refresh_labels), which makes a new job that runs on the next redraw.
  cat >>"$CONF" <<'EOF'
set -g pane-border-lines heavy
set -g pane-border-indicators both
set -g pane-border-status off
set -g pane-border-format " [#P] #(#{@tx-bin} --state-dir #{@tx-state} _pane-label #D #{@tx-label-rev}) "
EOF
  # A new window in a VIEW gets window-top pane borders (so nested panes carry a labelled border).
  # A view is marked by the @tx-view session option, so the hook reads it directly with `if-shell
  # -F` (non-empty/non-zero = true) — no Python launch on the hook path.
  cat >>"$CONF" <<'EOF'
set-hook -g after-new-window "if-shell -F '#{@tx-view}' 'setw pane-border-status top'"
EOF
fi

# --- Session labels in prefix+s (choose-tree) ---
if [ "$session_labels" = on ]; then
  cat >>"$CONF" <<EOF
bind s {
  run-shell "$TX _relabel"
  choose-tree -Zs -F '#{?session_format,#{?@tx-name,#{@tx-name}  ,}#{session_windows}w#{?session_attached, (attached),},#{?window_format,#{window_index}: #{window_name},#{pane_current_command}}}'
}
EOF
fi

# --- Agent scroll intercept ---
# Inside an agent (Claude Code / Codex) pane, C-u/C-d scroll the TUI buffer (PageUp/PageDown).
# Outside one, they pass through (default copy-mode-vi behavior). Match the literal "claude" or
# "codex" command, or a version-string-shaped command (an agent TUI shows its version while
# loading: "2.1.138"). Kept in LOCKSTEP with the Python agent-pane predicates (spawn.infer_role /
# reconcile._is_agent_command) — same set of engine binaries, enforced by review across the boundary.
if [ "$agent_scroll" = on ]; then
  cat >>"$CONF" <<'EOF'
bind -n C-u if -F '#{||:#{==:#{pane_current_command},claude},#{||:#{==:#{pane_current_command},codex},#{m:[0-9]*.[0-9]*.[0-9]*,#{pane_current_command}}}}' 'send-keys PageUp' 'send-keys C-u'
bind -n C-d if -F '#{||:#{==:#{pane_current_command},claude},#{||:#{==:#{pane_current_command},codex},#{m:[0-9]*.[0-9]*.[0-9]*,#{pane_current_command}}}}' 'send-keys PageDown' 'send-keys C-d'
EOF
fi

# --- Nav keys (C-h/j/k/l: nvim splits ↔ panes ↔ nested sessions) ---
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
if [ "$nav_keys" = on ]; then
  nav_pass='#{||:#{==:#{pane_current_command},nvim},#{==:#{pane_current_command},tmux}}'
  nav_nested='#{==:#{pane_current_command},tmux}'
  nav_prefix=$(tmux show-option -gv prefix 2>/dev/null || echo C-b)
  while read -r key direction edge; do
    cat >>"$CONF" <<EOF
bind -n C-$key if -F '$nav_pass' {
  send-keys C-$key
} {
  if -F '#{$edge}' {
    run-shell -b "$NAV $direction #{pane_id} #{client_tty}"
  } {
    select-pane -$direction
  }
}
EOF
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
else
  # Re-sourcing with the option off must clear previously installed binds. Unbind only
  # bindings that test pane_current_command — ours — never a personal C-h/j/k/l scheme.
  for key in h j k l; do
    tmux list-keys -T root "C-$key" 2>/dev/null | grep -q 'pane_current_command' &&
      printf 'unbind -n C-%s\n' "$key" >>"$CONF"
    tmux list-keys -T prefix "C-$key" 2>/dev/null | grep -q 'pane_current_command' &&
      printf 'unbind C-%s\n' "$key" >>"$CONF"
  done
fi

# --- Pane keys (M-1..9 → select-pane) ---
# Terminal must produce M-N. Configure your terminal (see setup/iterm.sh).
# Default tmux: prefix M-1..7 trigger layout cycling — unbind so a stray
# prefix+digit doesn't reflow your panes.
if [ "$pane_keys" = on ]; then
  for n in 1 2 3 4 5 6 7 8 9; do
    printf 'bind -n M-%s select-pane -t %s\n' "$n" "$n" >>"$CONF"
  done
  for n in 1 2 3 4 5 6 7; do
    printf 'unbind -T prefix M-%s\n' "$n" >>"$CONF"
  done
fi

# --- Window keys (User0..8 → select-window) ---
# Define user-key escape sequences ESC+'W'+digit, then bind. Terminal must
# emit ESC+'W'+digit on Cmd+Opt+N (or equivalent).
if [ "$window_keys" = on ]; then
  i=0
  for n in 1 2 3 4 5 6 7 8 9; do
    printf 'set -s user-keys[%d] "\\033W%s"\n' "$i" "$n" >>"$CONF"
    printf 'bind -n User%d select-window -t %s\n' "$i" "$n" >>"$CONF"
    i=$((i + 1))
  done
fi

tmux source-file "$CONF"
