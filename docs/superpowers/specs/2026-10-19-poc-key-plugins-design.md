# poc key plugins — one plugin per key, resources instead of a central fragment

Status: design, 2026-10-19. Restructures phase 1 of `2026-10-19-poc-port-roadmap.md` (the
`views` plugin, landed the same day as `poc/plugins/views`). Extends
`2026-10-16-poc-plugins-design.md` with one generic slot.

## Intent

The user, or an agent acting for them, switches a feature on or off by naming it in
`plugins.yaml`. "Turn off prefix+X" must be `kill: { enabled: false }` and nothing else. Adding a
prefix key later must be adding a plugin, never editing a shared file. Today's `views` plugin
breaks both: one yaml key covers six features, and one fragment lists every key.

## Goals

- One plugin per feature a person would name: `picker`, `edit`, `kill`, `labels`, `ide`.
- A plugin carries everything it needs: verbs, its key bindings, its helper scripts.
- `txc` is self-contained after `cargo build`: `ui install` reads nothing from the source tree.
- Disabling a plugin removes its keys, including on a running server after `txc ui install`.

## Non-goals

- No change to any verb's behaviour, output or name. No new keys. No per-plugin settings.
- No unbinding of keys the user bound themselves.

## Anti-goals

- A tmux-specific slot in the core. The core must not learn what a key binding is.
- A plugin calling another plugin's Rust code at run time (roadmap D3 stands).
- A central list of key plugins anywhere except the one line each plugin already has in
  `tx-core-cli/src/plugins.rs::MANIFEST`.

## Constraints

- Everything in the roadmap's Global Constraints and D1–D7 still binds. In particular: every
  tmux call from Rust goes through the socket-bound `Ui` helper; option names stay hyphenated
  (`@tx-id`, `@tx-view`, `@tx-name`, `@tx-label-rev`, `@tx-bin`, `@tx-state`).
- Key fragments stay bash snippets in the style of today's `tx-ide.tmux` (compose a temp conf,
  `source-file` it), reading `@tx-bin` / `@tx-state` from the server.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo fmt --all -- --check` and `shellcheck` on every emitted script stay green.

## Decisions

### D8. The core gains a generic resource slot

A plugin may publish named static blobs. The core stores and lists them; it never reads one.

```rust
impl Registry {
    /// `name` is `[A-Za-z0-9_./-]+`, no leading `/`, no `..` segment, unique across plugins
    /// (`PluginError::DuplicateResource`). Convention: `<kind>/<file>`, e.g. `keys/kill.tmux`,
    /// `bin/tmux-nav`, `conf/tmux.conf`.
    pub fn resource(&mut self, name: &'static str, bytes: &'static [u8]) -> Result<(), PluginError>;
}
pub struct Resource { pub plugin: String, pub name: String, pub bytes: &'static [u8] }
impl TxCore {
    /// Resources of active plugins whose name starts with `prefix`, in manifest order then name order.
    pub fn resources(&self, prefix: &str) -> Vec<Resource>;
}
```

Why it passes D2: it names no feature, and it has four consumers on day one (every key plugin's
fragment, `ide`'s conf and scripts) and two more in the roadmap (hooks' settings templates,
Antigravity's `.agents` files). Disabled plugins register nothing, so their resources are absent,
exactly like their verbs.

### D9. One plugin per feature

| id | crate | verbs | resources | ext key |
|---|---|---|---|---|
| `picker` | `poc/plugins/picker` (`tx-picker`) | `pick`, `_list` | `keys/picker.tmux` (prefix+t) | — |
| `edit` | `poc/plugins/edit` (`tx-edit`) | `_edit-session`, `_edit-tag` | `keys/edit.tmux` (prefix+e), `bin/tmux-edit-tx-session` | `edit` (`{"group": …}`) |
| `kill` | `poc/plugins/kill` (`tx-kill`) | `kill` | `keys/kill.tmux` (prefix+X), `bin/tmux-kill-tx-session` | — |
| `labels` | `poc/plugins/labels` (`tx-labels`) | `_relabel`, `_pane-label`, `_pane-info`, `_tmux-name`, `whoami` | `keys/labels.tmux` (prefix+s, pane-border-format, the after-new-window hook) | — |
| `ide` | `poc/plugins/ide` (`tx-ide`) | `ui install`, `ui attach`, `_resource <name>` | `conf/tmux.conf` (theme), `keys/ide.tmux` (nav keys, C-u/C-d, M-digits, user-keys, status-right, pane-border style), `bin/tmux-nav`, `bin/tmux-system-resources` | — |

`views` disappears as an id, a crate and an ext key. `labels` groups the "what is this session
called" readers; `whoami` rides along because it is the same lookup from the inside.

### D10. Shared code is a library, not a plugin

`poc/tx-ui` (`tx-ui`, a plain library crate, not in the manifest) holds what several plugins
need: the `Ui` tmux helper (`tmux.rs`), the row renderer and palette (`render.rs`), the modal
editor (`editor.rs`). The editor's duplicated ANSI constants are replaced by `render`'s. Crate
dependencies on `tx-ui` are library reuse, allowed by D3.

### D11. `ide` composes; the fragments own their keys

`txc ui install`:

1. Refuses an exe or state path containing whitespace or `"'$#;\`` and a backslash (today's
   guard). `--poc` is gone.
