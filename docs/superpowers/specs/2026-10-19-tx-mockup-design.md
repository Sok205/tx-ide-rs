# poc `mockup` plugin — the brainstorming companion, in a ratatui view

Status: design, 2026-10-19. A new key-less plugin in `poc/plugins/mockup`, under the constraints
of `2026-10-19-poc-port-roadmap.md` (Global Constraints, D1–D7) and the one-plugin-per-feature
rule of `2026-10-19-poc-key-plugins-design.md`.

## Intent

During brainstorming an agent shows the operator mockups, today through the superpowers
browser companion (an HTML server plus a browser tab). The operator lives in tmux and wants the
same loop in the terminal: **faster and more token-efficient**. The agent writes a short
description file and a prebuilt ratatui viewer renders it instantly. There's no HTML, no
compile step, and no browser.

The first consumer is the next brainstorm: a ratatui **start menu** for tx-ide (animated
splash, history of past sessions, "new tx session"). So the viewer has to show layouts *and*
animation well enough to judge a TUI by.

## Goals

- A plugin, `mockup` (`tx-mockup` crate), that owns:
  - a viewer verb that renders description files with ratatui and reloads whenever a newer
    file appears;
  - verbs the agent calls to start, show, park, and stop a mockup session;
  - a skill, `agents/skills/tx-mockup/SKILL.md`, published as a resource (as `spawn` publishes
    its skills), plus `agents/COMMON.local.md` whose frontmatter is `skills: [tx-mockup]`, so
    every tx agent gets it after `txc ui install`. *(Builder-check call: `spawn`'s COMMON.md is
    not edited. Listing the skill there would make every `create_session` fail with
    `UnknownSkill` while mockup is disabled. The `.local.md` overlay from the shipped dir is
    pruned with the plugin. If a second plugin ever needs a COMMON overlay, resource names
    collide, so revisit then.)*
- The viewer runs in its **own view**: a tmux session on the core's server marked `@tx-view`,
  named `mockup-<agent-name>`. There is one per agent session, so two agents brainstorming at
  once never collide.
- **Automatic switching:** when the agent shows a new screen, the operator's terminal is
  switched to the mockup view. Confirming or dismissing in the viewer switches them back to
  where they came from.
- The operator picks an option with a key, and the pick is recorded to an events file the agent
  reads on its next turn. This is the same contract as the browser companion's `state/events`.
- Animation: a node can carry frames and a frame rate, and the viewer plays them.
- Token efficiency is a first-class requirement of the description format. A typical
  three-option screen should be about 20–60 lines.

## Non-goals

- Running real ratatui code for each mockup. A real-code spike may still follow for the chosen
  screen, but it is outside this plugin.
- Mouse support, and pixel-perfect fidelity to any final app.
- Anything outside the core's private tmux server. The user's default tmux server is never
  touched.
- A prefix key binding. The agent drives this plugin, and the operator never opens it by hand.
- Replacing the browser companion in the superpowers skill. This is a tx skill that agents
  choose instead of it.

## Anti-goals (a failure even if it works)

- Getting slower or more verbose than the browser companion. If a mockup needs more tokens than
  the equivalent HTML fragment, the format is wrong.
- Switching the operator's terminal when nothing new is shown. Waiting or parked screens never
  switch.
- A malformed file crashing the viewer or blanking the screen. The last good screen stays up,
  and the error shows in a bottom bar.
- Core learning anything about mockups (D2). No new core type, field, event or method.

## Constraints

