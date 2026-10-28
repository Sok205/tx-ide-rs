# poc backlog: code quality

Findings from review rounds over the poc workspace. Fix in priority order (HIGH, MEDIUM, LOW),
one commit per group, with `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo test --workspace` green after each. Each review round reads this file first and adds
only new findings.

Legend: **bug** fixes wrong behaviour; **behaviour** changes observable output, so it needs a
decision or a test update; everything else is a behaviour-preserving refactor.
Status: `[ ]` open, `[x]` done (commit hash), `[?]` needs a decision.

## Round 1 (2026-10-28): tx-core, tx-ui, cross-plugin duplication

### HIGH

- [x] (59f2129) **H1 bug, behaviour.** Commands that re-invoke `txc` are built 8 ways. Picker, menu and
  chat omit `--socket`, so with a custom socket their binds and popups reach the wrong tmux
  server. The picker also does not shell-quote, so paths with spaces break. One shared
  `tx_ui::txc::{exe, shell, command}` built from artifacts' `txc_command`.
  Copies: artifacts/menu.rs `txc_command`, mockup/lib.rs `viewer_command`, timeline/verbs.rs
  `resident_command`, menu/verbs.rs `Txc`, chat/fork_pane.rs, chat/ops.rs `finish_command`,
  chat/detach.rs, picker/lib.rs `tx_command`. Also 13 hand-written `current_exe()` lookups.
- [x] (20bfcb2) **H2.** Listing sessions reads every record twice and handles a vanished record 3 ways.
  Add `TxCore::sessions(&ListFilter) -> Result<Vec<Session>, CoreError>` over `store.all()`.
  13 callers: artifacts verbs/menu/views, picker `running`, labels, hooks status, tutor, chat
  ops (2) / ext / history_verbs, menu data, timeline `load_sessions`.
- [ ] **H3 bug.** hooks/install.rs is an older fork of chat/install.rs: fixed temp name, no
  fsync, a same-second backup overwrites the last. Move chat's installer (plus `read_payload`,
  `home_dir`, `shell_quote`) to `tx_core::agent_hooks`; replace the `_history-ingest` special
  case with an `Entries` field. Behaviour: hooks' uninstall message wording.
- [x] (3ac1400) **H4.** `failed(impl Display) -> VerbError` copied 6 times plus 18 inline `map_err`s.
  Add `VerbError::failed` in tx-core plugin.rs.
- [x] (3ac1400) **H5.** stdout write helpers copied 7 times (`write_out`, `say`, `write_failed`) plus 3
  inline. Add `tx_core::say(out, text)` and `VerbError::write(io::Error)`.
- [x] (3ac1400) **H6.** Plugin data dir `state_dir/plugins/<id>` built 4 times (artifacts, chat, tutor,
  picker); timeline imports chat's just for this. Add `CoreConfig::plugin_dir(plugin)`.
- [x] **H7 bug.** (fixed 5dccbb8) Ellipsis truncation copied 5 times; artifacts render/menu and chat
  history_verbs compute `width - 1` on `usize` and panic at width 0. One `tx_ui::render::trunc`
  (`saturating_sub`) and `pad`. Keep `py_prefix`.
- [x] (d0824cb) **H8.** `$TX_SESSION_ID` read 5 times, the constant declared 3 times (hooks state.rs,
  chat history_verbs.rs shadow `tx_core::core::SESSION_ID_ENV`). Add
  `tx_ui::env_session_id()`; mockup's `agent_from_env` re-implements `Ui::inside()`.
- [x] **H9 bug.** (fixed 5dccbb8) tx-ui popup.rs `run_shell_background` duplicates `Ui::run_shell_background`
  without `#` to `##` escaping; a `#` in the state dir is read as a tmux format. Delete it.
- [ ] **H10.** Atomic file writes implemented 7 times (tx-core store, artifacts store, tutor
  progress/bundle, chat lib/install, hooks install). One `tx_core::fs::write_atomic`, with a
  permission-keeping variant for the installers.
- [x] (ad8e0d5) **H11.** tx-core store.rs `all()` stats each record, then `read()` stats it again. Map
  `ReadError::NotAFile` to skip instead; move the misplaced comment.
- [x] (ad8e0d5) **H12.** tx-core core.rs `store_io` duplicates store.rs `io_error` (4 call sites).
- [x] (ad8e0d5) **H13.** tx-core core.rs: lock, work, release, emit is written 7 times in two styles.
  One private `emitting(op)` helper; `refresh_all` keeps per-record emits.
