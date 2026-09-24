#!/usr/bin/env bash
# setup/engines/claude.sh — the per-engine (Claude) integration for tx-ide (stage S2).
#
#   claude.sh {install|uninstall|status} [--dry-run] [--settings PATH]
#
# The Claude integration is decoupled from the tx core (the future-agent seam, §10/§15): this
# script owns the settings.json hook registration behind the `_tx_ide_managed` marker, so a second
# engine can ship its own setup/engines/<name>.sh without touching the core. install-flip.md §3/§4
# is canonical for the mechanism.
#
#   install   — generate 6 C9-baked hook shims under $TX_IDE_HOME/hooks/claude/{start,pre,work,post,notify,end}.sh
#               and surgically REPOINT settings.json's tx hook events at them (match-by-marker, so
#               interleaved peon-ping / require-worktree / discord entries are preserved); install
#               the ONE global tmux session-closed → reconcile hook (C2); stop mx-speaker (the
#               mailbox goes dark); update the marker (mode=coexist, home, previous).
#   uninstall — reverse all of it via the marker: restore each event's prior command + the prior
#               marker EXACTLY, remove the generated shims + the global tmux hook. Leaves the tx
#               core (records / history / log / symlinks) untouched.
#   status    — report drift (what the marker claims vs what settings.json holds).
#
# COEXISTENCE (S2, install-flip §3): run with TX_IDE_HOME=~/.tx-ide-next so the dev home is baked
# into the shims (C9) and a `txn`-spawned session drives state, while the live ~/.tx-ide, mailbox,
# and statusLine stay untouched. Discrimination is internal (the hook no-ops on an id the dev home
# never recorded, D4) — there is no dispatch key. The statusLine move (C10) is Flip-only.
#
# SAFETY — verifiable without touching the live environment:
#   * --dry-run prints every action and changes nothing.
#   * --settings PATH operates on that file (a COPY) instead of the live settings.json AND treats
#     the run as a sandbox: the two LIVE-GLOBAL steps (the tmux global hook + the mx-speaker stop)
#     are printed, never executed, because they act on live globals a copy can't stand in for.
#   So `install --settings <copy>` fully exercises the shim generation + settings surgery on a
#   copy, and `install --dry-run` previews the live-global steps — neither mutates the daily
#   driver. The real coexistence activation (no --settings, not --dry-run) is a supervised step.
#
# settings.json is a symlink into the claude-server git repo; every edit targets its REALPATH and
# is atomic (temp + rename), so the dotfiles repo sees one clean, auditable diff.
set -euo pipefail

SCRIPT_DIR="$(cd -P "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -P "$SCRIPT_DIR/../.." && pwd)"
TX="$REPO_ROOT/bin/tx"

# C9: the home baked into the shims + recorded in the marker. Default ~/.tx-ide; the S2 dev home is
# ~/.tx-ide-next (passed via TX_IDE_HOME). Expand a leading ~ (env vars are not tilde-expanded).
TX_HOME="${TX_IDE_HOME:-$HOME/.tx-ide}"
TX_HOME="${TX_HOME/#\~/$HOME}"

CLAUDE_DIR="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
DEFAULT_SETTINGS="$CLAUDE_DIR/settings.json"
STAMP="$(date +%Y%m%d%H%M%S)"

B=$'\e[1m'; G=$'\e[32m'; Y=$'\e[33m'; RED=$'\e[31m'; D=$'\e[2m'; X=$'\e[0m'
ok()     { printf '  %s→%s %-46s %s%s%s\n' "$G" "$X" "$1" "$G" "${2:-ok}" "$X"; }
warn()   { printf '  %s→%s %-46s %s%s%s\n' "$Y" "$X" "$1" "$Y" "${2:-}" "$X"; }
info()   { printf '  %s%s%s\n' "$D" "$*" "$X"; }
header() { printf '\n%s%s%s\n' "$B" "$*" "$X"; }

