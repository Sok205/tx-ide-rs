# Follow-ups (after parity is proven — never inside a parity commit)

- messages.rs hand-writes Python's regexes and `datetime.fromisoformat` (no regex/chrono dep):
  1.3k lines vs 339 in the reference. Revisit with `regex` / `jiff` once parity holds.
- render.rs approximates `unicodedata` width / `str.isprintable` with hand tables.
- artifact.rs rejects float revs such as `1.0`, which Python accepted (narrowing on corrupt data).
- argparse `int()` accepts ASCII digits / i64 only (Python: any Unicode digit, unbounded).
- Kit fixes to propose upstream: see the end of BASELINE.md.
- Installer scripts call `tx` hidden verbs through a scratch `TX_IDE_HOME` because the core creates
  the home skeleton on every start; a core flag to skip `ensure` would remove the `tx_seam` wrapper.
- `setup/iterm.sh` still edits iTerm prefs with `python3` (optional path).
- migrations: CPython-only JSON tokens (`NaN`, `Infinity`) in a record give a JSONDecodeError skip
  reason instead of Python's `not an upgradable record (schema_version=nan)` (corrupt input only).
- messages: the composer count uses ASCII digits; Python's `\d` is Unicode; a non-string truthy
  `uuid` is replaced by the synthetic key (Python keeps it).
- codex_update schedule test: rare flake under the full parallel suite (~1/25), not reproduced
  in isolation; the assertion now prints the log + marker when it fires.
- Reinstall rewrites settings.json (+ `.bak.<stamp>`) even when nothing changed — same as the
  reference's claude.sh / statusline step; skip the write when the serialized document is unchanged.
- tx tutor: decode chunked HTTP responses before adding non-Python packs.
- tx tutor: `tutor` is a public verb with no reference counterpart, so `tx --help` gains one line —
  a deliberate difference from the Python reference. Likewise `tx start -h` gains `--tutor [LANG]`.
- tx attach (picker): from a view pane that already hosts a nested session (`tmux`), the pick
  replaces what that pane shows (the old session keeps running); the reference only nested into
  shell panes and switched the client otherwise. Deliberate — the tutor view's viewer pane relies on it.
- prefix+n (new agent: an nvim form (nvim/lua/tx-new-agent.lua) for name / tags / prompt / project / engine / model / effort / role, then a background `tx spawn`, outcome on the status line; the projects are `config.json`'s `projects` + `default_project`, else the firing pane's directory)
  and the hidden `tx _new-agent` behind it are new here; the reference has no spawn key. The key
  replaces tmux's stock next-window on `n`.
- prefix+f (fork the pane's session: the popup only asks the name, the fork runs in the background, outcome on the status line) and the
  hidden `tx _fork-pane` behind it are new here; the reference has no fork key. The key
  replaces tmux's stock find-window on `f`.
- tx timeline (prototype): a public verb with no reference counterpart — the artifact-browser
  prototype's HTML timeline as a ratatui TUI (`crates/tx/src/plugins/timeline/`); `tx --help` gains one
  line. The data layer diffs byte-for-byte against `prototypes/artifact-browser/timeline.py` via
  the hidden `tx _timeline-json`. The view deliberately shows less than the HTML: working / idle
  bars per lane and peer messages (who → who) only. You-spoke rings, milestone diamonds, spawn
  edges, the dialogue drawer, group drag-reorder and the reply box are in the payload (or the
  HTML) but not drawn. New deps: ratatui, regex.
- tx timeline row taxonomy: a peer envelope the harness delivered inside `<pasted_content id=…>`
  (long messages arrive like that) is unwrapped before classification; the Python prototype
  counts such rows as the human typing and loses the edge (19 of 68 edges on 2026-10-11).
  Kit fix to propose upstream in `timeline.py` (`ukind` / `clean_user`).
- prefix+e (`_edit-session`): the popup (still over the firing pane) has prefix+f's heavy yellow border, the form is
  drawn in the palette's truecolor (the reference used basic ANSI blue / dim white / yellow), and the
  popup closes on exit with a failure on the status line (the reference kept it open on errors).
- Pane borders show the hosted session's state: `tx _pane-info` prints a third line (the record's
  state) and `tmux-pane-session-name` ends the label with a right-aligned bar of middle sextants,
  green while `working`, border grey otherwise, with the resting age (`14m`) before it once a minute
  has passed (`last_activity` is now stamped on every live transition, so it reads "since the
  state changed" in the picker too); the global `pane-border-format` lost its trailing
  space so the bar reaches tmux's two-cell inset. New here; the reference's `_pane-info` printed
  two lines and stamped `last_activity` only at turn start. Deferred: a session resting while its spawned workers or Claude's own background
  tasks still run shows as resting (the Stop payload's `background_tasks` and the children's
  `parent` field are the inputs for a `delegating` state).