- [x] (ad8e0d5) **H14.** tx-core core.rs `stop_session` re-implements `mark_exited`. Generalise to
  `mark_ended(session, lifecycle, event_type)`.
- [x] (ad8e0d5) **H15.** tx-core core.rs:481 dead `#[allow(unreachable_patterns)]` wildcard arm
  (`#[non_exhaustive]` has no effect inside the defining crate).
- [x] (ad8e0d5) **H16.** tx-core core.rs: the `"duplicate session"` to `NameTaken` mapping appears twice
  (create, rename). One `name_taken_or(error, name)`.
- [x] (132c7b0) **H17.** tx-core events.rs `append_at` takes its six `&str`s in a different order from
  `encode` / `line`; a swap still compiles. Reorder (callers: `append`, unit tests,
  tests/fuzz_events.rs) or pass a `Fields` struct.
- [x] (132c7b0) **H18.** tx-core tmux.rs `run` builds an unused stdout `String` (4 `.map(drop)`s);
  stderr lossy-trim copied twice. Return `()`, add `stderr_of`.
- [x] (132c7b0) **H19.** tx-core tmux.rs argument arrays are all `"x".into()`; make private `output` /
  `run` generic over `&[impl AsRef<OsStr>]` (keep pub `argv` non-generic).
- [ ] **H20 ?, behaviour.** tmux.rs `kill_session` and core.rs `session_vanished` disagree on
  "session gone": a server quitting mid-kill is an error in one, gone in the other. Decide
  whether both use one `tmux::session_gone(stderr)`.

### MEDIUM

- [ ] **M1.** tx-ui dead or crate-only pub API: delete `popup::focus_hosting_pane`; make
  `popup_args`, `popup_attach_command`, `walk_to_outer_client`, `tmux::parse_attachment_join`,
  `editor::{Form, Frame, Draw, Style, Mode, Outcome}` crate-private.
- [ ] **M2.** The exact tmux target `=name` / `=name:` is hand-built about 25 times; tx-ui
  `exact_session_pane` re-implements tx-core's private `exact_for_pane_target`. Make
  `tx_core::tmux::exact_pane` public and use it with `exact` everywhere.
- [ ] **M3.** `popup::global_option` belongs on `Ui` next to `session_option`; tutor inlines it.
  menu verbs.rs `status()` is `Ui::display_status` minus stderr (merge if the extra line is ok).
- [ ] **M4 behaviour.** chat ops.rs rebuilds tx-core's session env by hand; add
  `TxCore::session_env(id, extra)`. Override order in the `env` prefix changes.
- [x] (ad8e0d5) **M5.** tx-core core.rs `.map_err(CoreError::Lock)` 10 times; add `lock_create` /
  `lock_session`. Do not add `#[from] io::Error` to `Lock`.
- [x] (ad8e0d5) **M6.** tx-core core.rs profile path `profiles/<id>.md` built twice; add `profile_path`.
- [ ] **M7.** tx-core core.rs `create_locked` is 170 lines: redundant clones (match
  `opts.launch` by value), a second dead `unreachable!` arm (use a `Prepared` enum), split
  `agent_launch` so cleanup runs once.
- [x] (ad8e0d5) **M8.** tx-core core.rs `emit(Vec<Event>)` forces `vec![x]` 5 times; take
  `impl IntoIterator<Item = Event>`.
- [x] (ad8e0d5) **M9.** tx-core core.rs `update_ext` clones the value twice; remove first, clone once.
- [x] (132c7b0) **M10.** tx-core tmux.rs `LIST_FORMAT` hardcodes `#{@tx-id}`; add a test tying it to
  `TX_ID_OPTION`.
- [x] (4c9c8b2) **M11.** tx-core profile.rs frontmatter lines copied to `String`s then borrowed back;
  return `(skills, body)` from `read_role`; `Cow` for the `\r` strip.
- [x] (4c9c8b2) **M12.** tx-core profile.rs `is_bare_name` duplicates session.rs `is_safe_fragment`
  minus the length cap.
- [x] (4c9c8b2) **M13.** tx-core session.rs `RecordIn` and `let _ = record.version` do nothing; check the
  version on a `Map` then deserialize. Confirm against tests/fuzz_session.rs.
- [x] (4c9c8b2) **M14.** tx-core engine.rs `EngineError::Workspace { .. }` written out 4 times; add a
  constructor.
- [x] (132c7b0) **M15.** tx-core events.rs `encode` serializes up to three times; compute the skeleton
  length from `escaped_len`.

### LOW

- [x] (ad8e0d5) **L1.** tx-core core.rs `resolve` collects a `Vec` only to search it; `send_message`
  clones `opts.sender` that could move; `!= Running` vs `is_terminal()` mixed.
