# port-tests baseline — Python reference on macOS

What the acceptance suite in `tx-ide/port-tests/` does against the **Python reference** on the
dev host. Use this to tell "the port is wrong" apart from "nothing can pass this here".

## Host

| fact | value |
|---|---|
| OS | macOS 14.6 (Darwin 23.6.0, arm64), BSD userland (`stat`, `sed`, `readlink`) |
| tmux | 3.5a (Homebrew; the spec floor is 3.6, and Homebrew has 3.7c available) |
| bash | `/bin/bash` 3.2.57 is the only bash on PATH (`#!/usr/bin/env bash` resolves to it) |
| python | python3.14 3.14.4 (Homebrew) |
| missing | `setsid`, `flock` (util-linux), `bwrap`, `/proc`, `C.UTF-8` locale |
| present | `/usr/bin/sandbox-exec`, nvim, lsof, jq, git |
| `/tmp` | symlink to `/private/tmp` |
| reference | tx-ide `9d2b97c` (PR #134, coverage 433/433) |

Command: `TMPDIR=/tmp python3.14 -m unittest discover port-tests -v`. `TMPDIR=/tmp` is needed
because the default macOS `$TMPDIR` (`/var/folders/…`) makes tmux socket paths longer than the
104-byte limit. Log: `/tmp/txport/baseline-python.log`.

**Totals:** 847 tests: 710 ok, 75 skipped, 67 FAIL, 14 ERROR. That is **81 failing entries**,
subtests counted separately.

**Main finding:** 50 of the 81 entries go away if you run with **`TMPDIR=/private/tmp`**. This was
verified by re-running all 38 affected methods: every one passed except the argparse-wording ones
in bucket 2. Use `TMPDIR=/private/tmp` for every run on this host, for both the reference and the port.

**Flakiness:** none found. The 12 failures that looked timing-dependent (waits, pty expects,
popups) were each re-run 3× in isolation and failed the same way every time
(`/tmp/txport/rerun-{1,2,3}.log`). No test is in a `flaky` bucket.

Status legend: **target**: the port can and should pass it on this host, possibly after the
listed kit fix or workaround. **host-gated**: nobody can pass it on macOS as written.

---

## 1. `/tmp` → `/private/tmp`: kit temp root not canonicalised (50) — kit bug exposed by macOS

`TxCase.setUp` uses `Path(tempfile.mkdtemp(...))` without `.resolve()`. The reference (like git)
canonicalises cwd, worktree and settings paths, so its output says `/private/tmp/txkit-…`. The
tests compare that against the unresolved `/tmp/txkit-…`. Several assertions compare `git worktree
list` directly with `self.git.path`, so they fail **whatever tx does**. On Linux `/tmp` is a real
directory, so this never showed up there. Verified: all of these pass with
`TMPDIR=/private/tmp`.

| tests | evidence | status |
|---|---|---|
| cli_03 parity ×14 subtests (`['w']`, `--tag ''`, `--tag ','`, 7× `--cmd zsh …`, `--env FOO`, `--group ''`, `--role ''`, `--role NOPE`, `--read-only`) | `git.worktrees()` gives `['/private/tmp/…/repo']` but the test expects `['/tmp/…/repo']` | target |
| cli_04, cli_05 | cwd / worktree list is `/private/tmp/…` | target |
| inst_14 copy_and_settings_edit, inst_14 settings_symlink, inst_18 legacy_marker, inst_18 ownership_check, inst_26 | the installer prints `backup: /private/tmp/…settings.json.bak.N`, which is the realpath (install:348 / claude.sh:212 `os.path.realpath`) | target |
| nvim_02, nvim_05 ×2, nvim_06 ×2 subtests, nvim_06 other_and_shell, nvim_07 | record cwd or spawn log `/private/tmp/…` | target |
| smoke helpers_are_copies | `PosixPath('/private/tmp/…/helpers/bin') != '/tmp/…'` | target |
| spawn_07, 08, 11, 13, 14, 17, 19, 20, 21 | `Spawned … (cwd=/private/tmp/…)`, `git worktree list` realpaths | target |
| tmux_01 ×2, tmux_09, tmux_15 focus_envelope, tmux_15 parity_remote, tmux_15 process_host | `pane_path` / envelope `pane-path='/private/tmp/…'` | target |
| tmuxconf_02, 04, 05, 07 | bindings use the helper path `/private/tmp/…/helpers/tmux/../bin/…` | target |
| wt_01, wt_03 | `git worktree list` lines start `/private/tmp/…` (wt_03: `next()` → StopIteration) | target |

## 2. CPython argparse wording pinned to an old interpreter (5) — test-kit bug

The tests expect `invalid choice: '0' (choose from '1', '2', '3', '4', '5')`, with quoted choices.
Every Homebrew 3.14.x (3.14.3_1 and 3.14.4 checked), and 3.13.7 and 3.12.9 too, format it as
`', '.join(map(str, choices))`, which gives `(choose from 1, 2, 3, 4, 5)`. No python3.14 the
reference can run under produces the pinned string. T-SPAWN-18 already pins only the prefix
(`choose from .*`), so the kit disagrees with itself. The Rust `argparse.rs` also emits the
unquoted 3.14 form.

| tests | evidence | status |
|---|---|---|
| cli_03 parity subtests `--effort 0`, `--effort 6`, `--engine gpt` | stderr `… invalid choice: '0' (choose from 1, 2, 3, 4, 5)` | target (after the kit fix; keep the port on the 3.14 wording) |
| eng_01 effort_levels_and_default | same, `--effort 0` | target (after the kit fix) |
| model_05 engine_values | `invalid choice: 'gemini' (choose from claude, codex, antigravity)` | target (after the kit fix) |

## 3. util-linux `flock` CLI used by the kit (6) — macOS host difference

The tests hold or probe locks with `subprocess.Popen(["flock", "-x", lock, "sleep", "120"])` /
`flock -n`. macOS ships no `flock` binary. The reference itself uses `fcntl.flock`
(history.py:211, engines/codex_update.py:76), which works on macOS.

| tests | evidence | status |
|---|---|---|
| chat_12 rollover_self_catch_up, chat_16 detached_finish | `FileNotFoundError: 'flock'` in `_hold_ingest_lock` (test_chat.py:200) | target (with the kit fix or `brew install flock`) |
| eng_41 codex_schedule_update_interval_flock_log | `update_lock_is_free` (test_eng.py:1071) | target (same) |
| hist_07, hist_08, hist_09 | `hold_lock[_until_released]` (test_hist.py:134/147) | target (same) |

## 4. Kit code that only reads `/proc` (7) — kit bug on macOS

| tests | evidence | status |
|---|---|---|
| status_10 background_post, status_10 listener_hangs, status_10 no_connection ×3 subtests | `curl_processes` iterates `/proc` → `FileNotFoundError: '/proc'` (test_status.py:132) | host-gated until the kit uses `ps`/`pgrep -f` |
| nvim_16 socket_discovery | `nvim_pid_of` reads `/proc/<pid>/comm`; the `OSError` is swallowed, so the wait always times out (test_nvim.py:743) | host-gated until the kit fix |
| smoke teardown_reaper_kills_this_homes_children | `kill_home_children` without `/proc` falls back to `pkill -f <root>` and returns `[]` (txkit.py:1323) → `69437 not found in []` | host-gated until the kit fix. **Safety:** on macOS the D15 reaper only kills processes whose *cmdline* names the temp root, so stragglers like `sleep`, fake engines and `hook ingest` can survive teardown |

## 5. Linux read-only sandbox (`bwrap`) asserted without `@platform_only("linux")` (5) — host-gated

On Darwin, `read_only.wrap_read_only_command` wraps with `/usr/bin/sandbox-exec -p <profile>`,
never `bwrap`. The fake `bwrap` is never called, and the start command never starts with
`…/fake-bin/bwrap`.

| tests | evidence | status |
|---|---|---|
| spawn_16 read_only_wrapper | start command is `/usr/bin/sandbox-exec -p '(version 1)…' claude …` | host-gated |
| spawn_16 bwrap_absent_refused | expects `Linux read-only sessions require bubblewrap (bwrap)`. It errors earlier in `path_without_binary` (bucket 6) | host-gated |
| eng_12 claude_read_only_launch_shape | `no bwrap dump … within 10.0s` | host-gated |
| chat_13 read_only_source_respawns, chat_19 read_only_propagation_rollover | `_wait_respawn` times out after 15 s (the respawn runs under sandbox-exec). The later assertions require a `bwrap` start command | host-gated |

## 6. `path_without_binary` breaks on an unreadable PATH entry (1, + spawn_16 bwrap_absent above) — kit bug on macOS

`os.scandir('/usr/sbin')` yields `weakpass_edit`, a symlink into a root-only dir.
`entry.is_dir()` raises `PermissionError` (reproduced).

| tests | evidence | status |
|---|---|---|
| ro_03 no_sandbox_exec_refused (`@platform_only("darwin")`) | `PermissionError: '/usr/sbin/weakpass_edit'` (test_ro.py:69) | target (after the kit fix). This is the only darwin-specific RO refusal case, and it currently never runs |

## 7. tmux on macOS reports the executable, not `argv[0]` (2) — host-gated

`bash -c 'exec -a claude sleep 600'` → `#{pane_current_command}` = `sleep` on macOS, and
`claude` on Linux. Verified on a scratch `-L` server. `stuck_probes` waits for the probe names,
which never appear.

| tests | evidence | status |
|---|---|---|
| recon_05 agent_command_keeps_working, recon_05 non_agent_command_is_demoted | `wait_pane_commands` times out after 10 s (test_recon.py:41) | host-gated |

## 8. Reference helper bug under macOS stock bash 3.2 (1) — genuine reference bug on this host

`shared/palette.sh` `tag_color_index` does `printf -v c '%d' "'${s:i:1}"`. Under bash 3.2 this
gives the **signed first byte** (`é` → `-61`) even in `en_US.UTF-8`, and bash 5 gives 233. The
hash goes negative, `TAG_CUBE[-n]` → `bad array subscript`, and the result is an empty `colour`.
The kit's `C.UTF-8` pin also does not exist on macOS, which makes it byte-wise as well. The
failure persists with `en_US.UTF-8` (checked). Real users on stock macOS see a broken border
colour for non-ASCII tags.

| tests | evidence | status |
|---|---|---|
| tmuxconf_11 non_ascii_tag_colour_agrees_with_list | helper prints `#[fg=colour,…][é]` where `tx _list` says `colour73` | target (the port's helpers must hash by code point without depending on bash 3.2) |

## 9. Pty/popup text matching on this host (1) — kit bug on macOS

`UTF8_LOCALE = C.UTF-8` does not exist on macOS. With `en_US.UTF-8` patched in (via
`/tmp/txport/run_locale.py`), the popup does render, but tmux emits `ESC ( B` (the terminfo `sgr0`
here is `\E(B\E[m`) inside `› Name:\x1b(B   w`. `ANSI` in txkit.py:130 does not strip `ESC ( B`,
and `assert_popup` removes only `\x0f`.

| tests | evidence | status |
|---|---|---|
| tmuxconf_14 edit_popup_geometry | `Regex didn't match: '› Name:\s+w'`. The popup is present, and the raw text contains `› Name:\x1b(B   w` | target (after the kit fix) |

## 10. tmux 3.5a (below the 3.6 floor) behaviour (2)

| tests | evidence | status |
|---|---|---|
| tmux_18 respawn_pane_nest_attach | 3.5a renders `pane_start_command` as `exec \${SHELL:-zsh}` (one backslash). The kit's `_start_command` undoes only the 3.4 form `\\$` (test_tmux.py:40) | target (after the kit unescaper accepts both) |
| tmuxconf_13 kill_tx_session_view_home_and_plain_session | After `y`, `show-messages` shows the `run-shell "…/tx kill 'p' 2>&1 >/dev/null"` ran (issued by the helper's detached command client), and `tx kill p` alone prints the expected error and exits 1. On 3.5a nothing reaches the attached client (no view mode, no `returned 1`). Probable cause is tmux-version-specific routing of run-shell output; **not confirmed**, re-check on tmux ≥ 3.6 | host-gated on 3.5a (re-test on ≥ 3.6) |

## 11. GNU `stat -c` in a kit-written fake (1) — kit bug on BSD userland

| tests | evidence | status |
|---|---|---|
| attach_05 arm_file_lifecycle | the hand-written `fzf` runs `stat -c %s "$TX_ARM_FILE"`, and stderr gets `stat: illegal option -- c` (test_attach.py:312) | target (after the kit fix) |

---

## Summary

| bucket | entries | status |
|---|---|---|
| 1 `/private/tmp` realpath (kit root) | 50 | target (run with `TMPDIR=/private/tmp`) |
| 2 argparse wording (old CPython) | 5 | target after the kit fix |
| 3 no `flock` CLI | 6 | target with `brew install flock` or the kit fix |
| 4 `/proc`-only kit code | 7 | host-gated |
| 5 bwrap asserted on darwin | 5 | host-gated |
| 6 `path_without_binary` PermissionError | 1 | target after the kit fix |
| 7 tmux `exec -a` comm | 2 | host-gated |
| 8 bash 3.2 palette hash (reference bug) | 1 | target |
| 9 locale / `ESC ( B` in popup | 1 | target after the kit fix |
| 10 tmux 3.5a | 2 | 1 target after the kit fix, 1 host-gated (unconfirmed) |
| 11 BSD `stat` | 1 | target after the kit fix |
| **total** | **81** | flaky: 0 |

By category: (a) macOS/BSD host: 3, 5, 7, 10 = 15 · (b) operator-environment leakage: **0**
(nothing from `~/.claude`, the real PATH tools or the host name influenced an assertion) ·
(c) flaky: **0** · (d) reference bug: 8 = 1 · (e) kit bug: 1, 2, 4, 6, 9, 11 = 65.

## Suggested kit fixes (to propose upstream; not applied)

1. `TxCase.setUp`: `self.root = Path(tempfile.mkdtemp(prefix="txkit-")).resolve()`, and the same
   for `GitFixture.path`. This removes bucket 1 on macOS.
2. `test_cli.py:202-203`, `test_eng.py:234` and `test_model.py:159`: pin the 3.14 wording
   (`choose from 1, 2, …`) or only the prefix, as T-SPAWN-18 already does.
3. Replace the `flock` CLI with a small Python holder (`fcntl.flock` in a child process) in
   test_chat / test_hist / test_eng. Otherwise, gate those tests on `requires_bin("flock")`.
4. `kill_home_children`, `test_status.curl_processes` and `test_nvim.nvim_pid_of`: add a non-`/proc`
   path, e.g. `ps -Eww -o pid=,command=` for the environment on macOS (or `pgrep -f`), and
   `ps -o comm= -p`. Make the reaper actually reap on macOS; it is a D15 safety guard.
5. Add `@platform_only("linux")` to spawn_16 ×2, eng_12, chat_13 (respawn) and chat_19. Add
   darwin twins that assert the `sandbox-exec -p` shape.
6. `path_without_binary`: wrap `entry.is_dir()` / `os.access` in `try/except OSError`.
7. recon_05: gate on Linux, or on macOS launch probes through a symlink/copy named after the
   probe instead of `exec -a`.
8. `UTF8_LOCALE`: pick an existing UTF-8 locale (`C.UTF-8` on Linux, `en_US.UTF-8` on darwin).
   Make `ANSI` also strip `ESC ( B` / `ESC ) 0` charset designators.
9. `test_tmux._start_command`: unescape both `\$` (3.5+) and `\\$` (3.4).
10. `test_attach` fake fzf: use `wc -c < "$TX_ARM_FILE"` instead of `stat -c %s`.
11. README: document `TMPDIR=/private/tmp` for macOS (short socket path + canonical root). Re-run
    tmuxconf_13 on tmux ≥ 3.6 before treating it as a reference or port issue.
12. Upstream reference bug: `palette.sh` `tag_color_index` should not depend on bash 3.2's
    `printf "'c"`, e.g. take the absolute value, or compute the hash via `tx`.
