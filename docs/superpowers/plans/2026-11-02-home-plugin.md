# Home Plugin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the start-menu popup with a `home` plugin whose session-manager TUI is the floor of the `home` pane: projects → agents → views, history inline, the assistant warming in the header.

**Architecture:** A new crate `plugins/home` (`tx-home`) created by moving `plugins/menu` and then reshaping it: a pure model (`Context` → rows, keys → `Outcome`), a data gatherer over `TxCore` and tmux with a worker thread, a ratatui drawer, and verbs. The pane's floor is tmux state, not a parent process: `remain-on-exit on` plus a `pane-died` hook that runs `tx _home-perform`, which reads the TUI's choice file, nests into the picked session or runs the picked `tx` verb, and respawns the TUI when whatever ran in the pane exits. `tx-client`'s `attach` becomes glue that creates `home` and hands it to the plugin's `_home-install` verb.

**Tech Stack:** Rust 2024 (rust-version 1.92), ratatui 0.30, crossterm (via ratatui), tmux 3.6, serde_json, the workspace crates `tx-core`, `tx-client`, `tx-chat`, `tx-engine-claude`.

**Spec:** `docs/superpowers/specs/2026-11-02-home-plugin-design.md`

## Global Constraints

- Plugin boundary (D3): the home reads `TxCore` and tmux; it acts only through `tx` verbs and tmux commands; it never writes agent records or other sessions' tmux options.
- The builder never commits: after each task, stop for the operator's review (the tmux diff hook shows the change) and their commit; then dispatch a fuzz/property-test subagent on that task's pure code before the next task.
- `cargo check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass at the end of every task.
- Full variable names, no abbreviations; no defensive checks on internal interfaces; no exceptions for control flow.
- Colours: the palette constants moved from `menu::ui` (`ACCENT`, `INFO`, `WARN`, `FG`, `DIM_FG`), each with a `Color::Indexed` fallback; no idle animation; spinner frames at 4 Hz only on `◐` rows.
- Keys mirror the prefix table where one exists; footer is context-sensitive; 400 ms grace after an attach-triggered open.
- First paint of the TUI under 100 ms after attach: no git, transcript or artifact read happens before the first draw.
- Trust seeding edits an existing Claude config file only, adds one key, is idempotent, never creates the file.

## Review Focus

1. A session name containing characters tmux formats expand (`#`, `'`) — nesting and `respawn-pane` must quote (Task 9 integration test uses the name `a#1`).
2. A worktree whose repository was deleted (`git rev-parse` fails) — the agent must still appear, under a project named after the worktree's parent directory (Task 7 test).
3. A transcript that is missing or empty — the unfolded detail shows `—` for the last exchange and never blocks the draw (Task 7 test).
4. Two `tx ui attach` within the warm window, or a popup opened during the warm — exactly one assistant record (Task 4 concurrency test; Task 9 attach-twice test).
5. The terminal narrower than 60 columns or shorter than 8 rows — the drawer clips and never panics (Task 8 `a_tiny_area_never_panics`, carried from the menu).

---

### Task 1: Move the transcript readers into `tx-chat`

**Files:**
- Create: `plugins/chat/src/transcript/mod.rs`, `plugins/chat/src/transcript/rows.rs`, `plugins/chat/src/transcript/metadata.rs`, `plugins/chat/src/transcript/clock.rs` (moved from `plugins/timeline/src/{rows,metadata,clock}.rs`)
- Modify: `plugins/chat/src/lib.rs` (add `pub mod transcript;`), `plugins/chat/Cargo.toml` (add `regex = "1"`), `plugins/timeline/src/lib.rs` (replace the three `pub mod` lines with `pub use tx_chat::transcript::{clock, metadata, rows};`), `plugins/timeline/Cargo.toml` (drop `regex` if nothing else uses it)
- Test: existing unit tests travel with the modules; `cargo test -p tx-chat -p tx-timeline`

**Interfaces:**
- Produces: `tx_chat::transcript::rows::{Kind, Row, parse_line, parse_entry, clean_user, cap}`, `tx_chat::transcript::metadata::Metadata` (`update(&Value)`, `with_launch_fallback`, `model`, `effort`), `tx_chat::transcript::clock::parse_iso` — unchanged signatures.

- [ ] **Step 1: Run the timeline tests to record the baseline**

Run: `cargo test -p tx-timeline 2>&1 | tail -3`
Expected: all pass (note the count).