- [x] (ad8e0d5) **L2.** tx-core store.rs suffix slicing should be `strip_suffix`.
- [ ] **L3.** `get_session` / `get_execution_location` break the no-`get_` rule (about 124
  call sites): rename only with the next deliberate API change, with a deprecated alias.
- [x] (4c9c8b2) **L4.** tx-core locks.rs `LockGuard::is_current` can be private; test helper
  `unsafe_is_root` has no `unsafe` and a useless wrapper.
- [x] (4c9c8b2) **L5.** tx-core session.rs `SessionId: TryFrom<String>` goes through `parse`, allocating
  twice per deserialized id.
- [x] (4c9c8b2) **L6.** Use the map entry API in engine.rs `register` and plugin.rs `insert_verb`;
  plugin.rs:189 should use the `VerbTable` alias.
- [x] (132c7b0) **L7.** tx-core tmux.rs `Tmux::binary()` is pub with only a unit-test caller.
- [ ] **L8.** tx-core events.rs `now()` can be `map_or(0.0, as_secs_f64)` (last-bit change).
- [ ] **L9.** tx-ui editor.rs `Form::render` (77 lines) and `Form::handle`: extract
  `draw_field` / `handle_insert`. `out += &format!` / `push_str(&format!)` to `write!`.
- [ ] **L10.** Names: tx-ui `TAG_HEX` / `tag_hex` hold `colourN` names (`tag_colour`);
  `picker_namew` to `picker_name_width`; `Ui::quiet` returns `(bool, String)` read as `.0`
  / `.1` about 20 times.
- [ ] **L11.** 8-char short ids cut by hand 5 times; add `short_id`.
- [ ] **L20.** `current_exe:` / `could not run fzf:` / `HOME is not set` errors are
  hand-written 2-5 times each across plugins; `tx_ui::txc::exe()` exists for the first.
- [ ] **L12.** tx-core core.rs:477/543 report an empty program as `InvalidName` ("invalid
  session name"); needs its own variant (behaviour).

## Round 2 (2026-10-28): rust-skills lenses

Reviewers per skill: idiomatic-rust (I), rust-errors (E), ownership-not-clone (O),
type-driven-design + rust-api-design (T), rust-testing (Q). Already fixed in the artifacts
port (3125aa7): migrate path traversal and save order (Q1), close steps limited to temp
worktrees and the Exited path tested (Q2), unreadable store reported (E1), no panic on a
missing source file (E7, I11 half).

### HIGH

- [x] (3519d8c) **T1 bug.** The user envelope tx-core writes (`<from-user>…</from-user>`, no
  `session=`) is not parsed by chat's `match_envelope`, so user sends land as typed text
  with raw tags. The wire format is written in 4 places (tx-core messaging.rs, chat
  messages.rs dead `build_envelope` + tag constants, timeline rows.rs regex). One
  `tx_core::messaging::Envelope` with `wrap` / `parse`. Confirm with a `<from-user>hi</from-user>`
  chat-messages test first.
- [ ] **I1 / T2.** The hooks state is a string in 6 crates, `ext["hooks"]` parsed 6 ways,
  while plugins/hooks has `State`. One serde `HookState` + `Hooks { state, since }` in tx-ui
  (re-exported by hooks) or tx-core; one shared `activity_at` (chat, timeline copies).
- [ ] **T3.** `ext` is read by string key around the owners' types (`ext.edit.group` in
  artifacts and timeline while tx-edit exports `Ext`; spawn kind in tx-ui). Add
  `trait ExtRecord { const KEY }`, `Session::ext_of::<T>()`, `TxCore::update_ext_of::<T>()`.
- [x] (20bfcb2) **O1.** `TxCore::resolve` loads the record(s) then returns only the id; every caller
  loads again (worst: the once-a-second pane label resolves by name, so `store.all()` + a
  load per pane per second). Add `resolve_session` returning the record.
- [ ] **O2-O4.** Picker feed reload (every second): the sort deserializes `ext.hooks` per
  comparison; `picker_matches` lowercases the query per row and recomputes chips/location
  that rendering computes again; about 10 temporary Strings per row and the feed copied
  twice. Sort by cached keys, pre-split terms, `TAG_ANSI` table, `Cow` trunc, write into one
  `String`.
- [ ] **O5.** Timeline clones whole `Session`s and every cached transcript row per lane
  every tick; borrow (`LaneSeed<'a>`, `Vec<&Row>`).
