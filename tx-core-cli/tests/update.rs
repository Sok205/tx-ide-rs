//! `tx update` through the binary: the shipped plugins laid out in `<data>/shipped` from a
//! checkout's bundles and its build, the resources published under `<data>`, and the `update`
//! program of an installed plugin, updating or retiring. The sandboxes here name no plugin
//! root, so the roots are the defaults (`<data>/shipped`, then `<config>/plugins`), and every
//! plugin is a test's own: a manifest, files and shell scripts.

mod common;

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The five directories, each its own, and a checkout: `plugins/` (the bundles) and `build/`
/// (the built programs).
struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> Self {
        // No root of these sandboxes, but its build replaces the binaries: no `tx` of this
        // process runs before it is over.
        common::shipped();
        let dir = tempfile::Builder::new()
            .prefix("tx")
            .tempdir_in("/tmp")
            .unwrap();
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn data(&self) -> PathBuf {
        self.path("data")
    }

    fn shipped(&self) -> PathBuf {
        self.data().join("shipped")
    }

    fn bundles(&self) -> PathBuf {
        self.path("plugins")
    }

    /// `<config>/plugins`: the operator's own plugins.
    fn own(&self) -> PathBuf {
        self.path("config/plugins")
    }

    fn write_yaml(&self, text: &str) {
        std::fs::create_dir_all(self.path("config")).unwrap();
        std::fs::write(self.path("config/config.yaml"), text).unwrap();
    }

    /// A plugin `id` of the operator's own that publishes `resources`, each a name and its
    /// text.
    fn publishing(&self, id: &str, resources: &[(&str, &str)]) -> PathBuf {
        let mut manifest = format!("api: 1\nid: {id}\ndescription: Publishes.\nresources:\n");
        for (name, _) in resources {
            manifest.push_str(&format!("  - {name}\n"));
        }
        let directory = common::write_plugin(&self.own(), id, &manifest, &[]);
        for (name, text) in resources {
            let path = directory.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        directory
    }

    /// A file under `<data>`, as text.
    fn published(&self, name: &str) -> String {
        std::fs::read_to_string(self.data().join(name)).unwrap()
    }

    /// A plugin `id` in `root` whose `update` program is the script `bin/up` with `body`.
    fn updating(&self, root: &Path, id: &str, body: &str) {
        common::write_plugin(
            root,
            id,
            &format!("api: 1\nid: {id}\ndescription: The {id} plugin.\nupdate:\n  bin: up\n"),
            &[("up", body)],
        );
    }

    fn binaries(&self) -> PathBuf {
        self.path("build")
    }

    /// `tx` on this sandbox's directories, with nothing of the session the tests run in.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tx"));
        for name in ["config", "data", "state", "cache", "run"] {
            command.arg(format!("--{name}-dir")).arg(self.path(name));
        }
        command.args(args);
        for variable in [
            "TMUX",
            "TX_SESSION_ID",
            "TX_PLUGIN_PATH",
            "TX_PROFILE_PATH",
            "TX_SKILL_PATH",
            "TX_CONFIG_FILE",
            "TX_SESSION_ENV",
            "TX_PLUGIN_TRACE",
            "TX_PLUGIN_RETIRE",
        ] {
            command.env_remove(variable);
        }
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    /// The standard output of a `tx` that succeeds.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(output.status.success(), "tx {args:?}: {}", stderr(&output));
        stdout(&output)
    }

    /// `tx update --bundles <plugins> --binaries <build>` and then `extra`.
    fn update_arguments(&self, extra: &[&str]) -> Vec<String> {
        let mut arguments = vec![
            "update".to_owned(),
            "--bundles".to_owned(),
            self.bundles().to_string_lossy().into_owned(),
            "--binaries".to_owned(),
            self.binaries().to_string_lossy().into_owned(),
        ];
        arguments.extend(extra.iter().map(|word| (*word).to_owned()));
        arguments
    }

    fn update(&self, extra: &[&str]) -> Output {
        let arguments = self.update_arguments(extra);
        self.run(&arguments.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// The `shipped:` lines of an update that succeeds.
    fn shipped_lines(&self, extra: &[&str]) -> Vec<String> {
        let output = self.update(extra);
        assert!(output.status.success(), "{}", stderr(&output));
        stdout(&output)
            .lines()
            .filter(|line| line.starts_with("shipped:"))
            .map(str::to_owned)
            .collect()
    }

    /// The bundle `id`: one verb of its own name, run by the built program `tx-<id>`, which
    /// notes every run in `<sandbox>/ran` and answers the probe.
    fn bundle(&self, id: &str) {
        common::write_plugin(
            &self.bundles(),
            id,
            &format!(
                "api: 1\nid: {id}\ndescription: The {id} plugin.\n\
                 verbs:\n  - {{name: {id}, usage: \"{id}\", bin: tx-{id}}}\n"
            ),
            &[],
        );
        common::shim_path_in(
            &self.binaries(),
            &[(
                &format!("tx-{id}"),
                &format!(
                    "echo \"{id} $*\" >> {}\n[ \"$1\" = --tx-probe ] && exit 0\necho \"{id} runs in $TX_PLUGIN_DIR\"",
                    self.path("ran").display()
                ),
            )],
        );
    }

    fn select(&self, plugins: &str) {
        std::fs::create_dir_all(self.shipped()).unwrap();
        std::fs::write(
            self.shipped().join(".selection"),
            format!("{{\"preset\": null, \"plugins\": [{plugins}]}}"),
        )
        .unwrap();
    }

    /// What the programs noted, one line per run.
    fn ran(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("ran"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The names in the shipped root that are plugins.
    fn laid_out(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.shipped())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with('.'))
            .collect();
        names.sort();
        names
    }

    fn stamp(&self) -> serde_json::Value {
        let text = std::fs::read_to_string(self.shipped().join(".stamp")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn origin(&self, id: &str) -> serde_json::Value {
        let rows: serde_json::Value =
            serde_json::from_str(&self.ok(&["plugins", "--json"])).unwrap();
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("no plugin {id}: {rows}"));
        row["origin"].clone()
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn inode(path: &Path) -> u64 {
    std::fs::symlink_metadata(path).unwrap().ino()
}

#[test]
fn linked_bundles_are_links_into_the_checkout_and_the_build() {
    let sandbox = Sandbox::new();
    sandbox.bundle("hello");
    assert_eq!(sandbox.shipped_lines(&["--link"]), ["shipped: hello"]);
    let hello = sandbox.shipped().join("hello");
    assert_eq!(
        std::fs::read_link(hello.join("plugin.yaml")).unwrap(),
        sandbox.bundles().join("hello/plugin.yaml")
    );
    assert_eq!(
        std::fs::read_link(hello.join("bin/tx-hello")).unwrap(),
        sandbox.binaries().join("tx-hello")
    );
    assert_eq!(
        sandbox.stamp(),
        serde_json::json!({
            "tx_version": env!("CARGO_PKG_VERSION"),
            "api": 1,
            "bundles": sandbox.bundles(),
            "binaries": sandbox.binaries(),
            "linked": true,
        })
    );
    assert_eq!(sandbox.origin("hello"), "shipped");
    assert_eq!(
        sandbox.ok(&["hello"]),
        format!("hello runs in {}\n", hello.display())
    );
    assert!(sandbox.shipped_lines(&["--link"]).is_empty());
    assert_eq!(sandbox.ran(), ["hello "], "a link is not probed");
}

#[test]
fn copied_bundles_are_files_and_each_program_is_probed_once() {
    let sandbox = Sandbox::new();
    sandbox.bundle("hello");
    sandbox.bundle("other");
    let trace = sandbox.path("trace");
    let update = || {
        let arguments = sandbox.update_arguments(&[]);
        let output = sandbox
            .command(&arguments.iter().map(String::as_str).collect::<Vec<_>>())
            .env("TX_PLUGIN_TRACE", &trace)
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", stderr(&output));
        stdout(&output)
    };
    let out = update();
    assert!(
        out.starts_with("shipped: hello\nshipped: other\n"),
        "first of all: {out}"
    );
    let program = sandbox.shipped().join("hello/bin/tx-hello");
    for file in [&program, &sandbox.shipped().join("hello/plugin.yaml")] {
        assert!(file.symlink_metadata().unwrap().is_file(), "{file:?}");
    }
    assert_eq!(sandbox.stamp()["linked"], false);
    let mut probed = sandbox.ran();
    probed.sort();
    assert_eq!(probed, ["hello --tx-probe", "other --tx-probe"]);
    let mut traced: Vec<String> = std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    traced.sort();
    assert_eq!(
        traced,
        [
            format!("probe\thello\t{}", program.display()),
            format!(
                "probe\tother\t{}",
                sandbox.shipped().join("other/bin/tx-other").display()
            ),
        ]
    );

    let before = inode(&program);
    let out = update();
    assert!(!out.contains("shipped:"), "{out}");
    assert_eq!(inode(&program), before);
    assert_eq!(sandbox.ran().len(), 2, "nothing was written: no probe");

    // A rebuilt program is a new file, probed again; the other one is left alone.
    common::shim_path_in(&sandbox.binaries(), &[("tx-hello", "exit 0")]);
    let out = update();
    assert!(out.starts_with("shipped: hello\nupdate steps"), "{out}");
    assert_ne!(inode(&program), before);
    assert_eq!(std::fs::read_to_string(&trace).unwrap().lines().count(), 3);
}

#[test]
fn the_selection_names_the_plugins_laid_out() {
    let sandbox = Sandbox::new();
    for id in ["hello", "other", "third"] {
        sandbox.bundle(id);
    }
    sandbox.select("\"hello\"");
    assert_eq!(sandbox.shipped_lines(&[]), ["shipped: hello"]);
    assert_eq!(sandbox.laid_out(), ["hello"]);

    sandbox.select("\"hello\", \"third\"");
    assert_eq!(sandbox.shipped_lines(&[]), ["shipped: third"]);
    assert_eq!(sandbox.laid_out(), ["hello", "third"]);

    sandbox.select("\"third\"");
    assert_eq!(sandbox.shipped_lines(&[]), ["shipped: hello removed"]);
    assert_eq!(sandbox.laid_out(), ["third"]);
    let unknown = sandbox.run(&["hello"]);
    assert_eq!(unknown.status.code(), Some(2), "{}", stderr(&unknown));

    sandbox.select("");
    assert_eq!(sandbox.shipped_lines(&[]), ["shipped: third removed"]);
    assert!(sandbox.laid_out().is_empty());

    std::fs::remove_file(sandbox.shipped().join(".selection")).unwrap();
    assert_eq!(
        sandbox.shipped_lines(&[]),
        ["shipped: hello", "shipped: other", "shipped: third"]
    );
}

#[test]
fn a_program_that_is_not_built_stops_the_update_and_changes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.bundle("hello");
    sandbox.bundle("other");
    assert_eq!(sandbox.shipped_lines(&[]).len(), 2);
    let program = sandbox.shipped().join("hello/bin/tx-hello");
    let before = inode(&program);

    std::fs::remove_file(sandbox.binaries().join("tx-hello")).unwrap();
    // Not reached: the other plugin's new program is not laid out either.
    common::shim_path_in(&sandbox.binaries(), &[("tx-other", "exit 0")]);
    let output = sandbox.update(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!(
            "tx: update: plugin hello: no bin/tx-hello in {} and no {}: build it \
             (cargo build --release -p tx-hello)\n",
            sandbox.bundles().join("hello").display(),
            sandbox.binaries().join("tx-hello").display()
        )
    );
    assert_eq!(stdout(&output), "", "no step ran");
    assert_eq!(inode(&program), before);
    assert_eq!(
        sandbox.ok(&["hello"]),
        format!(
            "hello runs in {}\n",
            sandbox.shipped().join("hello").display()
        )
    );
    assert_eq!(
        sandbox.ok(&["other"]),
        format!(
            "other runs in {}\n",
            sandbox.shipped().join("other").display()
        ),
        "the installed copy, not the new program"
    );
}

#[test]
fn a_selected_id_without_a_bundle_stops_the_update() {
    let sandbox = Sandbox::new();
    sandbox.bundle("hello");
    sandbox.select("\"hello\", \"nope\"");
    let output = sandbox.update(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        format!(
            "tx: update: plugin nope: no {}\n",
            sandbox.bundles().join("nope/plugin.yaml").display()
        )
    );
    assert!(sandbox.laid_out().is_empty());
}

#[test]
fn a_fresh_install_is_stamped_at_the_latest_step_without_running_any() {
    let sandbox = Sandbox::new();
    sandbox.bundle("hello");
    let output = sandbox.update(&["--link"]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    // A step that ran is `update <n>: <what it did>`.
    let ran_a_step = out.lines().any(|line| {
        line.strip_prefix("update ")
            .is_some_and(|rest| rest.starts_with(|first: char| first.is_ascii_digit()))
    });
    assert!(!ran_a_step, "{out}");
    assert_eq!(
        std::fs::read_to_string(sandbox.data().join("update-version")).unwrap(),
        format!("{}\n", tx_core::updates::latest())
    );
}

#[test]
fn the_directories_come_together_and_the_link_only_with_them() {
    let sandbox = Sandbox::new();
    let bundles = sandbox.bundles();
    let bundles = bundles.to_str().unwrap();
    for (arguments, message) in [
        (
            vec!["update", "--bundles", bundles],
            "tx: --bundles and --binaries go together",
        ),
        (
            vec!["update", "--binaries", bundles],
            "tx: --bundles and --binaries go together",
        ),
        (
            vec!["update", "--link"],
            "tx: --link needs --bundles and --binaries",
        ),
        (
            vec!["update", "--binaries", bundles, "--bundles"],
            "tx: --bundles needs a value",
        ),
        (vec!["update", "now"], "tx: unexpected argument \"now\""),
        (
            vec!["update", "--bundles", bundles, "--binaries", bundles, "x"],
            "tx: unexpected argument \"x\"",
        ),
    ] {
        let output = sandbox.run(&arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        let stderr = stderr(&output);
        assert_eq!(stderr.lines().next(), Some(message), "{arguments:?}");
        assert!(stderr.contains("usage: tx "), "{arguments:?}: the usage");
    }
    assert!(!sandbox.data().exists(), "misuse creates nothing");
}

/// An updater that makes `<data>/<id>-made`, and takes it back when it is retired.
const MAKES: &str = r#"if [ -n "$TX_PLUGIN_RETIRE" ]; then rm -f "$TX_DATA_DIR/$TX_PLUGIN_ID-made"; echo retired; exit 0; fi
touch "$TX_DATA_DIR/$TX_PLUGIN_ID-made"; echo "did x"; echo "tx-update: respawn-agents""#;

#[test]
fn an_installed_plugins_updater_is_its_program_with_its_lines_under_its_id() {
    let sandbox = Sandbox::new();
    sandbox.updating(&sandbox.own(), "hello", MAKES);
    sandbox.updating(
        &sandbox.own(),
        "after",
        r#"echo "in $TX_PLUGIN_DIR for $TX_PLUGIN_ID"; echo "args: $*"; cat"#,
    );
    // One without an updater, and one that does not load: neither is run.
    common::write_plugin(
        &sandbox.own(),
        "plain",
        "api: 1\nid: plain\ndescription: No updater.\n",
        &[],
    );
    sandbox.updating(&sandbox.own(), "broken", "echo never");
    std::fs::OpenOptions::new()
        .append(true)
        .open(sandbox.own().join("broken/plugin.yaml"))
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"colour: red\n"))
        .unwrap();

    let trace = sandbox.path("trace");
    let output = sandbox
        .command(&["update"])
        .env("TX_PLUGIN_TRACE", &trace)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        format!(
            "update steps are at {}: nothing pending\n\
             after: in {} for after\n\
             after: args: \n\
             hello: did x\n\
             respawn running agents to pick this up\n",
            tx_core::updates::latest(),
            sandbox.own().join("after").display(),
        ),
        "the updaters in plugin order; no input"
    );
    assert!(sandbox.data().join("hello-made").is_file());
    assert_eq!(
        std::fs::read_to_string(&trace).unwrap(),
        format!(
            "update\tafter\t{}\nupdate\thello\t{}\n",
            sandbox.own().join("after/bin/up").display(),
            sandbox.own().join("hello/bin/up").display()
        )
    );
}

#[test]
fn an_updater_that_fails_is_named_and_the_others_still_run() {
    let sandbox = Sandbox::new();
    sandbox.updating(&sandbox.own(), "hello", "echo before; exit 3");
    sandbox.updating(&sandbox.own(), "later", "echo ran");
    common::write_plugin(
        &sandbox.own(),
        "nothing",
        "api: 1\nid: nothing\ndescription: Its program is not there.\nupdate:\n  bin: up\n",
        &[],
    );
    sandbox.updating(&sandbox.own(), "signalled", "kill -TERM $$");
    let output = sandbox.run(&["update"]);
    assert_eq!(output.status.code(), Some(1));
    let out = stdout(&output);
    assert!(out.ends_with("hello: before\nlater: ran\n"), "{out}");
    let errors = stderr(&output);
    let mut lines = errors.lines();
    assert_eq!(lines.next(), Some("tx: update: hello: exit 3"));
    let missing = lines.next().unwrap();
    assert!(
        missing.starts_with(&format!(
            "tx: update: nothing: cannot run {}: ",
            sandbox.own().join("nothing/bin/up").display()
        )),
        "{errors}"
    );
    assert_eq!(lines.next(), Some("tx: update: signalled: exit 143"));
    assert_eq!(lines.next(), None, "{errors}");
}

#[test]
fn a_disabled_plugin_is_retired_by_its_updater() {
    let sandbox = Sandbox::new();
    sandbox.updating(&sandbox.own(), "hello", MAKES);
    sandbox.ok(&["update"]);
    assert!(sandbox.data().join("hello-made").is_file());

    sandbox.write_yaml("plugins:\n  hello: { enabled: false }\n");
    let out = sandbox.ok(&["update"]);
    assert!(out.ends_with("hello: retired\n"), "{out}");
    assert!(!sandbox.data().join("hello-made").exists());
    // Safe to repeat: it is retired again by the next update.
    assert!(sandbox.ok(&["update"]).ends_with("hello: retired\n"));

    sandbox.write_yaml("plugins:\n  hello: { enabled: true }\n");
    let out = sandbox.ok(&["update"]);
    assert!(out.contains("hello: did x\n"), "{out}");
    assert!(sandbox.data().join("hello-made").is_file());

    // A variable of the caller's does not retire a plugin that is on.
    let output = sandbox
        .command(&["update"])
        .env("TX_PLUGIN_RETIRE", "1")
        .output()
        .unwrap();
    assert!(stdout(&output).contains("hello: did x\n"), "{output:?}");
}

#[test]
fn a_shipped_plugin_that_left_the_selection_is_retired_before_it_is_removed() {
    let sandbox = Sandbox::new();
    sandbox.updating(&sandbox.bundles(), "gone", MAKES);
    sandbox.updating(&sandbox.bundles(), "stays", "echo here");
    sandbox.bundle("plain");
    let out = sandbox.ok(&sandbox
        .update_arguments(&[])
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>());
    assert!(out.contains("gone: did x\n"), "{out}");
    assert!(sandbox.data().join("gone-made").is_file());

    // `plain` has no updater: it goes without a word. `gone` is told first.
    sandbox.select("\"stays\"");
    let output = sandbox.update(&[]);
    assert!(output.status.success(), "{}", stderr(&output));
    let out = stdout(&output);
    assert!(
        out.starts_with("gone: retired\nshipped: gone removed\nshipped: plain removed\n"),
        "{out}"
    );
    assert!(out.ends_with("stays: here\n"), "{out}");
    assert!(!sandbox.data().join("gone-made").exists());
    assert_eq!(sandbox.laid_out(), ["stays"]);
}

#[test]
fn a_retire_that_fails_is_named_and_the_directory_goes_all_the_same() {
    let sandbox = Sandbox::new();
    sandbox.updating(&sandbox.bundles(), "gone", "echo leaving; exit 4");
    assert!(sandbox.update(&[]).status.code() == Some(1));
    sandbox.select("");
    let output = sandbox.update(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), "tx: update: gone: exit 4\n");
    let out = stdout(&output);
    assert!(
        out.starts_with("gone: leaving\nshipped: gone removed\nupdate steps"),
        "the update goes on: {out}"
    );
    assert!(sandbox.laid_out().is_empty());
}

const FRAGMENT: &str = "#!/usr/bin/env bash\n# keys: H\ntmux bind H display-message hello\n";

#[test]
fn cores_roles_and_definitions_are_published_without_a_plugin() {
    let sandbox = Sandbox::new();
    let out = sandbox.ok(&["update"]);
    assert!(!out.contains("ide:"), "{out}");
    for file in [
        "roles/COMMON.md",
        "definitions/agents/developer.md",
        "definitions/mcp.json",
    ] {
        assert!(sandbox.data().join(file).is_file(), "{file}");
    }
    for directory in ["keys", "bin", "conf", "nvim"] {
        assert!(!sandbox.data().join(directory).exists(), "{directory}");
    }
}

#[test]
fn an_installed_plugins_resources_are_published_and_go_when_it_is_disabled() {
    let sandbox = Sandbox::new();
    sandbox.publishing(
        "hello",
        &[
            ("keys/hello.tmux", FRAGMENT),
            ("agents/NOTE.md", "a note\n"),
        ],
    );
    sandbox.ok(&["update"]);
    let fragment = sandbox.data().join("keys/hello.tmux");
    assert_eq!(sandbox.published("keys/hello.tmux"), FRAGMENT);
    let metadata = fragment.symlink_metadata().unwrap();
    assert!(metadata.is_file(), "a copy, whatever the plugin holds");
    assert_eq!(metadata.mode() & 0o7777, 0o755);
    assert_eq!(sandbox.published("roles/NOTE.md"), "a note\n");

    // Nothing changed: the files are left alone.
    let before = inode(&fragment);
    sandbox.ok(&["update"]);
    assert_eq!(inode(&fragment), before);

    sandbox.write_yaml("plugins:\n  hello: { enabled: false }\n");
    sandbox.ok(&["update"]);
    assert!(!fragment.exists());
    assert!(!sandbox.data().join("roles/NOTE.md").exists());
    assert!(sandbox.data().join("roles/COMMON.md").is_file());
}

#[test]
fn a_later_source_replaces_a_resource_of_the_same_name() {
    let sandbox = Sandbox::new();
    sandbox.ok(&["update"]);
    let cores = sandbox.published("roles/COMMON.md");
    // In plugin order `first` comes before `second`; both come after core.
    sandbox.publishing(
        "first",
        &[
            ("agents/COMMON.md", "first\n"),
            ("agents/NOTE.md", "first\n"),
        ],
    );
    sandbox.publishing("second", &[("agents/NOTE.md", "second\n")]);
    sandbox.ok(&["update"]);
    assert_eq!(sandbox.published("roles/COMMON.md"), "first\n");
    assert_eq!(sandbox.published("roles/NOTE.md"), "second\n");

    sandbox.write_yaml("plugins:\n  second: { enabled: false }\n");
    sandbox.ok(&["update"]);
    assert_eq!(sandbox.published("roles/NOTE.md"), "first\n");

    sandbox.write_yaml("plugins:\n  first: { enabled: false }\n  second: { enabled: false }\n");
    sandbox.ok(&["update"]);
    assert_eq!(sandbox.published("roles/COMMON.md"), cores);
    assert!(!sandbox.data().join("roles/NOTE.md").exists());
}

#[test]
fn a_resource_a_plugin_lists_and_lacks_is_named_and_the_rest_is_published() {
    let sandbox = Sandbox::new();
    let hello = sandbox.publishing(
        "hello",
        &[
            ("keys/hello.tmux", FRAGMENT),
            ("agents/NOTE.md", "a note\n"),
        ],
    );
    std::fs::remove_file(hello.join("agents/NOTE.md")).unwrap();
    sandbox.updating(&sandbox.own(), "later", "echo ran");
    let output = sandbox.run(&["update"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        stderr(&output),
        "tx: update: hello: resource agents/NOTE.md is missing\n"
    );
    assert!(sandbox.data().join("keys/hello.tmux").is_file());
    assert!(
        stdout(&output).ends_with("later: ran\n"),
        "the updaters still ran"
    );
}