- [ ] **Step 2: Move the three files with `git mv` into `plugins/chat/src/transcript/`, add `mod.rs` declaring them, fix `use crate::clock::parse_iso` → `use super::clock::parse_iso`, add `pub mod transcript;` to chat's lib.rs and `regex` to its Cargo.toml**

- [ ] **Step 3: In `plugins/timeline/src/lib.rs` replace `pub mod clock; pub mod metadata; pub mod rows;` with `pub use tx_chat::transcript::{clock, metadata, rows};`; keep every `super::rows::…` import in the timeline compiling (they resolve through the re-export)**

- [ ] **Step 4: Run the tests**

Run: `cargo test -p tx-chat -p tx-timeline 2>&1 | tail -5`
Expected: the moved tests pass under `tx-chat`, the timeline count equals the baseline minus the moved tests.

- [ ] **Step 5: Clippy, then stop for the operator's review and commit; dispatch the fuzz subagent on `transcript::rows::parse_line` (random JSONL lines never panic)**

---

### Task 2: `_new-agent --cwd`

**Files:**
- Modify: `plugins/spawn/src/lib.rs:263-290` (argv parsing of `_new-agent`)
- Test: `tx-core-cli/tests/spawn.rs`

**Interfaces:**
- Produces: `tx _new-agent <pane> <client> [--cwd DIR]`; with `--cwd`, the form's project defaults come from `DIR` instead of the pane's `#{pane_current_path}`.

- [ ] **Step 1: Write the failing test `new_agent_form_takes_its_project_from_cwd` in `tests/spawn.rs`: run `_new-agent <pane> <client> --cwd <repository>` with the form shim the suite already uses, assert the recorded form spec's project path equals `<repository>` (find how the existing tests inspect the form spec and reuse it)**

- [ ] **Step 2: Run it; expected FAIL with the usage line `usage: tx _new-agent <pane> <client>`**

- [ ] **Step 3: Extend the argv match to `[pane, client]` and `[pane, client, flag, directory] if flag == "--cwd"`; `path` is `directory` when given; update the usage strings to `_new-agent <pane> <client> [--cwd DIR]`**

- [ ] **Step 4: Run `cargo test -p tx-core-cli --test spawn`; expected PASS**

- [ ] **Step 5: Clippy; stop for review and commit**

---

### Task 3: Claude trust seeding helper

**Files:**
- Create: `plugins/engine-claude/src/trust.rs`
- Modify: `plugins/engine-claude/src/lib.rs` (`pub mod trust;`, `pub fn claude_config_file_from_env() -> PathBuf`)
- Test: unit tests in `trust.rs` with `tempfile`

**Interfaces:**
- Produces:
  - `pub fn claude_config_file_from_env() -> PathBuf` — `$CLAUDE_CONFIG_DIR/.claude.json` when the variable is set and non-empty, else `$HOME/.claude.json`.
  - `pub enum Trusted { Added, AlreadyTrusted, NoConfigFile }`
  - `pub fn trust_project(config_file: &Path, project: &Path) -> Result<Trusted, TrustError>` — sets `projects["<project>"].hasTrustDialogAccepted = true` in the JSON object at `config_file`, preserving everything else (serde_json `preserve_order` is on in the workspace), writing atomically (write a sibling temp file, rename). `TrustError` wraps io and JSON errors with the path.

- [ ] **Step 1: Write the failing tests in `trust.rs`:**
  - `a_missing_file_is_left_alone` — no file → `Ok(NoConfigFile)` and still no file.
  - `trust_is_added_under_projects_and_nothing_else_changes` — file `{"numStartups":3,"projects":{"/other":{"allowedTools":[]}}}` → `Added`; re-read JSON equals the original plus `projects["/p"] == {"hasTrustDialogAccepted": true}`; `numStartups` still 3.
  - `an_existing_project_entry_gains_only_the_key` — `projects["/p"] = {"allowedTools":["Bash"]}` → `Added`; entry becomes `{"allowedTools":["Bash"],"hasTrustDialogAccepted":true}`.
  - `a_second_call_is_a_no_op` — second call → `AlreadyTrusted`, file bytes unchanged.
  - `a_non_object_file_is_an_error` — file `[]` → `Err`.

- [ ] **Step 2: Run `cargo test -p tx-engine-claude trust`; expected FAIL (module missing)**

- [ ] **Step 3: Implement `trust.rs` and `claude_config_file_from_env`**

- [ ] **Step 4: Run the tests; expected PASS**

- [ ] **Step 5: Clippy; stop for review and commit; fuzz subagent: arbitrary JSON objects in, the only diff is the one key**

---

### Task 4: `assistant warm`: lock, respawn-ended, failure line, trust