usage() {
  cat >&2 <<EOF
usage: claude.sh {install|uninstall|status} [--dry-run] [--settings PATH] [--no-context-profile]

  install     register tx-ide's Claude hooks (coexistence: TX_IDE_HOME=~/.tx-ide-next)
  uninstall   reverse the registration exactly via the _tx_ide_managed marker
  status      report drift between the marker and settings.json

  --dry-run              print every action, change nothing
  --settings PATH        operate on PATH (a copy) — sandbox: skip the live tmux hook + mx-speaker stop
  --no-context-profile   register hooks only; leave the context profile (below) untouched
EOF
}

# ----- argument parsing --------------------------------------------------------------------

OP=""; DRY_RUN=0; SETTINGS=""; APPLY_PROFILE=1
while [[ $# -gt 0 ]]; do
  case "$1" in
    install|uninstall|status) OP="$1"; shift ;;
    --dry-run)        DRY_RUN=1; shift ;;
    --settings)       SETTINGS="${2:?--settings needs a PATH}"; shift 2 ;;
    --settings=*)     SETTINGS="${1#*=}"; shift ;;
    --no-context-profile) APPLY_PROFILE=0; shift ;;
    -h|--help)        usage; exit 0 ;;
    *) printf 'claude.sh: unknown argument: %s\n' "$1" >&2; usage; exit 2 ;;
  esac
done
[[ -n "$OP" ]] || { usage; exit 2; }

# --settings ⇒ sandbox: don't touch live globals (the tmux hook + mx-speaker stop).
SANDBOX=0
if [[ -n "$SETTINGS" ]]; then SANDBOX=1; else SETTINGS="$DEFAULT_SETTINGS"; fi

# The shims and the events they own. The state hooks span the full Claude set so a missed edge
# self-heals: a missed UserPromptSubmit recovers on the first tool call (work.sh), a missed Stop on
# the idle_prompt Notification (notify.sh). C6: Stop / StopFailure / PermissionRequest → post.sh.
#
# T4 (capture-after-launch): the session id is CAPTURED from the hook payload, never pre-minted, so
# the two shims that establish a chat — SessionStart (at startup) and UserPromptSubmit (first turn) —
# KEEP stdin (`keep-stdin`) so `tx hook` can read `session_id` + `transcript_path` off the payload.
# SessionStart closes the id-unknown window before the first turn. The rest still drain stdin (their
# state maps from the event name alone); notify keeps it to read notification_type.
START_SHIM="$TX_HOME/hooks/claude/start.sh"    # SessionStart                                      → session-start → chat capture
PRE_SHIM="$TX_HOME/hooks/claude/pre.sh"        # UserPromptSubmit                                  → prompt-submit → WORKING + capture
WORK_SHIM="$TX_HOME/hooks/claude/work.sh"      # PreToolUse/PostToolUse/…/SubagentStart/PreCompact → working      → WORKING
POST_SHIM="$TX_HOME/hooks/claude/post.sh"      # Stop / StopFailure / PermissionRequest            → stop         → WAITING (C6)
NOTIFY_SHIM="$TX_HOME/hooks/claude/notify.sh"  # Notification (reads notification_type)            → notification → WAITING if yield
END_SHIM="$TX_HOME/hooks/claude/end.sh"        # SessionEnd                                        → session-end  → IDLE (+ingest)

# The global tmux session-closed hook value (C2): baked like service.py's per-session hook —
# $TX_IDE_HOME + the tx binary resolved literally, since tmux runs hooks with a minimal env.
TMUX_SESSION_CLOSED="run-shell -b \"env TX_IDE_HOME=$TX_HOME $TX hook session-closed\""

# ----- shim generation (C9 bake) -----------------------------------------------------------

write_shim() {  # <path> <event> [keep-stdin]
  local path="$1" event="$2" keep_stdin="${3:-}"
  if [[ $DRY_RUN -eq 1 ]]; then
    info "would generate shim $path  ($event, home baked = $TX_HOME)"
    return 0
  fi
  mkdir -p "$(dirname "$path")"
  # Most shims drain the payload (state maps from the event name alone); the notify shim leaves it
  # on stdin so `tx hook notification` can read notification_type.
  local drain="cat >/dev/null"
  [[ -n "$keep_stdin" ]] && drain="# stdin left connected — tx hook reads the JSON payload"
  cat >"$path" <<EOF
#!/bin/bash
# tx-ide hook shim — GENERATED by setup/engines/claude.sh (stage S2). Do NOT edit; re-run the
# installer to regenerate. C9: \$TX_IDE_HOME and the tx binary are baked in as literals because
# Claude runs hooks with a minimal env (no shell rc). Drain (or pass) the payload, then drive state.
$drain
exec env TX_IDE_HOME="$TX_HOME" "$TX" hook $event
EOF
  chmod +x "$path"
  ok "shim $path" "$event"
}

