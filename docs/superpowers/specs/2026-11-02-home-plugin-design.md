# The `home` plugin: tx-ide's session manager as the landing screen

Design, 2026-11-02. Companion artifacts: "tx-ide startup: rough edges" (what is wrong today, measured) and "Modern terminal UI/UX: research for the tx-ide startup" (the field survey this follows and the mockups the operator picked).

## Intent

`tx ui attach` should land the operator on a screen that answers, within 100 ms, *which sessions are open, what each is doing, and what ended recently*, and lets them get into one with a single key. The assistant starts in the background; its state is one line of the header. The screen is a plugin, `plugins/home` (crate `tx-home`), and absorbs the `menu` plugin.

**Why now.** Measured today: a cold attach shows nothing for 3.7 s, then a shimmering menu popup over Claude's hidden trust prompt; a reflexive Enter exits the assistant into a dead `[exited]` shell; the next attach lands on that shell while `tx-assistant-2` warms unseen. The operator's words: "better management of what sessions are actually opened there and better see the history … it should all be a plugin".

## Goals

1. First paint of the home screen under 100 ms after attach, before any agent is started.
2. One list: projects → agents → the views that belong to each agent, with live state and age; ended agents inline, greyed, resumable.
3. History on demand per row (unfold inline): branch, roles and model, the last exchange, artifacts, started/ended, with the verbs `r` resume, `a` artifacts, `t` timeline.
4. The home pane can never show a dead shell: the TUI is the floor of the pane; nesting into a session and coming back is a loop.
5. The assistant warms in the background from the home; the header shows `◐ starting …` → `● ready`; a dead assistant shows as a state with `r` to restart in place, never a silent `-2`.
6. Everything the start menu offered stays reachable: `n` new agent, `/` ask the assistant, `?`/`K` key sheet, `L` tutor, `T` timeline, `a` artifacts, `p` plugins.
7. The `menu` plugin's first-attach hook and shimmer go; `prefix+m` opens the home screen in a popup instead.

## Non-goals

- No command palette in this round (footer + `?` sheet first; a fuzzy palette is a later plugin or a later version).
- No changes to the picker (`prefix+t`), the tutor or the artifacts plugin; the timeline, spawn, chat and engine-claude plugins change only as listed under "Boundary changes".
- No images, no Nerd Font icons, no truecolour-only styling.
- No new agent state in core: "waiting for your approval" is not distinguishable from `Idle` today and the home shows what core knows.

## Anti-goals (a failure even if it works)

- A splash or any idle animation. Motion is spinners at 4 Hz while something is starting, nothing else.
- A popup that covers a prompt needing an answer.
- A home that goes stale: state older than ~1 s, or an ended agent still shown as running.
- The home writing to agent records, tmux options of other sessions, or other plugins' data. It reads core and tmux; it acts only through `tx` verbs and tmux commands (the repository's D3 rule).

## Constraints

