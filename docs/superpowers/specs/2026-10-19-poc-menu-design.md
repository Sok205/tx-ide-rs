# poc `menu` plugin — the tx-ide start menu

Status: design, 2026-10-19. A new plugin in `poc/plugins/menu` (`tx-menu`), under the
constraints of `2026-10-19-poc-port-roadmap.md` (Global Constraints, D1–D7). Like Phases 3, 5 and 6
it is built with **zero changes to `tx-core`**.

## Intent

When the operator opens tx-ide, a full-screen start menu greets them. It shows everything useful
tx-ide can do, one keypress away, so they don't have to remember each `prefix+` key or `txc` verb.
It also shows off: an animated title screen. The operator picked look **A, "Title screen"**, from
three ratatui mockups (`.superpowers/mockup/97211e02-…/content/start-menu.json`, option `a`).

## Look (operator's choice: A)

- **Top:** a big block-letter `TX` logo (the mockup's 6-line `████████╗██╗  ██╗` art) with
  `ide` beside it. A shimmer runs through it: one line at a time lights up in accent/info while
  the rest stay dim, looping while the menu is open at about 6–8 fps. *(The loop is my guess;
  the operator didn't say "play once".)*
- **Middle:** the menu, one item per line with a dim single-key hint aligned at the right. The
  selected line is highlighted. Grouped with dim section headers (below).
- **Bottom:** a `recent` box with the last 3 past sessions (`name  tag · age`), resumed with
  keys `1`–`3`.
- Palette: `tx_ui::render` (accent rgb(122,162,247), info rgb(125,207,255), warn
  rgb(224,175,104), dim fg rgb(169,177,214)). ratatui 0.30, as in `mockup`.

## When it appears (operator's choice)

1. **The first time tx-ide opens**: the first client attach on a fresh tx tmux server. Plugin-only:
   the menu's `keys/menu.tmux` fragment sets a `client-attached` hook that pops the menu up
   unless the server option `@tx-menu-shown` is set, then sets it. Reattaching to a running
   server never shows it again.
2. **`prefix+m`**: always opens it (`display-popup -E`, about 80%×80%). The key is proposed and
   not yet confirmed; it's free in poc today.

## Items

Only verbs that exist today. An item whose plugin is disabled (`core.plugin_status`) is hidden.
Selecting an item closes the popup and runs a `txc` verb, so the menu calls other plugins only
through the CLI, never their Rust code (D3). That's the same precedent as the picker's binds.

| Section | Item | Key | Runs |
|---|---|---|---|
| Sessions | New tx session | `n` | spawn's new-agent form (`_new-agent <pane> <client>`) |
| | Attach to running | `t` | `pick` (picker) |
| | Resume from history | `h` | an in-menu history list (`history` data, resumable sessions only), Enter → `resume <id>`, then the new session is nested into the launching pane (or the client switched to it), as the picker's Enter does |
| | Fork this session | `f` | `_fork-pane <pane> <client>` (only when the launching pane is a tx session) |
| Look around | Timeline | `T` | `_timeline-attach` |
| | Artifacts | `a` | `_artifact-menu <session> <client>` (only when the launching pane is a tx session) |
| Setup | Plugins | `p` | an in-menu read-only list from `plugins` (id, active/disabled) |
| | Keys cheat sheet | `K` | an in-menu list built from every `keys/*.tmux` resource's `# keys:` line plus its first comment |
| Learn | Tutor | `?` | `tutor start` |
| | Assistant | `/` | switch to `tx-assistant` (`assistant warm` first) |
| | Quit | `q` / `Esc` | close the menu |

An item's key is the IDE's prefix key for the same thing wherever one exists (`n`, `t`, `f`, `T`,
`a`, `/`), so a reflexive prefix key lands on the same action; the others are `h`, `p`, `K`
(`k` moves), `?` and `q`.

`j`/`k` and the arrow keys move; `Enter` runs the selected item. Keys `1`–`3` resume a recent
session. Ctrl and Alt chords never reach the menu. A menu opened by the first attach takes only
the arrows, `j`/`k`, `Enter`, `Esc` and `q` for its first 400 ms, so keys typed for the IDE
while the client attached do not pick items.

## Non-goals and anti-goals

- No new behaviour inside other plugins. If an item needs something a verb can't do, leave it out
  and note it.
- No changes to `tx-core` or to other plugins' crates (adding resources and the MANIFEST line is
  fine).
- The first-open popup must never fire on reattach, and must never block the attach if the
  menu fails.
- No editing of `plugins.yaml` from the menu (read-only list).

## Builder's call

- How the menu passes `<pane>` and `<client>` to the verbs it runs (the popup's `#{pane_id}` /
  `#{client_name}`, as the other key fragments do).
- The history list's columns and how many rows it shows.
- The test strategy: `TestBackend` snapshots of the title screen (two frames, to prove the
  shimmer moves), state-machine tests for keys → action, a CLI test that `ui install` binds
  `prefix+m` and the attach hook, and a test that the hook fires once per server.