- Everything in the roadmap's Global Constraints binds. Every tmux call goes through
  `tx_ui::tmux::Ui`, socket-bound with `-S <socket> -f <state_dir>/tmux.conf`. Option names are
  hyphenated. `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
  pass in `poc/`. Tests use a temp state dir and a private socket (`require_tmux!` skip).
- `ratatui = "0.30"`, matching `crates/tx` (the main `tx timeline` already uses it), plus
  `crossterm` as its backend.
- The plugin is registered with one line in `tx-core-cli/src/plugins.rs::MANIFEST` and enabled
  by default, like the others. `txc plugins` lists it, and `mockup: { enabled: false }` in
  `plugins.yaml` removes its verbs and its skill.
- Mockup files live under the project, as the browser companion's do:
  `<project>/.superpowers/mockup/<session-id>/{content,state}/`. The skill tells the agent to
  remind the operator about `.gitignore`.

## Behaviour

### The agent's loop (what the skill teaches)

1. `txc mockup start --project <dir>`. This creates the session dir, creates or reuses the
   view `mockup-<agent>`, and starts the viewer in it. It prints one JSON line:
   `{"view":…,"content_dir":…,"state_dir":…}`. The agent is resolved from `TX_SESSION_ID` in
   the agent's environment, with a `--for <session>` override.
2. The agent writes `<content_dir>/<semantic-name>.json`, always a new filename. It then runs
   `txc mockup show`, which switches the operator's terminal to the view.
3. The operator compares options, picks one, and is switched back. They answer in the agent's
   pane, and the agent reads `<state_dir>/events` together with what they typed.
4. When the conversation returns to text, the agent runs `txc mockup wait`. This puts up a
   "continuing in terminal…" screen and does **not** switch.
5. At the end, `txc mockup stop` kills the view. The files stay on disk.
6. If the view has died (killed, or the server restarted), `show` recreates it from the same
   session dir before switching. The agent never needs to notice.

### The viewer

- It renders the newest file in `content_dir` by modification time, picking up new files by
  polling or a file watcher (builder's call; polling about every 200ms is acceptable). A file
  named `waiting*.json`, or the screen `wait` writes, shows the waiting screen.
- **Keys:**
  - `1`–`9`: select that option (it is highlighted and the event recorded).
  - `←`/`→` and `h`/`l`: move between options when they are shown one at a time.
  - `Tab`: toggle between all options side by side and one option fullscreen.
  - `Enter`: confirm the current selection and switch back.
  - `Esc`/`q`: switch back without a selection.
  - `r`: restart animations.
- The title and subtitle are shown at the top and a key-hint line at the bottom. Parse errors
  replace the hint line in the warning colour (`tx_ui::render` palette), and the last good
  screen stays rendered.
- **Events** go to `<state_dir>/events` as JSON lines, cleared when a new screen loads:
  `{"type":"select","choice":"b","title":"Dashboard panes","screen":"layout.json","timestamp":…}`
  and `{"type":"confirm",…}` / `{"type":"dismiss",…}`.

### Switching

- `show` finds the operator's terminal. This is the **outermost** tmux client showing the
  agent: attached to the agent's session directly, or attached to a view (typically `home`)
  whose pane nest-attaches it (see `Ui::attached_to`, `attachment_map`, and the picker's nest
  handling). It switches *that outer client* to the mockup view, so the mockup gets the full
  screen and never a nested pane. It records the client's previous session on the view as
  `@tx-mockup-return`, never pointing at a `mockup-*` view. Switching back restores that
  session's own current window and pane. *(Builder-check call: outer client, not the nested one.)*
- The viewer's `Enter`/`Esc` switches that client back to `@tx-mockup-return`.
- If no client is found (the operator is detached, or the agent was run headless), `show` prints
  `mockup ready in view <name>` and exits 0. The skill tells the agent to relay that line.
- If several clients are attached, `show` uses the most recently active one.

### The description format (sketch; the builder settles details)

The format is JSON, because `serde_json` is already a workspace dependency, agents write it
reliably, and it nests without TOML's `[[a.b.c]]` noise. Keys are short. Text uses inline
colour markup, so most mockups are mostly text art:

```json
{"title":"Start menu — which layout?","sub":"pick the closest","fps":8,
 "options":[
  {"id":"a","title":"Centered splash","note":"game title screen",
   "body":{"col":[
     {"size":7,"frames":["[green]▀█▀ ▀▄▀[/]","[dim]▀█▀ ▀▄▀[/]"]},
     {"list":["New tx session","Resume","Timeline","Quit"],"sel":0},
     {"block":"recent","body":{"text":"[yellow]wrangler-p1[/] [dim]2h ago[/]"}}]}},
  {"id":"b","title":"Dashboard panes","body":{"row":[
     {"size":"30%","block":"actions","body":{"list":["New tx","Resume"],"sel":0}},
     {"block":"history","body":{"table":[["wrangler-p1","tx-ide-rs","2h"]]}}]}}]}
```

- **Nodes:** `row` / `col` (splits), `block` (a border plus title around a `body`), `text`,
  `list` (+ `sel`), `table` (+ optional `head`), `frames` (+ per-node `fps`, overriding the
  top-level one).
- **Sizes:** `size` is a length (an integer), a percentage (`"30%"`), or absent (fill).
- **Markup:** `[green]…[/]`, `[bold]`, `[dim]`, `[on blue]`, combinable as `[bold yellow]`.
  Named colours plus the `tx_ui::render` palette names (`accent`, `warn`, …). `[[` escapes a
  literal `[`.
- A file without `options` is a single screen with a top-level `body`.

## Left to the builder

- Module layout inside the crate, and how the viewer verb is spelled (internal `_mockup-view`
  versus `mockup view`).
- Polling versus a file watcher, and the exact markup grammar beyond what is shown above.
- Which extra widgets (gauge, sparkline, tabs) are worth adding beyond the list above. Add none
  unless they're cheap.
- How to find the operator's client precisely under nesting, within the behaviour above.
- The skill's wording. It should mirror the browser companion's `visual-companion.md` loop
  (when to use, the loop, cleanup) without its HTML parts, and stay short.