- A plugin crate under `plugins/home`, registered in `tx-core-cli` like the others; depends on `tx-core`, `tx-client`, `ratatui`, and `tx-chat` for the transcript message parser (the precedent is `tx-timeline` → `tx-chat`).
- `tx-client`'s `attach` stays glue: create the `home` view with the plugin's loop as its program, attach. It must still work with the `home` plugin disabled (then: today's shell, minus the assistant nesting).
- Keys mirror the prefix table where one exists (the menu's convention), and the footer is context-sensitive.
- Claude's trust prompt must not be the first thing an operator meets. The only Claude-side state the design touches is trust for the assistant's home repository (below).
- The operator reviews and commits; the builder does not commit (standing instruction for this repository).

## The screen

Picked by the operator from mockups; markup below is layout, not pixels.

```
tx-ide  2 running · 3 ended today · assistant ● ready                  (header, 1 line)

tx-ide-rs  ~/Developer/sandbox/tx-ide-rs                                (project row)
  ● startup-ux     working  12m   feat/startup-ux                       (agent row)
  ○ docs-site      ended  yesterday  14 chats                           (ended agent, dim)
      branch feat/docs-site   roles COMMON DEVELOPER   opus/medium      (unfolded detail)
      you ›  publish the API docs with a structure graph
      docs-site ›  Site builds; graph rendered with plantuml. Ready for review.
      artifacts 1 doc · 1 diff        r resume  a artifacts  t timeline
wrangler  ~/w/wrangler
  ◐ fixer          idle  3m      fix/retry-backoff
    ⌐ wrangler-p1-nvim · build-watch                                     (views of fixer, dim)
tx-system
  ○ tx-assistant   idle   1h

enter attach · →/← unfold · n new here · / ask · f find · ? keys       (footer, 1 line)
```

- **Header**: `tx-ide`, counts (running, ended today), assistant state. Assistant states: `◐ starting <step>` (step is the warm phase: `home repo`, `worktree`, `claude`, `waiting for READY`), `● ready`, `○ stopped · r restart`, `✗ failed: <last stderr line>`.
- **Project row**: the repository's main checkout (name = its directory name, path dimmed). Projects sort by most recent activity of anything under them. `tx-system` (the assistant's tag) is a pseudo-project pinned last.
- **Agent row**: state glyph (`●` working, `◐` starting, `○` idle/ended), name, state word, age (`reltime` of `last_activity_at`, else `created_at`), branch dimmed. Ended agents are dim; past the three newest per project they fold into one line `… N more ended · Tab`.
- **View rows**: under the agent whose `@tx-view-parent` they carry, one dim line listing names; views with no parent sit under their project by `session_path` when it is inside that checkout, else under a trailing `views` pseudo-project. The `home` view and the home's own mockup view are never listed.
- **Unfolded detail** (→ / `l` on a row; ← / `h` folds): branch, roles, model and effort (from the engine's `ext` data as the timeline reads them), parent name, the last exchange (last user message and the first line of the last assistant message, each clipped to one line), artifact counts by kind, started and ended times, then the verbs that apply.
- **First run** (no agents but the assistant): the list is replaced by the three verbs `n` new agent, `/` ask, `L` tour, each with one dim line of explanation, and a dim line saying ended sessions will appear under their project. The assistant row still shows.
- **Find** (`f`): a one-line input in the footer's place; rows whose name, tag, project or branch contain the text stay; Esc clears.
- **Ask** (`/`, mirrors prefix+/): a one-line input in the footer's place; Enter sends the line through `tx assistant ask` and nests into the assistant; Esc cancels.
- **Keys** (main list): `j/k`/arrows move over agent and view rows (project rows are skipped); `Enter` attach (nest) for a running agent or view, resume for an ended agent; `→/l` `←/h` unfold/fold; `Tab` fold/unfold all ended; `n` new agent (the `spawn` form, cwd preset to the selected project's checkout); `x` stop the selected agent (asks `y` on the footer line); `r` respawn the selected ended agent, or the assistant when it is stopped; `a` artifacts of the selected agent; `t`/`T` timeline; `f` find; `/` ask; `?` the key sheet (the cheat sheet the menu builds from the fragments' `# keys:` lines, plus the home's own keys); `K` same as `?`; `p` the plugin list; `L` the tutor; `q` does nothing on the floor and closes the popup variant; `Ctrl-c` likewise. During the first 400 ms after an attach-triggered open only navigation, Enter and Esc are accepted (the menu's grace rule).

## Mechanism

**The floor.** `tx ui attach` creates `home` (kind `home`) with `remain-on-exit on` and a session-scoped `pane-died` hook that runs `tx _home-perform <pane>`. The pane starts with `tx _home-tui`. The TUI draws, polls, and exits writing one line to its choice file (same contract as `_menu-tui`: `attach <session>`, `resume <id>`, `new <cwd>`, `stop <id>`, `ask <text>`, `artifacts <id>`, `timeline`, `tutor`, or nothing (`plugins` and `keys` are sub-views and never leave the TUI)). `_home-perform` reads the choice and respawns the pane: for `attach <session>` with the nested attach alone (`TMUX= tmux -S <sock> attach -t =<session>`, no trailing shell), for everything else it runs the `tx` verb or popup the menu's `perform` runs today and respawns the TUI. When whatever runs in the pane exits (the operator detached from the nested session, the session ended, a shell the picker put there was exited) the pane dies and the hook brings the TUI back with `--returned-from <session>` so that row is selected. The floor is therefore the hook, not a parent process: other plugins that `respawn-pane -k` into the home pane (the picker's Enter, the artifacts menu) keep working unchanged, and the TUI returns whenever their program ends. While the TUI runs, the pane's command is `tx`, so those plugins switch-client instead of nesting — that is acceptable; from the floor the operator uses Enter.

An existing `home` from an older install (pane running a shell or `tmux`) gets the hook and the TUI on the next attach via `respawn-pane -k` (the `nests_here` rule); an nvim or other program in the home pane is left alone.

**Attach.** `tx-client::install::attach` no longer calls `assistant warm` and no longer decides the pane program itself: it asks the registry for the `home` plugin's pane program (a resource or a verb that prints it), falling back to the shell when the plugin is disabled. Everything else in `attach` (switch-client inside the core, the nesting warning, `no-detach-on-destroy`) stays.

**Data.** The TUI reads through `TxCore` every second (and immediately after any action): `list_agents` running and ended, `refresh_agent` for live state, `conversation` → transcript path, `Ui::views()` for views and their parents, `git -C <worktree> rev-parse --git-common-dir` once per worktree (cached) for the project, `git symbolic-ref --short HEAD` for the branch (cached by `.git/HEAD` mtime). The last user and assistant messages, and the model and effort, come from the transcript readers that live in `tx-timeline` today (`rows`, `metadata`); those two modules move into `tx-chat` (which already is "the peer-message parser the timeline uses") and the timeline imports them from there — the only change to the timeline. Artifact counts come from `tx artifact ls --session <id>` output (cached, refreshed on unfold and after `a`). Nothing here blocks the draw: slow reads run on a worker thread and the row shows `…` until they land.

**The assistant.** Only an attach-triggered TUI (not `--returned-from`, not `--popup`) starts the assistant: it spawns `tx assistant warm` detached and forgets it. `warm` takes a plugin-level lock (`<data>/plugins/assistant/warm.lock`) around its running-check and create, so two warms cannot both create; a warm that finds the lock held waits and then finds the record. The header derives the phase from the record, not from `warm`'s output: no assistant record → `◐ starting · creating`, `Starting` → `◐ starting · claude`, `Idle`/`Working` → `● ready`, `Ended` → `○ stopped · r restart`, and `✗ failed: <reason>` when `warm` exits non-zero (its last stderr line is kept in a file beside the lock). The operator can `Enter` the assistant row as soon as its session exists. `r` on a stopped assistant runs `tx respawn <id>` (same record, same worktree); `warm` itself is changed to respawn an ended assistant instead of creating `-2`.

**Trust.** `assistant::ensure_home`, when it creates the home repository, also records trust for that path in Claude's project settings (`projects["<path>"].hasTrustDialogAccepted` in Claude's config file, under `CLAUDE_CONFIG_DIR` else `$HOME`) so the first assistant does not stop on the trust dialog (Claude keys trust by the main repository path, confirmed on this machine). The helper lives in the engine-claude plugin (a `trust <path>` method on its adapter), so knowledge of Claude's file layout stays in one place. It only edits an existing file and only adds that one key; when the file does not exist (a machine where Claude has never run) it does nothing — Claude's own onboarding comes first there, and the header shows the assistant as `starting` until the operator deals with the prompt inside the pane. Idempotent: a second call changes nothing.