2. Writes every `bin/*` resource of active plugins to `<state_dir>/bin/<file>` (mode 0755),
   every `keys/*` to `<state_dir>/keys/<file>`, every `conf/*` to `<state_dir>/conf/<file>`.
   Files in those three directories that no active plugin provides are deleted.
3. Writes `<state_dir>/tmux.conf`:
   ```
   # tx-core server config, written by `txc ui install`. Re-run to update.
   set -s exit-empty on
   set -g history-limit 50000
   set -g detach-on-destroy off
   set -g @tx-bin "<exe>"
   set -g @tx-state "<state_dir>"
   source-file "<state_dir>/conf/tmux.conf"
   run-shell "<state_dir>/keys/ide.tmux"
   run-shell "<state_dir>/keys/<id>.tmux"      # one line per other active key plugin, manifest order
   ```
4. Records which prefix keys each fragment declares in `<state_dir>/keys/.installed`
   (`<id>\t<key> <key>…` per line). A fragment declares its keys on a comment line
   `# keys: X` near its top (space-separated key names in the prefix table; `none` if it binds
   only root-table or non-key things). The pane-border format counts as no key.
5. If the server is running: for every key in the previous `.installed` whose plugin is no longer
   active, `tmux unbind <key>`; then `source-file <state_dir>/tmux.conf`; print `reloaded`.

Scripts call `$TX` built from `@tx-bin`/`@tx-state` as today; `$DIR` references become
`<state_dir>/bin`. `tmux-kill-tx-session` falls back to the raw target name when
`$TX _pane-info` fails or prints nothing, so `kill` works with `labels` off.

The picker's fzf binds that call other plugins degrade at run time: `pick` includes the ctrl-t
tag bind only when `core.verb("_edit-tag")` is `Some`, and the ctrl-d/y kill binds only when
`core.verb("kill")` is `Some`. Asking the host whether a verb exists is introspection of the
host, not a call into a plugin.

### D12. ext keys follow the owner

The group override is `ext["edit"].group`. The picker's role column reads `ext["spawn"].kind`
(`nvim` | `view`), which phase 2's `spawn` plugin writes; until then every process session shows
`shell`. The roadmap's mentions of `ext.views` change accordingly.

## Decisions taken after the builder check (author's calls)

- **The `@tx-ide-*` toggles go away.** Plugin enablement replaces them: `agent-scroll`, `nav-keys`,
  `pane-keys`, `window-keys` and the palette are `ide`'s and always on while `ide` is active;
  `popups`, `pane-borders`, `session-labels` are the plugins `picker`, `labels`. `bin/tmux-nav`
  stops reading `@tx-ide-nav-keys`. The verb-gated binds for `T`, `/`, `f`, `n`, `a` are dropped
  from `ide.tmux`; the plugins of phases 2–6 bring their own `keys/<id>.tmux`.
- **Non-key state is declared too.** A fragment may carry `# unset: <option> …` and
  `# unhook: <hook> …` lines; on a live reload `ide` runs `set -gu <option>` / `set-hook -gu
  <hook>` for every declaration of a plugin that is no longer active, before sourcing the conf.
  `labels.tmux` declares `# unset: pane-border-format` and `# unhook: after-new-window`.
- **A removed key is dead, not restored.** `unbind` leaves it unbound; the IDE's own
  `conf/tmux.conf` is where a stock binding would be re-added if wanted. No defaults table.
- **Legacy servers are not migrated.** When `.installed` is absent and the server is running,
  `ui install` prints `legacy bindings may remain; restart the server to drop them` after
  `reloaded`.
- **`_resource <name>`** prints the bytes of any active plugin's resource; it exists for tests
  and debugging only, nothing depends on it.
- **Intermediate commits keep the gate green, not a working install.** Tasks 3 and 4 copy the
  scripts and fragments into their plugins; Task 5 deletes `poc/bin` and `poc/tmux`.
- **Border look belongs to `ide`.** `pane-border-style`, `pane-active-border-style`,
  `pane-border-lines heavy`, `pane-border-indicators both` and `pane-border-status off` live in
  `ide.tmux`; `labels.tmux` owns only `pane-border-format` and the `after-new-window` hook.

## What is left to the builder