- [ ] **O6.** Pane label builds pieces as separate Strings per pane per second; `write!`
  into one buffer.
- [ ] **E2 behaviour.** artifacts `ArtifactStoreError` carries no path, so load errors print
  as bare "Permission denied". Wrap with the record path.
- [ ] **E3.** `VerbError::Failed(String)` drops every typed cause (chat stringifies 16
  variants). Add a boxed `Plugin` source variant; land with H4.
- [ ] **I4.** artifacts `try_outcome!` re-implements `?`; steps return `Result<i32, Outcome>`
  with `From<VerbError> for Outcome`.
- [ ] **Q3 behaviour.** Two group cascades disagree: timeline ignores `ext.artifacts.group`
  (and its doc still says artifacts have no plugin). One resolver, with a differential test.
- [ ] **Q4.** Picker key state machine: esc while searching, esc/enter with buffered digits,
  junk pending file untested. Property test over key sequences; extract `parse_pending`.
- [ ] **Q5.** menu scrapes chat's and tutor's stdout wording and tests it against strings it
  wrote itself; move the line formats to the producers.

### MEDIUM

- [ ] **I2 / T5.** artifacts menu `Target { kind: &'static str, id }` dispatched on strings and
  kept in vectors parallel to `names`; `enum Target { Session(SessionId), Artifact(String) }`
  carried on each row.
- [ ] **I3.** artifacts kind name stringly typed (`KIND_NAMES`, `--kind` compared by hand);
  `enum KindName` with `FromStr`.
- [ ] **I5 / T6.** artifacts owner/actor is a `String` holding a session id or `"user"`;
  `enum Actor`; mutations default to the service's actor; `archive` / `unarchive` instead of
  a bool.
- [ ] **I6 / T10.** chat matches engine ids `"claude"`/`"codex"` in six functions with silent
  fallbacks; `enum KnownEngine` parsed once; engine crates export `pub const ID`.
- [ ] **I7.** artifacts `Cleanup::to_value` / `from_value` are hand-written serde; derive.
- [ ] **I8 / T11.** roles are strings (`role_of`, picker `ROLES`, tutor); `enum Role` in tx-ui.
- [ ] **I9 behaviour.** timeline re-implements the group cascade minus artifacts; artifacts
  `own_group` reads raw JSON despite typed `Ext`. (Same fix as Q3.)
- [ ] **I10.** artifacts log event types are string literals and `append` takes three `&str`;
  `enum LogEvent`.
- [ ] **I12.** artifact ids are bare strings next to view and actor strings; `ArtifactId`
  newtype; migrate re-implements the store's record listing.
- [ ] **T4.** artifacts picker `handle -> Option<Option<Pick>>`, Attach/Focus/Popup spelled 3
  times, `back: Option<Option<String>>`; `enum Step`, `Pick::Show(Mode, usize)`, `enum Back`.
- [ ] **T7.** `LaunchOptions` agent builders silently no-op on a process launch; agent fields
  written 3 times (tx-core LaunchOptions, Launch, chat AgentLaunch); one `AgentLaunch` struct.
- [ ] **T8.** tx-ui popup API takes 5-6 `&str` in a row, sizes as strings, and a `socket`
  that duplicates `Ui`'s; `enum Extent`, socket from `ui`.
- [x] (ea3ca7b) **T9.** usage errors hand-written 29 times (drifted from `Verb::usage()`); add
  `VerbError::Usage`.
- [ ] **T9b.** Registry boxes verbs/engines/sinks itself.
- [ ] **E4 behaviour.** io errors without `#[source]` or path: `CoreError::Lock` (no lock
  file named), `UiError::Spawn`, `ReadError::Utf8`, tutor `PackError::Toml` flattened.
- [ ] **E5.** "io error with path" helper copied in 7 plugins; `VerbError::Io { path, source }`.
- [ ] **E6 bug.** `let _ =` drops failures users should see: mockup `notify` drops
  `send_message` (the waiting agent never hears); hooks Seed, labels, chat detach, mockup
  events reset silently. Report mockup's; comment the deliberate best-effort ones.
- [ ] **E8 behaviour.** "session not found" written 4 ways; one `txc <verb>: <message>`
  convention, prefixed by the host.
- [ ] **O7-O11.** artifacts: `Opener` cloned twice (take by value); `bound_artifact`
  deserializes the whole `Ext` inside nested loops (borrowed `ExtRef`); owner names loaded one
  record per row (one names map per listing); records loaded twice via `resolve_id` +
  `require` (return the record); menu loads the same session twice for a popup.
