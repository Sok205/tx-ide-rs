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

The CLI suites (`tx-core-cli/tests`) run the plugins the way an install does, as their own
programs: `tx` links none, so there is no other way. Cargo builds another package's program only
for that package's own tests, so the harness (`tests/common/mod.rs`, `shipped()`) runs
`cargo build --workspace --bins` once per test process, in the profile and target directory of
the test itself, and links one plugin root under the target directory,
`<target>/<profile>/tx-shipped/<id>/`, for every `plugins/<id>/plugin.yaml`: the manifest, each
resource and each program. Every sandbox (`Cli::new()`) is given that root and then its own
`<config>/plugins` with `--plugin-dir`; they are links, so a sandbox pays nothing for them. A
test's own plugin is a directory it writes under its `<config>/plugins/` (`Cli::plugin`): a
manifest and shell scripts. A sandbox whose roots are replaced this way is never told about the
shipped stamp; the tests of that warning, and of `tx update --bundles`, run on the default roots.

**Running from a checkout.** `tx` alone is only the built-ins: the plugins have to be laid out
where it looks for them. Build everything, then lay the shipped plugins out as links, once:

```sh
cargo build --workspace
target/debug/tx update --bundles plugins --binaries target/debug --link
```

`<data>/shipped/` then holds links into the checkout and the build, so a rebuild is live; run
the command again after adding a plugin, a program or a resource. To try the IDE as an install
has it, run `./install --upgrade` instead: it rebuilds the selected plugins in release, installs
`tx` and `txi` and lays everything out again (the server's configuration names the `tx` binary
by path). A `tx` newer or older than the shipped plugins says so on every command:
`tx: shipped plugins are from tx <version>: run install --upgrade`.

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

A plugin of this repository is a package under `plugins/<id>`, named `tx-<id>`: a library with
the plugin, a program that hosts it, a manifest, and the files it publishes. Nothing outside the
package names it but the workspace: add `plugins/<id>` to `members` in the root `Cargo.toml`.
`tx` finds it, like any plugin, as a directory with a `plugin.yaml`; `plugins/kill` is the
template and [Writing plugins](writing-plugins.md) the reference.

```text
plugins/notes/
  Cargo.toml        package tx-notes; a [[bin]] named tx-notes; depends on tx-core, tx-plugin
                    and, for anything that talks to tmux, tx-client
  plugin.yaml       the manifest: id, description, tier, verbs, resources
  src/lib.rs        the plugin: its `impl Plugin`, its verbs
  src/main.rs       the program: three lines
  keys/notes.tmux   a key fragment, listed under `resources`
  tests/bundle.rs   the manifest against what the plugin registers
```

The library holds a type implementing `Plugin`, whose `register` adds the verbs (and an engine,
sinks or an updater, when it has them):

```rust,ignore
pub struct NotesPlugin;

impl Plugin for NotesPlugin {
    fn id(&self) -> &'static str { "notes" }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(NotesVerb));
        Ok(())
    }
}
```

The program hosts it:

```rust,ignore
fn main() -> std::process::ExitCode {
    tx_plugin::sdk::main(vec![Box::new(tx_notes::NotesPlugin)])
}
```

And the manifest says what `tx` needs to know without starting it: each verb with its usage
line, the program and the verb's own name as the first argument, and each file to publish.

```yaml
api: 1
id: notes
description: Notes on the agent in the active pane.
tier: extra

verbs:
  - name: notes
    usage: "notes <name>   open the notes of a session"
    bin: tx-notes
    args: [notes]

resources:
  - keys/notes.tmux
```

A verb may not take the name of a built-in (`create`, `send`, `stop`, …): `tx harness check` reports it
and the built-in stays. `tier` decides which presets install the plugin (`core`, `ide`, `extra`);
`./install` builds the program of every selected plugin and `tx update` lays the bundle out.

A plugin that keeps files of its own up to date (generated config, its own one-shot steps)
registers an `Updater` with `r.updater(…)` and names the program under `update:` in the manifest
(`args: [--tx-update]`): `tx update` runs every active plugin's updater after the core's update
steps. It runs on every `tx update`, so it must be idempotent.

**A key** is a `keys/<id>.tmux` resource: a script the server's config (written by `tx update`)
runs through the interpreter its first line names, never by executing the file (the first direct
run of a new executable file is slow on macOS). A comment near its top declares the prefix keys
it binds, `# keys: <key>` (`none` for root-table binds only); if it sets global options or hooks
it also declares `# unset: …` and `# unhook: …` so that disabling the plugin undoes them. It
finds `tx` and its flags through the server options `@tx-bin` and `@tx-arguments`, as every
shipped fragment does. Helper scripts go in as `bin/<file>` resources.

**Tests.** `tests/bundle.rs` is one call, `tx_plugin::testing::check_bundle`: the manifest
reads, every resource is a file of the package, every `bin` is a program it builds, and the
verbs, their usages, the engine kind and the updater are what `register` produces. The plugin's
own tests stay in-process (`core.plug(vec![Box::new(NotesPlugin)], …)`). Add a
`tx-core-cli/tests/<id>.rs` that drives the real binaries through the `tests/common` harness:
one temporary directory for all five directories, a private server, and `shim_path` for stand-in
executables such as a `claude` that only sleeps. A plugin that publishes resources must also be
listed in `tests/ide.rs::update_materialises_resources`; a new key means extending
`expected_conf`, `start_server`, `update_fragment_loads` and
`fragments_do_nothing_outside_an_installed_server` in the same file.

Finally, follow the rule from [Architecture](architecture.md): act on other plugins through `tx`
verbs and tmux, never through their files.