remove_shim() {  # <path>
  local path="$1"
  if [[ $DRY_RUN -eq 1 ]]; then info "would remove shim $path"; return 0; fi
  if [[ -f "$path" ]]; then rm -f "$path"; ok "removed $path"; else info "$path (already absent)"; fi
}

# ----- the context profile + settings.json surgery (tx _claude-settings) -------------------
# Besides the hooks, install applies tx's context profile — settings keys a tx worker never uses
# (disableWorkflows, disableArtifact, includeGitInstructions, skillOverrides, permissions.deny;
# measured on claude 2.1.232: headless baseline 22,340 tok → 11,893). The profile table lives in
# the binary (crates/tx/src/verbs/install.rs). Every key is reversible: install records each key's
# prior value in the marker under `context_profile_previous`, uninstall restores it verbatim.
# `--no-context-profile` skips the whole block.
#
# Re-enable per session without touching this file — CLI --settings outranks the user layer:
#   claude --settings '{"disableWorkflows": false}'
#
# The surgery is match-by-marker (only the entries tx recorded are touched), atomic (temp +
# rename on the REALPATH) and reversible; `tx _claude-settings` owns it.
run_settings() {  # <install|uninstall|status>
  local -a args=("$1" "--settings=$SETTINGS" "--home=$TX_HOME" "--stamp=$STAMP"
                 "--session-closed=$TMUX_SESSION_CLOSED")
  [[ $SANDBOX -eq 1 ]] && args+=(--sandbox)
  [[ $DRY_RUN -eq 1 ]] && args+=(--dry-run)
  [[ $APPLY_PROFILE -eq 0 ]] && args+=(--no-context-profile)
  "$TX" _claude-settings "${args[@]}"
}

# ----- live-global steps (skipped under --dry-run / sandbox) --------------------------------

# Where a stashed FOREIGN global session-closed hook is parked so uninstall can restore it.
PREV_HOOK_FILE="$TX_HOME/hooks/.session-closed.prev"
# All live-tmux calls go through this so a test can aim them at an isolated socket (TX_TMUX_SOCKET).
tmux_cmd() { tmux ${TX_TMUX_SOCKET:+-L "$TX_TMUX_SOCKET"} "$@"; }

apply_tmux_hook() {  # set | unset
  local action="$1" shown prior
  if [[ "$action" == set ]]; then
    shown="tmux set-hook -g session-closed '$TMUX_SESSION_CLOSED'"
  else
    shown="tmux set-hook -gu session-closed"
  fi
  # Skip the LIVE socket in sandbox/dry-run, but honor an explicit isolated socket (TX_TMUX_SOCKET)
  # so the capture/restore path is testable without touching the live server.
  if [[ $DRY_RUN -eq 1 || ( $SANDBOX -eq 1 && -z "${TX_TMUX_SOCKET:-}" ) ]]; then
    info "live-only (skipped here): $shown"
    return 0
  fi
  # Q28: probe with list-sessions — `info` fails from an unattached client below tmux 3.6.
  if ! command -v tmux >/dev/null 2>&1 || ! tmux_cmd list-sessions >/dev/null 2>&1; then
    warn "tmux global session-closed" "no tmux server — will apply on next start"
    return 0
  fi
  if [[ "$action" == set ]]; then
    # Non-destructive: stash a FOREIGN existing hook (one that isn't ours) so uninstall restores it
    # rather than clobbering a user's hook. The live server IS a boundary — observed populated here.
    prior="$(tmux_cmd show-hooks -g session-closed 2>/dev/null | sed -E 's/^session-closed(\[[0-9]+\])? *//')"
    if [[ -n "$prior" && "$prior" != *"tx hook session-closed"* ]]; then
      mkdir -p "$(dirname "$PREV_HOOK_FILE")"
      printf '%s' "$prior" >"$PREV_HOOK_FILE"
      warn "tmux global session-closed" "stashed an existing hook (restored on uninstall)"
    fi
    tmux_cmd set-hook -g session-closed "$TMUX_SESSION_CLOSED" && ok "tmux global session-closed" "set (→ reconcile)"
  elif [[ -f "$PREV_HOOK_FILE" ]]; then
    tmux_cmd set-hook -g session-closed "$(cat "$PREV_HOOK_FILE")" && ok "tmux global session-closed" "restored prior hook"
    rm -f "$PREV_HOOK_FILE"
  else
    tmux_cmd set-hook -gu session-closed 2>/dev/null && ok "tmux global session-closed" "unset" || info "no global session-closed hook"
  fi
}