**Files:**
- Modify: `plugins/assistant/src/lib.rs` (`warm`, `ensure_home`, new `warm_lock`, `FAILURE_FILE`), `plugins/assistant/Cargo.toml` (add `tx-engine-claude = { path = "../engine-claude" }`)
- Test: `tx-core-cli/tests/assistant.rs`

**Interfaces:**
- Consumes: Task 3's `trust_project`, `claude_config_file_from_env`.
- Produces:
  - `pub fn warm(core: &TxCore) -> Result<AgentId, CoreError>` — unchanged signature; now: take `<data>/plugins/assistant/warm.lock` (`std::fs::File::lock`), then `running()`; if none, `latest_ended()` (the newest `Ended`, non-archived record with the role) → `core.respawn_agent(&id)`; else `create_agent`. Release on return.
  - `pub const FAILURE_FILE: &str = "warm.failed"` under the plugin data dir: the verb writes the error's display text there on failure and removes the file on success (the home header reads it).
  - `ensure_home` calls `trust_project(&claude_config_file_from_env(), &home)` right after the first commit; its result is ignored except an `Err`, which is written to stderr and does not fail `ensure_home`.

- [ ] **Step 1: Rewrite `warm_restarts_a_vanished_assistant` in `tests/assistant.rs` as `warm_respawns_a_vanished_assistant_in_place`: after `kill-session`, the second warm prints `tx-assistant\n`, `ls --tag tx-system` has one line, `cwd_of("tx-assistant")` is unchanged and `note.txt` survives, state is not `ended`.**

- [ ] **Step 2: Add `two_concurrent_warms_make_one_assistant`: spawn two `assistant warm` processes at once (std threads running `cli.command`), both exit 0, both print the same name, `ls --tag tx-system` has one line.**

- [ ] **Step 3: Add `a_failed_warm_leaves_its_reason_in_the_plugin_dir`: run warm with the roles not installed (as `warm_without_the_roles_installed_fails` does); assert `<data>/plugins/assistant/warm.failed` exists and contains `COMMON`; then `ui install` + warm succeeds and the file is gone.**

- [ ] **Step 4: Add `ensure_home_seeds_claude_trust`: with `HOME` pointed at a temp dir holding `.claude.json` = `{"projects":{}}`, run warm; assert the file now has `projects["<data>/plugins/assistant/home"].hasTrustDialogAccepted == true`. (The suite sets `HOME` per test through `Cli::command`; check `common/mod.rs:79-107` for the env it passes and extend if `HOME` is not among them.)**

- [ ] **Step 5: Run `cargo test -p tx-core-cli --test assistant`; expected: the four new/changed tests FAIL**

- [ ] **Step 6: Implement: `warm_lock(core) -> io::Result<File>`, `latest_ended(core) -> Result<Option<AgentId>, CoreError>` (same tag/role filter as `running`, over `ListFilter::new().tag(TAG).ended()`, max by `last_modified_at`), the respawn branch via `core.respawn_agent` (confirm the core method name with `grep -n "pub fn respawn" tx-core/src/core.rs`), the failure file in the verb's `warm` arm, the trust call in `ensure_home`**

- [ ] **Step 7: Run the assistant suite; expected PASS. Then `cargo test -p tx-core-cli --test ide` still passes (attach is untouched yet).**

- [ ] **Step 8: Clippy; stop for review and commit; fuzz subagent on `latest_ended` ordering with synthetic records**

---

### Task 5: Move `plugins/menu` to `plugins/home` (pure rename)

**Files:**
- Move: `plugins/menu/` → `plugins/home/` (`git mv`), crate `tx-home`, plugin id `home`
- Modify: `Cargo.toml` members (`plugins/menu` → `plugins/home`), `tx-core-cli/Cargo.toml` (dependency), `tx-core-cli/src/plugins.rs` (`tx_home::HomePlugin`; `RETIRED` gains `"menu"`), `plugins/home/tmux/keys.tmux` (verb names), `plugins/home/src/verbs.rs` (verb names), `plugins/home/src/lib.rs` (`PLUGIN_ID = "home"`, `HomePlugin`)
- Move: `tx-core-cli/tests/menu.rs` → `tests/home.rs`; update `tests/ide.rs` strings (`menu.tmux` → `home.tmux`, `("menu", "keys=m")` → `("home", "keys=m")`, `_menu-open` → `_home-open`, `_menu-first` → `_home-first`, the plugin-id lists), `tests/plugins.rs` if it lists ids