**The menu.** `plugins/menu` is removed (its id stays in the registry's retired list so old `plugins.yaml` files keep loading). `prefix+m` moves to `home`'s key fragment and opens `tx _home-tui --popup` in an 80 % popup. From the popup, `attach <session>` is a `switch-client` to that session (the home floor stays behind in its pane); every other choice runs through the same `perform`. The `client-attached` hook is gone with it (the floor is the landing). The cheat-sheet builder (`data::key_hint`), the 400 ms grace, `Action` encode/decode, `nests_here`, `tutor_view`, `respawned_name` and their tests move to `home`.

## Boundary changes outside the plugin

- `tx-client::install`: `attach` asks the `home` plugin for the pane program and the hook (a verb that prints them, or applies them to a given session), falling back to the shell when the plugin is disabled; `ensure_home` keeps the status-bar and border setup. Window name of `home` becomes `home` (today `bash`).
- `plugins/assistant`: `warm` takes the warm lock, prefers respawn of an ended assistant, keeps its last failure line; `ensure_home` seeds trust through the claude engine plugin.
- `plugins/engine-claude`: the trust-seeding helper (edit an existing Claude config file, add one key, never create).
- `plugins/spawn`: `_new-agent` accepts `--cwd <dir>` to preset the form's directory (the home passes the selected project's checkout).
- `plugins/chat`: gains the transcript row/metadata readers from `tx-timeline`; `plugins/timeline` imports them (no behaviour change).
- `tx-core-cli`: register `tx-home`, drop `tx-menu` (retired id), `tests/menu.rs` becomes `tests/home.rs`.
- README: the plugin table, the `prefix+m` row, the `ui attach` paragraph.

