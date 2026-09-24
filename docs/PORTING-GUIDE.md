# Porting guide (for everyone writing Rust here, human or agent)

Read `PARITY-CONTRACT.md` and `ARCHITECTURE.md` first. The reference is `../tx-ide/lib/tx`; the
contract is `../tx-ide/port-tests` (read the test file of every area your module feeds — the
assertions, not the Python docstrings, are the spec, and `NOTES-0*.md` list where they differ).

## Rules

- **Behaviour first.** Port what the Python does, bugs included, except the FIX quirks listed in
  `PARITY-CONTRACT.md` (implement the fixed behaviour the `@expected_failure_on_python` leg asserts).
  Improvements go to `docs/FOLLOW-UPS.md`, not into the port.
- **Not a transliteration.** Python dicts-as-records become structs/enums; duck typing becomes the
  concrete type unless there are several implementations; Python class hierarchies become enums.
  Keep the module boundaries of the reference (one Rust module per Python module) so a reader can
  diff them, but not its shape inside.
- **No globals, no hidden env reads.** The Python calls `tx_ide_home()` / `os.environ` anywhere.
  In Rust, `storage::Home` (the resolved paths) and any env-derived value are passed in explicitly;
  env is read once at the edge (`main` / the component wiring). This is what lets modules become
  Cordis components (`ARCHITECTURE.md`).
- **JSON bytes** are written with `crate::pyjson::{dumps_pretty, dumps_compact, dumps}` — never
  `serde_json::to_string*` for anything a test can see. Records keep exact key order; numbers that
  Python could hold as int OR float (timestamps, pid) are `serde_json::Number` so an int stays an
  int on resave.
- **Errors**: one `thiserror` enum per module; no `unwrap`/`expect` outside tests except on true
  invariants (comment why). User-facing text is part of the contract: copy message strings from the
  Python exactly (including Python-isms such as a `KeyError` printing as `'key'`) when a test or the
  parity legs can see them.
- **Python traps** (`/port-from-python`): `//` and `%` on negatives (`div_euclid` / `rem_euclid`),
  `str` slicing by code point (`s[:n]` → `chars().take(n)`), truthiness (`if x:` on `""`, `[]`, `0`,
  `None` → spell out the predicate), `int` width.
- **Processes**: `std::process::Command`, argv exactly as the Python builds it (the fakes record
  argv/env). tmux calls go through `crate::tmux::Tmux` only.
- **Filesystem**: atomic record writes = temp file in the same dir + `rename` (`tempfile` crate);
  `log.jsonl` = one `write` on an `O_APPEND` fd; `flock` / exclusive `mkdir` where the Python uses them.
- **Tests**: unit tests in `#[cfg(test)] mod tests` for pure logic with interesting edges; the
  port-tests suite is the acceptance gate (`scripts/port-tests.sh <area>`). Every test observed red
  once. Report pass counts with the denominator.
- **Done** = `cargo fmt`, `cargo clippy --all-targets` with zero warnings, `cargo test` green.