**Interfaces:**
- Produces: verbs `home` (was `menu`), `_home-open`, `_home-first`, `_home-tui`; resource `keys/home.tmux`; option `@tx-home-shown` (was `@tx-menu-shown`). Behaviour identical to the menu.

- [ ] **Step 1: `git mv plugins/menu plugins/home && git mv tx-core-cli/tests/menu.rs tx-core-cli/tests/home.rs`; rename crate, plugin, verbs, option, resource; `sed` the test strings**

- [ ] **Step 2: Add `"menu"` to `RETIRED` in `plugins.rs` and extend `a_retired_id_is_skipped_with_its_settings` to include `menu: { enabled: false }` loading without error**

- [ ] **Step 3: Run `cargo test --workspace 2>&1 | grep -E "test result|FAILED|panicked" | head`; expected: everything green (the menu suite now under `home`)**

- [ ] **Step 4: Clippy; stop for review and commit**

---

### Task 6: The session-manager model

**Files:**
- Rewrite: `plugins/home/src/model.rs`
- Keep: `accept`, `GRACE`, `Outcome`, the `Plugins`/`Keys` sub-views and `KeyHint`
- Test: unit tests in `model.rs`

**Interfaces:**
- Produces (all pure):
  ```rust
  pub struct Project { pub name: String, pub path: PathBuf }
  pub struct AgentRow { pub id: String, pub name: String, pub tag: String, pub state: LlmEngineState,
      pub age_at: f64, pub branch: Option<String>, pub chats: usize, pub project: usize /* index */,
      pub is_assistant: bool }
  pub struct ViewRow { pub name: String, pub parent: Option<String> /* agent id */, pub project: Option<usize> }
  pub struct Detail { pub roles: Vec<String>, pub model: Option<String>, pub effort: Option<String>,
      pub parent_name: Option<String>, pub last_user: Option<String>, pub last_agent: Option<String>,
      pub artifacts: Vec<(String /*kind*/, usize)>, pub started_at: f64, pub ended_at: Option<f64> }
  pub enum AssistantState { Absent, Creating, Starting, Ready, Stopped { id: String }, Failed(String) }
  pub struct Context { pub projects: Vec<Project>, pub agents: Vec<AgentRow>, pub views: Vec<ViewRow>,
      pub details: BTreeMap<String, Detail> /* by agent id, filled lazily */, pub assistant: AssistantState,
      pub plugins: Vec<(String, bool)>, pub keys: Vec<KeyHint>, pub now: f64 }
  pub enum Row<'a> { Project(&'a Project), Agent(&'a AgentRow), Detail(&'a AgentRow, Option<&'a Detail>),
      Views(&'a [&'a ViewRow]), EndedFold { project: usize, hidden: usize }, FirstRun }
  pub enum Action { Attach(String /*session name*/), Resume(String /*id*/), New(PathBuf), Stop(String),
      Ask(String), Artifacts(String), Timeline, Tutor }  // encode/decode as today; `new <cwd>`, `stop <id>`, `ask <text>` added
  pub enum Confirm { Stop(String) }
  pub struct Home { pub ctx: Context, /* private: cursor, unfolded: BTreeSet<String>, ended_shown: bool,
      filter: Option<String>, view: View, confirm: Option<Confirm> */ }
  impl Home {
      pub fn new(ctx: Context) -> Self;
      pub fn rows(&self) -> Vec<Row<'_>>;           // projects ordered by newest activity; tx-system last; views pseudo-project trailing
      pub fn cursor(&self) -> Option<usize>;        // index into rows(); never on Project/EndedFold/FirstRun
      pub fn select_session(&mut self, name: &str); // --returned-from
      pub fn is_first_run(&self) -> bool;           // no agent other than the assistant and no views
      pub fn key(&mut self, code: KeyCode) -> Outcome;
      pub fn footer(&self) -> String;               // context-sensitive hint line
      pub fn needs_detail(&self) -> Option<String>; // the unfolded agent id whose Detail is missing (data layer fills it)
      pub fn replace_context(&mut self, ctx: Context); // keeps cursor by session name, unfold set, filter
  }
  pub const ENDED_SHOWN: usize = 3;
  ```
