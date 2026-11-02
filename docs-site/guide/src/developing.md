# Developing

## Build and test

```sh
cargo build --workspace
cargo test --workspace                                  # needs tmux on PATH
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p tx-core-cli --bin tx -- help               # the tx binary from source
```

The integration tests start real tmux servers, each on a private socket under `/tmp`, with all
five directories in a temporary directory; your default tmux server and your tx-ide data are
never touched. Tests that need tmux skip themselves when it is not on `PATH`. The workspace
enables every clippy lint group at warn level and the check runs with `-D warnings`, so a clean
build means no clippy warnings at all.

After rebuilding, run `tx ui install` again (or the `install` script) before trying the IDE: the
server's configuration names the `tx` binary by path.

## Property tests and fuzzing

Every module has unit tests, and `tx-core/tests` holds integration suites for the core's
guarantees: eight processes racing for one name, six threads sending into one pane, records that
are corrupt, misnamed or not regular files. Next to them sit `fuzz_*.rs` files: property-based and
adversarial suites written by a separate reviewer against each module after it is built. They
have found real bugs (non-UTF-8 paths, path traversal through ids, a trailing `;` in `send-keys`,
FIFOs in the record directory, a create/refresh race), each now fixed and pinned. Because they
run thousands of cases against real tmux servers, they only build behind a feature:

```sh
cargo test --workspace --features fuzz
```

The habit for new work is the same: build a piece, then have a separate pass attack it with
property tests before calling it done. Two tests stay `#[ignore]`d as documented platform limits:
a cooked-mode tty drops a single line over 1024 bytes, and `flock` cannot protect a lock file that
is deleted under its holder.

## This site

`docs-site/build.py` builds what you are reading. It runs `cargo doc -p tx-core` twice, a
**public** build and an **internal** one with private and `#[doc(hidden)]` items, adding a
Structure drawer (`g`) to every rustdoc page. The drawer's graph comes from `extract.py`, a small
lexer over `tx-core/src/*.rs` that finds items, their impls and the calls between functions (a
heuristic, not the compiler), and `graph.py`, which turns them into cards and *has*, *uses* and
*calls* edges and holds the module layout. Then it builds this guide with `mdbook` and writes a
landing page.

```sh
python3 docs-site/build.py . /tmp/tx-docs-site "tx-core @ $(git rev-parse --short HEAD)"
mdbook serve docs-site/guide --open      # the guide alone, with live reload
```

Adding, renaming or removing a `pub mod` in `tx-core/src/lib.rs` means updating
`graph.py`'s `LAYOUT_ROWS` and `MODULE_COLORS`; the build stops with an error until they match.
The GitHub workflow `docs-site.yml` builds and deploys the site on every push to `main`.

## Adding a plugin

A plugin is a crate under `plugins/<id>`, package `tx-<id>`, depending on `tx-core` and, for
anything that talks to tmux, `tx-client`. Its type implements `Plugin`:

```rust,ignore
pub struct NotesPlugin;

impl Plugin for NotesPlugin {
    fn id(&self) -> &'static str { "notes" }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(NotesVerb));
        r.resource("keys/notes.tmux", include_bytes!("../tmux/keys.tmux"))?;
        Ok(())
    }
}
```

Wire it in with three edits: add `plugins/<id>` to `members` in the root `Cargo.toml`, add
`tx-<id> = { path = "../plugins/<id>" }` to `tx-core-cli/Cargo.toml`, and add one line to
`MANIFEST` in `tx-core-cli/src/plugins.rs`. A verb may not take a name from `RESERVED` there
(the built-ins such as `create`, `send`, `stop`).

**A key** is a `keys/<id>.tmux` resource: a script `ui install` runs from the server's config. A
comment near its top declares the prefix keys it binds, `# keys: <key>` (`none` for root-table
binds only); if it sets global options or hooks it also declares `# unset: …` and `# unhook: …`
so that disabling the plugin undoes them. It finds `tx` and its flags through the server options
`@tx-bin` and `@tx-arguments`, as every bundled fragment does. Helper scripts go in as
`bin/<file>` resources.

**Tests.** Give the plugin a `tests/resources.rs` that checks what it publishes, and that a
disabled plugin publishes nothing (`plugins/kill/tests/resources.rs` is the template). Add a
`tx-core-cli/tests/<id>.rs` that drives the real binary through the `tests/common` harness: one
temporary directory for all five directories, a private server, and `shim_path` for stand-in
executables such as a `claude` that only sleeps. A plugin that publishes resources must also be
listed in `tests/ide.rs::ui_install_materialises_resources`; a new key means extending
`expected_conf`, `start_server`, `ui_install_fragment_loads` and
`fragments_do_nothing_outside_an_installed_server` in the same file.

Finally, follow the rule from [Architecture](architecture.md): act on other plugins through `tx`
verbs and tmux, never through their files.