stop_speaker() {
  local pid_file="$CLAUDE_DIR/mailbox/mx-speaker.pid"
  if [[ $DRY_RUN -eq 1 || $SANDBOX -eq 1 ]]; then
    info "live-only (skipped here): stop mx-speaker via $pid_file (mailbox left on disk, dark)"
    return 0
  fi
  if [[ -f "$pid_file" ]]; then
    local pid; pid="$(cat "$pid_file" 2>/dev/null || true)"
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null && ok "mx-speaker" "stopped (pid $pid)" || warn "mx-speaker" "could not stop pid $pid"
    else
      info "mx-speaker not running (stale pid file left in place)"
    fi
  else
    info "no mx-speaker pid file (already dark)"
  fi
  # L5: leave the mailbox files + pid on disk (dark, not deleted) — Flip removes them.
}

# ----- subcommands -------------------------------------------------------------------------

cmd_install() {
  printf '%s== claude.sh install ==%s  %s\n' "$B" "$X" "$( ((DRY_RUN)) && echo '(dry-run)'; ((SANDBOX)) && echo "(sandbox: $SETTINGS)")"
  header "Hook shims under $TX_HOME/hooks (C9-baked)"
  write_shim "$START_SHIM"  session-start keep-stdin
  write_shim "$PRE_SHIM"    prompt-submit keep-stdin
  write_shim "$WORK_SHIM"   working
  write_shim "$POST_SHIM"   stop
  write_shim "$NOTIFY_SHIM" notification keep-stdin
  write_shim "$END_SHIM"    session-end

  header "settings.json — repoint the tx hook events (match-by-marker)"
  run_settings install

  header "Global tmux session-closed → reconcile (C2)"
  apply_tmux_hook set

  header "mx-speaker (mailbox goes dark — files left on disk)"
  stop_speaker
}

cmd_uninstall() {
  printf '%s== claude.sh uninstall ==%s  %s\n' "$B" "$X" "$( ((DRY_RUN)) && echo '(dry-run)'; ((SANDBOX)) && echo "(sandbox: $SETTINGS)")"
  header "settings.json — restore via the marker"
  run_settings uninstall

  header "Remove generated hook shims"
  remove_shim "$START_SHIM"
  remove_shim "$PRE_SHIM"
  remove_shim "$WORK_SHIM"
  remove_shim "$POST_SHIM"
  remove_shim "$NOTIFY_SHIM"
  remove_shim "$END_SHIM"

  header "Remove global tmux session-closed hook"
  apply_tmux_hook unset
  info "mx-speaker is NOT restarted (manual rollback step — install-flip §7)"
}

cmd_status() {
  printf '%s== claude.sh status ==%s\n' "$B" "$X"
  header "settings.json marker vs reality  ($SETTINGS)"
  run_settings status
  header "Generated shims ($TX_HOME/hooks)"
  for shim in "$START_SHIM" "$PRE_SHIM" "$WORK_SHIM" "$POST_SHIM" "$NOTIFY_SHIM" "$END_SHIM"; do
    if [[ -f "$shim" ]]; then ok "$shim" "present"; else warn "$shim" "missing"; fi
  done
}

case "$OP" in
  install)   cmd_install ;;
  uninstall) cmd_uninstall ;;
  status)    cmd_status ;;
esac