- Key table (main list; `/` is ask and `f` is find — the spec's resolution of the two `/` uses): `j`/`k`/arrows move; `Enter` → `Attach(name)` for running agent or view, `Resume(id)` for ended; `l`/`→` unfold, `h`/`←` fold; `Tab` toggles `ended_shown`; `n` → `New(project.path)` (cursor's project, else `$HOME`); `x` → sets `confirm`, then `y` → `Stop(id)`, anything else clears; `r` → `Resume(id)` on ended agent or stopped assistant; `a` → `Artifacts(id)`; `t`/`T` → `Timeline`; `?`/`K` → `View::Keys(0)`; `p` → `View::Plugins(0)`; `L` → `Tutor`; `f` → find mode (chars append, Backspace, Esc clears and leaves, Enter leaves keeping the filter); `/` → ask mode (same input; Enter → `Ask(text)` when non-empty, Esc cancels); `q`/`Esc` → `Outcome::Close` only when `popup` (a `Home::new_popup` flag), else `Stay`.

- [ ] **Step 1: Write the failing tests (replace the menu's):**
  - `projects_are_ordered_by_newest_activity_with_tx_system_last`
  - `ended_agents_fold_past_three_per_project_and_tab_unfolds`
  - `the_cursor_skips_project_rows_and_wraps`
  - `enter_attaches_running_and_resumes_ended`
  - `unfold_adds_a_detail_row_under_the_agent_and_needs_detail_reports_it`
  - `n_uses_the_cursors_project_path`
  - `x_then_y_stops_x_then_other_does_not`
  - `find_keeps_rows_matching_name_tag_project_or_branch`
  - `slash_then_enter_asks_and_esc_cancels`
  - `first_run_shows_the_three_verbs_and_the_assistant_row`
  - `returned_from_selects_that_session`
  - `replace_context_keeps_cursor_unfold_and_filter`
  - `q_closes_only_the_popup_variant`
  - `action_round_trips_through_encode_decode` (all eight variants)
  - keep `accept`'s grace tests.

- [ ] **Step 2: Run `cargo test -p tx-home model`; expected FAIL (types missing)**

- [ ] **Step 3: Implement `model.rs` to the interface above**

- [ ] **Step 4: Run the model tests; expected PASS**

- [ ] **Step 5: Clippy (`ui.rs`/`verbs.rs` will not compile yet against the new model — gate them: keep the crate compiling by adapting `verbs.rs` minimally to construct `Home` with an empty `Context` and `ui.rs` to draw only `View::Keys`/`View::Plugins`; the real drawing is Task 8). Stop for review and commit; fuzz subagent: random key sequences over random contexts never panic and the cursor is always on a selectable row.**

---

### Task 7: Data gathering

**Files:**
- Rewrite: `plugins/home/src/data.rs`
- Create: `plugins/home/src/detail.rs` (transcript + artifacts), `plugins/home/src/project.rs` (git lookups with caches)
- Test: unit tests in `project.rs` (temp git repos) and `detail.rs` (temp transcripts); `data.rs` keeps `key_hint` and its tests

**Interfaces:**
- Consumes: Task 1's `tx_chat::transcript`, Task 6's `Context`, `Detail`, `AssistantState`.
- Produces:
  ```rust
  // project.rs
  pub struct ProjectCache { /* worktree → (main checkout, branch, head mtime) */ }
  impl ProjectCache {
      pub fn new() -> Self;
      pub fn main_checkout(&mut self, worktree: &Path) -> PathBuf; // `git -C <w> rev-parse --git-common-dir` → its parent; on failure, worktree.parent()
      pub fn branch(&mut self, worktree: &Path) -> Option<String>;  // `git symbolic-ref --short HEAD`, re-read when <w>/.git's HEAD mtime changes; None when detached or failed
  }
  // detail.rs
  pub fn last_exchange(transcript: &Path) -> (Option<String>, Option<String>); // last Kind::User text (clean_user, first line, cap 120) and last Kind::Agent first line; (None, None) when missing/empty
  pub fn artifact_counts(core: &TxCore, id: &str) -> Vec<(String, usize)>;   // runs `tx artifact ls --session <id>` via core.verb("artifact") in-process; counts by kind column; empty on any error
  pub fn detail(core: &TxCore, cache: &mut ProjectCache, agent: &Agent) -> Detail;
  // data.rs
  pub fn context(core: &TxCore, ui: &Ui, cache: &mut ProjectCache, previous: Option<&Context>) -> Result<Context, CoreError>;
  pub fn assistant_state(core: &TxCore) -> AssistantState; // by tag tx-system + role TX-ASSISTANT: none → Absent unless `<data>/plugins/assistant/warm.failed` exists → Failed(text); Starting → Starting; Idle|Working → Ready; Ended → Stopped{id}
  ```
  `context` carries over `previous.details` so unfolded details survive polls; `views` come from `tx_client::views::list(ui)` minus `home` and kinds `Mockup` owned by the home itself; `chats` is the running count of `Kind::is_message` rows the detail layer computed, else 0.

- [ ] **Step 1: Write the failing tests:**
  - `project.rs`: `a_worktree_resolves_to_its_main_checkout` (init repo, `git worktree add`), `a_deleted_repository_falls_back_to_the_parent_directory`, `branch_follows_head_changes`.
  - `detail.rs`: `last_exchange_reads_the_newest_user_and_agent_lines` (write three JSONL entries in Claude's shape — copy the fixture style from `plugins/timeline` tests), `a_missing_transcript_gives_none_none`, `an_empty_transcript_gives_none_none`.
  - `data.rs`: `assistant_state_maps_record_states` using `Cli`-free unit construction is not possible → test through the binary in Task 9 instead; here test `key_hint` as before.

- [ ] **Step 2: Run `cargo test -p tx-home project detail`; expected FAIL**

- [ ] **Step 3: Implement the three modules**

- [ ] **Step 4: Run; expected PASS**

- [ ] **Step 5: Clippy; stop for review and commit; fuzz subagent on `last_exchange` with random JSONL**

---

### Task 8: Drawing

**Files:**
- Rewrite: `plugins/home/src/ui.rs` (keep `draw_list`, `draw_plugins`, `draw_keys`, `put`, `truncate`, the palette; delete the logo, shimmer, `FRAMES`, `draw_history`)
- Test: unit tests in `ui.rs` rendering into a `Buffer` (the menu's style)

**Interfaces:**
- Consumes: Task 6's `Home`, `Row`, `AssistantState`.
- Produces: `pub fn draw(home: &Home, frame: usize, area: Rect, buf: &mut Buffer)`; `pub const FPS: u64 = 4` (spinner only); layout: row 0 header, rows 1..h-1 list (scrolled so the cursor is visible), last row footer (or the filter input when filtering, or the `stop <name>? y/n` confirm). Glyphs: `●` green working, `◐` yellow starting (spinner frames `◐◓◑◒`), `○` dim idle/ended, `✗` red failed; view rows prefixed `⌐`; project rows bold name + dim path; detail rows indented six columns, dim labels; the ended fold line `… N more ended · Tab`; first run: the three verbs with their one-line explanations exactly as in the spec's "First run".

- [ ] **Step 1: Write the failing tests (replace the menu's snapshot tests):** `header_shows_counts_and_assistant_state`, `a_project_row_is_bold_with_a_dim_path`, `an_unfolded_agent_shows_its_detail_rows`, `the_cursor_row_is_reversed`, `the_footer_changes_when_finding_asking_and_confirming`, `first_run_screen_lists_the_three_verbs`, `the_list_scrolls_to_keep_the_cursor_visible`, `a_tiny_area_never_panics` (5×3), `keys_view_shows_the_hints`, `plugins_view_marks_disabled_ones`.

- [ ] **Step 2: Run `cargo test -p tx-home ui`; expected FAIL**

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run; expected PASS**

- [ ] **Step 5: Clippy; stop for review and commit; fuzz subagent: random `Context` × random `Rect` sizes never panic**

---

### Task 9: The floor: `_home-install`, `_home-perform`, `_home-tui`

**Files:**
- Rewrite: `plugins/home/src/verbs.rs`; modify `plugins/home/src/run.rs` (poll loop with a worker thread and the 1 s refresh)
- Create: `plugins/home/src/floor.rs` (tmux side: remain-on-exit, hook, respawn, choice file paths)
- Modify: `tx-client/src/tmux.rs` (add `pub fn nest_attach_only_command(&self, target: &str) -> String` — the attach without the trailing shell)
- Test: `tx-core-cli/tests/home.rs`

**Interfaces:**
- Consumes: Tasks 6–8; `Ui::respawn_pane`, `Ui::set_session_option`, `Ui::hosts_nested_attaches`.
- Produces:
  - `tx _home-install <session>` — on the view `<session>` (kind `home`): `set-window-option -t =<s>: remain-on-exit on`; `set-hook -t =<s>: pane-died "run-shell -b '<tx> _home-perform #{hook_pane}'"` (if `#{hook_pane}` is empty in a `pane-died` hook on tmux 3.6, fall back to `#{pane_id}`; the integration test decides); then if the pane is dead, or its command is in `HOSTING_COMMANDS`, `respawn-pane -k` it with `<tx> _home-tui <pane> --first`. Prints nothing; exit 0. Disabled plugin ⇒ the verb does not exist and `attach` (Task 10) skips it.
  - `tx _home-tui <pane> [--first] [--returned-from S] [--popup] [--choice FILE]` — runs the TUI in the current terminal; on exit writes the action to `FILE` (default `<data>/plugins/home/choice.<pane digits>`), or deletes it when nothing was chosen. `--first` applies the grace and spawns `tx assistant warm` detached (`Command::new(current_exe).args(cli_arguments).arg("assistant").arg("warm")`, stdio null, `process_group(0)`) when `core.verb("assistant")` exists and `assistant_state` is `Absent`. `--returned-from` calls `select_session`. `--popup` makes `q`/`Esc` close.
  - `tx _home-perform <pane>` — reads and deletes the choice file; then: `attach <name>` → `respawn_pane(pane, nest_attach_only_command(target))` (target from `live_targets`), `resume <id>` → `tx respawn <id>` then attach by the printed name the same way, `new <cwd>` → the spawn form popup with `--cwd` then respawn the TUI, `stop <id>` → `tx stop <id>` then TUI, `ask <text>` → `tx assistant ask <text>` then attach the assistant by name the same way, `artifacts <id>` → `_artifact-menu` as the menu did, `timeline` / `tutor` → as the menu did; no choice → respawn the TUI with `--returned-from <inner>` where `<inner>` is the session the pane was showing (`inner_for_pane` captured before the pane died is not available; pass the last `attach` target the perform wrote to `<data>/plugins/home/last.<pane digits>`).
  - `run::run(home: &mut Home, first: bool) -> io::Result<Option<Action>>` — event loop at 4 fps while any `◐` row exists, else blocks on events with a 1 s timeout; every second (and after `needs_detail` changes) asks the worker for a fresh `Context` / `Detail` over an `mpsc` channel and `replace_context`s it.

- [ ] **Step 1: Write the failing integration tests in `tests/home.rs` (reuse `installed_server`, `screen`, `press`, `clients` from the old suite; `home_shown(cli)` = screen contains `tx-ide` header and the footer `enter attach`):**
  - `home_install_turns_a_shell_home_into_the_tui`: create `home` with `sleep 600`? no — with `sh`; run `_home-install home`; wait until the pane command is `tx`; `show-hooks -t =home:` contains `_home-perform`; `remain-on-exit` is `on`.
  - `enter_nests_into_the_picked_session_and_detach_returns_to_the_tui`: `cli.create("a#1")` (name with `#`), attach the outer pane to `home`, wait for `home_shown`, press `j` until the row `a#1` is reversed (or `f`, type `a#1`, Enter), press Enter; wait for `clients == ["a#1","home"]`; `detach-client -s a#1`... (the nested client is the one attached to `a#1`: `tmux detach-client -t <that client>`); wait for `home_shown` again and the `a#1` row reversed.
  - `a_picker_enter_then_exit_returns_to_the_tui`: from the TUI, `respawn-pane -k` the home pane with the picker's script shape `bash -c 'TMUX= tmux -S … attach -t =a; exec sh'`; kill session `a`; type `exit` Enter into the home pane; wait for `home_shown`.
  - `the_first_open_warms_the_assistant_once_and_the_header_follows`: with the fake claude on PATH and `ui install`, run `_home-install` twice quickly; wait up to 30 s for `ls --tag tx-system` to have one line and the header to contain `assistant ● ready`.
  - `x_then_y_stops_the_selected_agent`: create `b`, select, `x`, `y`; wait for `state_of(b) == "ended"` and the row greyed with `ended`.
  - `the_popup_variant_closes_on_q_and_attach_switches_the_client`: bind check is Task 11; here run `_home-tui %pane --popup` in a `display-popup` by hand and assert `q` closes it.

- [ ] **Step 2: Run `cargo test -p tx-core-cli --test home`; expected FAIL (verbs missing)**

- [ ] **Step 3: Implement `floor.rs`, the verbs, the run loop, and `Ui::nest_attach_only_command` (`set -m; TMUX= tmux -S <sock> attach -t <target>` as a `bash -c` command, quoted with `shell_quote`)**

- [ ] **Step 4: Run the home suite; expected PASS. Record the first-paint time: `tests/home.rs::the_tui_paints_within_100ms` — from `respawn-pane` to `home_shown` under 0.5 s in CI conditions (the budget in the spec is 100 ms on a quiet machine; assert 500 ms to stay green under load and print the measured value).**

- [ ] **Step 5: Clippy; stop for review and commit; fuzz subagent on `Action::decode` and the choice-file paths (pane ids with odd characters)**

---

### Task 10: `tx ui attach` becomes glue

**Files:**
- Modify: `tx-client/src/install.rs:365-500` (`warm_assistant` removed, `ensure_home` creates the shell home and names the window `home`, `attach` runs `core.verb("_home-install")` in-process when present), README is Task 12
- Test: `tx-core-cli/tests/ide.rs`

**Interfaces:**
- Consumes: Task 9's `_home-install <session>`.
- Produces: `attach` = `ensure_home` (window named `home`: `views::open` then `rename-window -t =home: home`) → `_home-install home` if the verb exists → the existing attach/switch logic. No `assistant` call anywhere in `tx-client`.

- [ ] **Step 1: Rewrite the ide tests:** `ui_attach_starts_the_assistant_and_nests_home_into_it` → `ui_attach_lands_on_the_home_tui_and_the_assistant_comes_up_behind_it` (clients == `["home"]` within 5 s and the screen shows the header; within 30 s `ls --tag tx-system` has one running line and the header says `● ready`); `ui_attach_without_the_roles_installed_opens_a_plain_home` → the header says `assistant ✗ failed: … COMMON …`; `ui_attach_warms_the_assistant_when_home_already_exists` → `ui_attach_installs_the_floor_on_an_existing_shell_home` (an existing `home` running `sh` gets the hook and the TUI); add `ui_attach_with_home_disabled_opens_a_shell` (plugins.yaml disables `home`; pane command is the shell; no hook). Window name assertion `#{window_name} == "home"` in `ui_attach_lands_in_a_view_home`.

- [ ] **Step 2: Run `cargo test -p tx-core-cli --test ide`; expected FAIL**

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run ide + home + assistant suites; expected PASS**

- [ ] **Step 5: Clippy; stop for review and commit**

---

### Task 11: `prefix+m` opens the home in a popup; the first-attach hook is gone

**Files:**
- Modify: `plugins/home/tmux/keys.tmux` (bind `m` → `_home-open '#{pane_id}' '#{client_name}'`; delete the `client-attached` hook, the `@tx-home-shown` scan, and the `# unhook:` line), `plugins/home/src/verbs.rs` (`_home-open` runs `_home-tui <pane> --popup --choice <tmp>` in the 80 % popup and performs: `attach` → `switch-client`, the rest as `_home-perform` minus the respawn), delete `_home-first` and `SHOWN_OPTION`
- Test: `tests/home.rs`, `tests/ide.rs` (`show-hooks -g client-attached` no longer contains a home hook; the installed-conf expectation drops `unhook`)

- [ ] **Step 1: Rewrite the tests:** `ui_install_binds_prefix_m_and_the_first_attach_hook` → `ui_install_binds_prefix_m_only` (bound `m` contains `_home-open`; `show-hooks -g client-attached` is empty of `_home`); delete `enabling_the_home_on_a_live_server_counts_the_attached_client_as_first` and `the_home_pops_up_on_the_first_attach_only`; `prefix_m_opens_the_menu_without_disabled_plugins_items` → `prefix_m_opens_the_home_popup_and_q_closes_it`; add `prefix_m_attach_switches_the_client` (from an agent pane, pick another agent, `list-clients` shows the switch and the home pane still runs `tx`).

- [ ] **Step 2: Run; expected FAIL**

- [ ] **Step 3: Implement**

- [ ] **Step 4: Run home + ide suites; expected PASS**

- [ ] **Step 5: Clippy; stop for review and commit**

---

### Task 12: Docs, sweep, final review

**Files:**
- Modify: `README.md` (plugin table row `menu` → `home`; the `ui attach` paragraph at 445–455; the `prefix+m` key row; the `menu` entry in the plugin verbs table around line 346), `plugins/home/src/lib.rs` crate docs, `plugins/home/Cargo.toml` description
- Test: whole workspace

- [ ] **Step 1: Update the README per the spec's "Boundary changes"; grep the repo for `tx-menu`, `_menu-`, `@tx-menu` and `start menu` and fix every hit (`docs-site/` excluded)**

- [ ] **Step 2: `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`; expected green**

- [ ] **Step 3: Rehearse the real path once on a throwaway app (`unset TX_*; TX_APPNAME=tx-ux-check target/debug/tx ui install && tx ui attach` from a scratch tmux, as in the measurement run): first paint < 100 ms, header goes `starting` → `ready`, Enter on the assistant nests, detach returns, `prefix+m` pops the same screen. Record the timings in the task's review note.**

- [ ] **Step 4: Stop for the operator's review and commit; then the whole-branch review per the execution method chosen**