## Style

- Colours: the IDE palette the menu already uses (`ACCENT`, `INFO`, `WARN`, `FG`, `DIM_FG`); state glyphs in green (working), yellow (starting), dim (idle/ended), red (failed). Every colour has a `Color::Indexed` fallback when the terminal reports no RGB (tmux quantises otherwise).
- Borders: none in the main list; rounded `Block` only in the popup variant and the key sheet.
- Spinner frames at 4 Hz on `◐` rows; otherwise the TUI redraws on events and on its 1 s poll.
- Age strings via `tx_client::render::reltime`; widths as in the picker (`NAME_W` 24).

## Tests

- Model tests (pure): grouping and ordering from a synthetic set of agents and views; fold/unfold; filter; first-run detection; key handling per view; the grace period; choice encode/decode.
- Verb tests through the binary (as `tests/menu.rs` does): `_home-tui` with a fake terminal writes the expected choice; `_home-loop` nests the picked session and returns to the TUI when the nested session ends; attach creates `home` with the loop and paints within the budget; a disabled `home` plugin gives today's plain shell.
- Assistant: `warm` output lines; respawn preferred over create; trust seeding writes exactly one key and is idempotent.
- A fuzz/property pass after each task (the repository's standing practice), then the operator's review.

## Left to the builder

- Exact column widths and clipping; what the detail shows when the transcript is missing.
- Thread/channel structure inside the TUI; whether the 1 s poll is a timer tick or a notify.
- How `_home-perform` is structured internally (one verb reading the choice file and respawning, as described) and how the hook is installed per session versus globally.
- The popup variant's size and whether it hides view rows.
- Whether the `tx-system` project shows the assistant's worktree path.

## Decisions made after the builder check (the designer's calls)

A reviewer who could not ask questions raised six; none needed the operator, and they are settled above as follows:

1. The floor is a `pane-died` hook with `remain-on-exit`, not a parent loop process, so other plugins' `respawn-pane -k` into the home pane cannot kill it and the picker stays untouched.
2. Only the attach-triggered TUI starts the assistant; `warm` is made race-safe with a plugin lock; the header reads phase from the record (three steps, not four).
3. The transcript readers move from `tx-timeline` into `tx-chat`; the home depends on `tx-chat` only.
4. `_new-agent` gains `--cwd`.
5. Trust seeding edits an existing Claude config file only, never creates one.
6. From the popup, `attach` is `switch-client`.