- [ ] **Q6.** All 12 property suites need `--features fuzz`, so the documented gate runs
  none; run the pure ones (events, session, store, profile, engine) by default at low cases.
- [ ] **Q7-Q9.** Test helpers duplicated: 3 CLI suites re-declare `Cli`; `show`,
  `client_sessions`, `outer_keys`, `installed_server`, `create_agent` copied 2-4 times;
  session record builders written about 10 times (one `tx_core::testing::session` builder).
- [ ] **Q10.** Sleeps instead of waits: artifacts prefix+a 500ms "for fzf", fuzz_tmux /
  fuzz_messaging 300ms, messaging 200ms, history/chat 500ms absence sleeps.

### LOW

- [ ] **E9 behaviour.** stderr writes sometimes `?`-propagated (usage paths fixed by T9);
  core verbs ignore stdout write failures, plugins fail. One policy.
- [ ] **E10.** unwraps without a stated invariant: mockup model.rs (7 after a presence count),
  viewer.rs, chat/hooks install `expect("JSON value")`, timeline regex literals.
- [ ] **E11.** mockup's caught viewer panic loses its message; downcast the payload.
- [ ] **E12 policy.** errors put `{source}` in Display and mark it `#[source]`, so a chain
  walker prints causes twice; tx-core-cli `Failure` is not an `Error`. Decide once.
- [ ] **O12.** small owned copies: `actor().to_owned()` x3, `resolving.clone()` per recursion
  in grouping, `view.clone()` in pending_cleanups, engine clone in timeline, picker
  `strip_prefix(&format!(..))`.
- [ ] **Q11.** concurrency test spends 15s in the fixed 300ms `DELIVERY_PAUSE`; make it a
  config value, 10ms in tests; drop the constant-equals-itself test.
- [ ] **Q12.** tests read ids and results out of prose (`created_id` takes word 4 of
  "Created …"); read from the store or a `--json` flag; assert `Report` exactly.
- [ ] **T12.** `edit_session` takes 4 `&str` and returns a 3-tuple; `SessionEdit` struct.
- [ ] **Flake.** tx-core tests/create.rs `agent_session_gets_profile_plan_and_workspace`
  failed once under parallel load (passes alone 3/3); watch.

## Round 3 (2026-10-28): tx-core rescan (idiomatic-rust, ownership-not-clone, rust-errors)

### MEDIUM

- [x] (59ea7fe) **M16.** core.rs: the "running session" precondition (load, `is_terminal` →
  `NotRunning`) is written 3 times (`get_execution_location`, `stop_session`,
  `send_message`); one `running_session(id)`.
- [x] (59ea7fe) **M17.** core.rs: `refresh_session` and `refresh_all` both do "lock, load, if running
  and not live then `mark_exited`" in two styles; one `exit_unless_live(id, &live)`.
- [ ] **M18 behaviour.** `InvalidEnvKey` says "a name without '=' or NUL" but the grammar is
  `[A-Za-z0-9_.-]+`; a NUL in the value is reported as an invalid key. State the real
  grammar; add `InvalidEnvValue`.

### LOW

- [x] (59ea7fe) **L13.** core.rs `forget_session` swallows an unreadable record's name with
  `.ok().flatten()` without saying why (a corrupt record must stay forgettable).
- [x] (59ea7fe) **L14.** store.rs: `ReadError::Io(e) if e.kind() == NotFound` written twice;
  `ReadError::is_not_found`.
- [ ] **L15 behaviour.** store.rs `Unreadable { file }` holds only the lossy file name, not the
  path its doc promises; carry `path: PathBuf`.
- [ ] **L16.** tmux.rs `InspectionFailed { stderr }` carries a parse diagnostic that is not
  stderr; rename the field to `reason` with the next deliberate API change.
- [x] (59ea7fe) **L17.** profile.rs allocates each role body twice then joins into a third buffer; push
  trimmed bodies straight into one `String`.
- [x] (59ea7fe) **L18.** profile.rs `skills_in` is `pub(crate)` but file-local.
- [x] (59ea7fe) **L19.** Stale or repeated comments: events.rs `encode` "fits PIPE_BUF" (false when the
  other fields exceed it); the trailing-`;` rule explained 3 times in tmux.rs; plugin.rs
  `VerbTable` declared after its first use.

## Decisions needed

- H20: one "session gone" rule for kill and refresh?
- Hooks state type: tx-ui (hooks gains a tx-ui dependency) or tx-core?
- H1, H3, M4, E2, E8, Q3/I9, M18, L15 change observable output (command strings in tests, an uninstall message, env
  order); default is to take them as bug fixes unless told otherwise.