- Internal module layout inside each plugin crate (one `lib.rs` is fine for `kill`).
- Whether `tx-ui` re-exports `tx_core` items for convenience.
- How `.installed` is parsed; any simple line format is fine as long as the unbind step works.
- Test file layout for the CLI integration tests: one file per plugin under
  `poc/tx-core-cli/tests/`, sharing one `tests/common/mod.rs` harness (fold `hooks.rs`'s copy in).

## Build order

Each task ends with the workspace gate green and one commit. Reviews happen per task.

### Task 1: resource slot in the core

**Files:** modify `poc/tx-core/src/plugin.rs`, `poc/tx-core/src/lib.rs` (re-export `Resource`); test `poc/tx-core/tests/plugins.rs`.

- `Registry::resource(name, bytes)` with name validation and `PluginError::DuplicateResource(String)` / `PluginError::InvalidResourceName(String)`.
- `TxCore::resources(prefix) -> Vec<Resource>` in manifest order then name order; disabled plugins contribute nothing.
- Tests: a stub plugin publishing two resources is listed by prefix; a duplicate name across two plugins fails `plug`; `../x` and `/x` are refused; a disabled plugin's resources are absent.
- Commit `feat(core): plugin resources — named static blobs listed by prefix`.

### Task 2: `tx-ui` library crate

**Files:** create `poc/tx-ui/{Cargo.toml,src/lib.rs}`; move `poc/plugins/views/src/{tmux,render,editor}.rs` into it; `poc/Cargo.toml` members; `poc/plugins/views` depends on `tx-ui` and re-imports.

- Replace `editor.rs`'s inlined `BOLD`, `ACCENT_ANSI`, `FG_ANSI`, `WARN_ANSI` with `render`'s; keep the other four as `pub` in `render.rs`.
- All existing tests move with their modules and stay green; `views` still works end to end.
- Commit `refactor(ui): shared tmux helper, renderer and editor in the tx-ui library`.

### Task 3: `labels` and `kill` plugins

**Files:** create `poc/plugins/labels`, `poc/plugins/kill`; move the verbs out of `views/src/session_verbs.rs`; move `poc/bin/tmux-kill-tx-session` into `kill` as `include_bytes!` resource `bin/tmux-kill-tx-session` (with the `_pane-info` fallback); new `keys/labels.tmux` and `keys/kill.tmux` extracted from `poc/tmux/tx-ide.tmux` (the `s` bind + pane-border-format + after-new-window hook; the `X` bind), each with a `# keys:` line; MANIFEST += both; `poc/tx-core-cli/tests/common/mod.rs` harness; tests move from `tests/views.rs` to `tests/labels.rs` and `tests/kill.rs`.

- Commit `feat(plugins): labels and kill as their own plugins`.

### Task 4: `edit` and `picker` plugins

**Files:** create `poc/plugins/edit`, `poc/plugins/picker`; move `edit.rs` and `picker.rs`; `edit` owns `ext["edit"]` and the `bin/tmux-edit-tx-session` resource and `keys/edit.tmux`; `picker` owns `keys/picker.tmux` and the verb-presence checks for its ctrl-t / ctrl-d / y binds; role column reads `ext["spawn"].kind`; MANIFEST += both; tests to `tests/edit.rs`, `tests/picker.rs`, `poc/plugins/edit/tests/edit.rs`.

- New tests: `fzf_args` without `_edit-tag` has no ctrl-t bind; without `kill` has no ctrl-d / y binds.
- Commit `feat(plugins): edit and picker as their own plugins`.

### Task 5: `ide` composes from resources

**Files:** rename `poc/plugins/views` → `poc/plugins/ide` (crate `tx-ide`, id `ide`); `install.rs` per D11; resources `conf/tmux.conf`, `keys/ide.tmux`, `bin/tmux-nav`, `bin/tmux-system-resources` via `include_bytes!`; hidden verb `_resource <name>` printing a resource's bytes; delete `poc/tmux/` and `poc/bin/`; MANIFEST entry renamed; `plugins.rs` RESERVED unchanged; tests `tests/ide.rs`.

- Tests: `ui install` writes `bin/`, `keys/`, `conf/` and the conf text (literal); the fragment loads (poll `list-keys -T prefix t` for `pick`); with `kill: { enabled: false }` in yaml, re-running `ui install` on the running server removes the `X` bind and `keys/kill.tmux`; `ui attach` lands in the view home (existing test moves here).
- Commit `feat(ide): compose the server config from plugin resources; drop --poc`.

### Task 6: docs and roadmap

**Files:** `poc/README.md` (plugin table: five rows replace `views`; Running the IDE: no `--poc`; "Adding a key" paragraph: a crate, a verb, a `keys/<id>.tmux` resource, one MANIFEST line); roadmap Phase 1 note (`views` → the five plugins; `ext.edit.group`; `ext.spawn.kind`; D8–D12 referenced); `docs/ARCHITECTURE.md` poc rows if they name `views`.

- Commit `docs: key plugins — one plugin per prefix key`.
