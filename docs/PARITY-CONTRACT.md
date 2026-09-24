# Parity contract — tx (Python reference) → tx (Rust)

End state **A, replacement**: one Rust `tx` binary replaces `lib/tx` + `bin/tx`. No bindings, no
PyO3. The seam is the process boundary: the CLI verbs, the files under `$TX_IDE_HOME`, the tmux
state on the server, and the processes `tx` launches.

## Unit of parity

One `tx` invocation: argv + env + stdin + cwd + prior `$TX_IDE_HOME` + prior tmux state
→ stdout, stderr, exit code, resulting `$TX_IDE_HOME` files, resulting tmux state, and the argv/env
of every process launched.

## How parity is measured

The black-box suite `tx-ide/port-tests` (433 spec cases, 27 areas) is the contract. It is run
against this binary with `scripts/port-tests.sh [area…]`, which sets `TX_BIN`, `TX_IMPL=rust` and
`TX_HELPERS_DIR`. Progress is reported as **passed / total per area with the denominator**, never
pass/fail alone. The reference's own baseline on the dev host is recorded in `docs/BASELINE.md`.
A case the reference itself fails on the host is not a parity target until explained there.

## Differences that are NOT acceptable

- Any stdout byte, including argparse-shaped usage/error lines (`usage: tx …`,
  `tx <verb>: error: argument --x: invalid choice: …`, `the following arguments are required: …`)
  and `golden/` fixtures (raw, with ANSI where the case compares raw).
- Exit codes; the `tx <verb>: ` prefix of error lines.
- Record JSON: exact key sets and key order (`schema_version` 6 sessions, artifact v2), value
  types, atomic write (temp + rename), the `log.jsonl` line shape and 512-byte cap.
- tmux options, session/window names, environment passed to panes, launch command shapes.

## Differences that ARE acceptable

- Timing (faster is fine), the text of Python tracebacks (they never happen in the port — Q9/Q26).
- Internal structure: the port is organised as Cordis-style components (see `ARCHITECTURE.md`);
  nothing of that is observable.
- Behaviour of quirks whose spec decision is **FIX** (Appendix B): the port implements the fixed
  behaviour, pinned by the `@expected_failure_on_python` legs. `@python_reference_only` legs are
  skipped for the port by design.

## Quirk decisions (from port-tests NOTES)

FIX: Q6, Q7, Q9, Q10, Q11 (`ls --json`), Q12 (chat-op timings from env), Q16, Q19, Q20, Q22, Q25,
Q26, Q28, Q29, Q30, Q31, Q32 (`tx revive`), Q33, Q39. PARITY: Q13, Q21, Q37. Q27: use exact
tmux targets (`=name`).

## Deliberately not ported

- `Storage` S3 stub beyond what SYNC pins; Python-only import side effects (engine registry
  self-registration becomes explicit plugin loading).
- Improvements beyond the FIX list go to `docs/FOLLOW-UPS.md`, never into a parity commit.

## Bash entry points

Helpers (`bin/tmux-*`, `tx-assistant`, `tx-graph-focus-poke`), `tmux/`, `shared/` are copied
verbatim and find `tx` beside themselves. `tmux-session-relabel` (Python in the reference) is a
shim over `tx _relabel`. `install`, `uninstall`, `setup/engines/*`, `claude/statusline.sh` embed
`python3.14 -m tx`; they are ported last (INST/STATUS) as `tx install` / `tx statusline` or as
bash with the python calls replaced.

## Open spec gaps (need a decision from the spec owner)

- **Hook shim / tmux hook line (NOTES-05 "Cross-cutting 7").** INST byte-compares the generated
  shims against the reference's `PYTHONPATH="<LIB>" "python3.14" -m tx hook …` line and
  `readlink == <REPO>/bin/<tool>`; the spec never says what a port's line is. The port writes
  `exec env TX_IDE_HOME="<home>" "<repo>/bin/tx" hook <event>` and links its own `bin/`.
  18 INST cases fail on exactly those strings (123/124 pass with only the expected strings pointed
  at the port). Proposed fix: let the kit take the expected exec line from the port (an env var
  beside `TX_INSTALLER`).
- **argparse wording.** T-CLI-03 / T-ENG-01 / T-MODEL-05 pin `(choose from '1', '2', …)`, which
  no CPython 3.14 on this host prints; the port follows the tests.
