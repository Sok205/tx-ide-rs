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
