//! `tx-setup` as it is run, without a terminal: what it prints, what it records in
//! `<data>/shipped/.selection`, and the `config.yaml` it seeds once.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use tx_plugin::bundle::Selection;

/// A source of bundles and the directories of one install, in a temporary directory.
struct Sandbox {
    directory: tempfile::TempDir,
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

impl Sandbox {
    /// Two `core` bundles, two `ide` (one with settings), one `extra` that requires an `ide`
    /// one and a plugin that has no bundle.
    fn new() -> Self {
        let sandbox = Self {
            directory: tempfile::tempdir().unwrap(),
        };
        sandbox.bundle("engine", "tier: core\n");
        sandbox.bundle("labels", "tier: core\n");
        sandbox.bundle("ide", "tier: ide\n");
        sandbox.bundle(
            "home",
            "tier: ide\nrequires: [ide]\nsettings: { theme: koi }\n",
        );
        sandbox.bundle("themes", "requires: [home, gone]\n");
        sandbox
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }

    fn bundles(&self) -> PathBuf {
        self.path("bundles")
    }

    fn bundle(&self, id: &str, rest: &str) {
        let bundle = self.bundles().join(id);
        std::fs::create_dir_all(&bundle).unwrap();
        let text = format!("api: 1\nid: {id}\ndescription: The {id} plugin.\n{rest}");
        std::fs::write(bundle.join("plugin.yaml"), text).unwrap();
    }

    fn preset(&self, name: &str, text: &str) {
        let presets = self.path("config/presets");
        std::fs::create_dir_all(&presets).unwrap();
        std::fs::write(presets.join(format!("{name}.yaml")), text).unwrap();
    }

    fn shipped(&self) -> PathBuf {
        self.path("data/shipped")
    }

    fn selection(&self) -> Option<Selection> {
        Selection::read(&self.shipped()).unwrap()
    }

    fn record(&self, preset: Option<&str>, plugins: &[&str]) {
        let selection = Selection {
            preset: preset.map(str::to_owned),
            plugins: plugins.iter().map(|id| (*id).to_owned()).collect(),
        };
        selection.write(&self.shipped()).unwrap();
    }

    fn config_file(&self) -> PathBuf {
        self.path("config/config.yaml")
    }

    /// `tx-setup <the five directories> --bundles <bundles> <arguments>`, with nothing of the
    /// caller's environment and no terminal.
    fn run(&self, arguments: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tx-setup"));
        for kind in ["config", "data", "state", "cache", "run"] {
            command.arg(format!("--{kind}-dir")).arg(self.path(kind));
        }
        let output: Output = command
            .arg("--bundles")
            .arg(self.bundles())
            .args(arguments)
            .env_clear()
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        Run {
            code: output.status.code(),
            stdout: String::from_utf8(output.stdout).unwrap(),
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }

    fn ok(&self, arguments: &[&str]) -> Run {
        let run = self.run(arguments);
        assert_eq!(run.code, Some(0), "{arguments:?}: {}", run.stderr);
        run
    }

    /// Fails with `code`: what it said on stderr. It printed nothing on stdout.
    fn fails(&self, arguments: &[&str], code: i32) -> String {
        let run = self.run(arguments);
        assert_eq!(run.code, Some(code), "{arguments:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{arguments:?}");
        run.stderr
    }
}

fn plugin_lines(ids: &[&str]) -> String {
    ids.iter().map(|id| format!("plugin\t{id}\n")).collect()
}

fn selected(preset: Option<&str>, plugins: &[&str]) -> Option<Selection> {
    Some(Selection {
        preset: preset.map(str::to_owned),
        plugins: plugins.iter().map(|id| (*id).to_owned()).collect(),
    })
}

const ALL: [&str; 5] = ["engine", "home", "ide", "labels", "themes"];

#[test]
fn the_probe_exits_0_at_once() {
    let output = Command::new(env!("CARGO_BIN_EXE_tx-setup"))
        .arg("--tx-probe")
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}

#[test]
fn without_bundles_it_is_misuse() {
    let output = Command::new(env!("CARGO_BIN_EXE_tx-setup"))
        .arg("--list")
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.starts_with("tx setup: --bundles DIR is required\nusage: tx setup --bundles DIR "),
        "{stderr}"
    );
}

#[test]
fn list_prints_every_preset_shipped_first_and_writes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.preset(
        "mine",
        "tiers: [core]\nplugins: [themes]\nexclude: [engine, engine-codex]\n",
    );
    // With a mode flag too it still only lists.
    let run = sandbox.ok(&["--list", "--preset", "headless"]);
    assert_eq!(
        run.stdout,
        "headless\tengine labels\n\
         ide\tengine home ide labels\n\
         full\tengine home ide labels themes\n\
         mine\tlabels themes\n"
    );
    assert_eq!(run.stderr, "");
    assert!(!sandbox.path("data").exists());
    assert!(!sandbox.config_file().exists());
}

#[test]
fn a_preset_is_recorded_printed_and_seeds_the_config_once() {
    let sandbox = Sandbox::new();
    let run = sandbox.ok(&["--preset", "ide"]);
    let ids = ["engine", "home", "ide", "labels"];
    assert_eq!(run.stdout, plugin_lines(&ids));
    assert_eq!(run.stderr, "");
    assert_eq!(sandbox.selection(), selected(Some("ide"), &ids));

    let text = std::fs::read_to_string(sandbox.config_file()).unwrap();
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/tx-ide/config.yaml");
    let defaults = std::fs::read_to_string(repository).unwrap();
    let plugins = text
        .strip_prefix(&defaults)
        .expect("the repository's file comes first");
    assert!(
        plugins.ends_with(
            "plugins:\n  engine: { enabled: true }\n  home:\n    enabled: true\n    settings:\n      \
             theme: koi\n  ide: { enabled: true }\n  labels: { enabled: true }\n"
        ),
        "{plugins}"
    );
    let config = tx_plugin::config::parse(&text, &sandbox.config_file()).unwrap();
    assert_eq!(config.plugins.len(), 4);

    // A second run changes the selection and leaves the file alone.
    sandbox.ok(&["--preset", "full"]);
    assert_eq!(sandbox.selection(), selected(Some("full"), &ALL));
    assert_eq!(
        std::fs::read_to_string(sandbox.config_file()).unwrap(),
        text
    );
}

#[test]
fn a_list_of_plugins_brings_what_they_require_and_records_no_preset() {
    let sandbox = Sandbox::new();
    let run = sandbox.ok(&["--plugins", "themes,engine"]);
    assert_eq!(
        run.stdout,
        plugin_lines(&["engine", "home", "ide", "themes"])
    );
    assert_eq!(
        run.stderr,
        format!(
            "tx setup: themes requires home: added\n\
             tx setup: themes requires gone, which has no bundle in {}\n\
             tx setup: home requires ide: added\n",
            sandbox.bundles().display()
        )
    );
    assert_eq!(
        sandbox.selection(),
        selected(None, &["engine", "home", "ide", "themes"])
    );
    // A requirement that was named is not "added".
    let run = sandbox.ok(&["--plugins", "home,ide"]);
    assert_eq!(run.stderr, "");
    assert_eq!(sandbox.selection(), selected(None, &["home", "ide"]));
}

#[test]
fn an_unknown_preset_or_plugin_fails_and_writes_nothing() {
    let sandbox = Sandbox::new();
    sandbox.preset("mine", "plugins: [nope]\n");
    let no_plugin = format!(
        "tx setup: no plugin \"nope\" in {}\n",
        sandbox.bundles().display()
    );
    assert_eq!(
        sandbox.fails(&["--preset", "nope"], 1),
        "tx setup: no preset \"nope\" (known: full, headless, ide, mine)\n"
    );
    assert_eq!(sandbox.fails(&["--preset", "mine"], 1), no_plugin);
    assert_eq!(sandbox.fails(&["--plugins", "ide,nope"], 1), no_plugin);
    assert_eq!(sandbox.fails(&["--list"], 1), no_plugin);
    assert!(!sandbox.path("data").exists());
    assert!(!sandbox.config_file().exists());
}

#[test]
fn a_preset_file_with_an_unknown_key_is_named_and_is_no_preset() {
    let sandbox = Sandbox::new();
    sandbox.preset("odd", "tiers: [core]\ncolour: red\n");
    let stderr = sandbox.fails(&["--preset", "odd"], 1);
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 2, "{stderr}");
    assert!(
        lines[0].starts_with("tx setup: ") && lines[0].contains("odd.yaml"),
        "{stderr}"
    );
    assert!(
        lines[0].contains("colour") && lines[0].ends_with(": skipped"),
        "{stderr}"
    );
    assert_eq!(
        lines[1],
        "tx setup: no preset \"odd\" (known: full, headless, ide)"
    );
}

#[test]
fn a_manifest_that_does_not_read_is_named_and_is_not_available() {
    let sandbox = Sandbox::new();
    sandbox.bundle("broken", "tier: nonsense\n");
    let run = sandbox.ok(&["--preset", "full"]);
    assert_eq!(run.stdout, plugin_lines(&ALL));
    assert!(run.stderr.starts_with("tx setup: "), "{}", run.stderr);
    assert!(run.stderr.contains("broken/plugin.yaml"), "{}", run.stderr);
    assert!(run.stderr.ends_with(": skipped\n"), "{}", run.stderr);
}

#[test]
fn without_a_terminal_and_without_a_choice_it_is_full() {
    let sandbox = Sandbox::new();
    let run = sandbox.ok(&[]);
    assert_eq!(run.stdout, plugin_lines(&ALL));
    assert_eq!(sandbox.selection(), selected(Some("full"), &ALL));
    assert!(sandbox.config_file().exists());
}

#[test]
fn an_upgrade_resolves_the_recorded_preset_again_and_never_seeds_the_config() {
    let sandbox = Sandbox::new();
    sandbox.record(Some("full"), &["engine"]);
    sandbox.bundle("extra1", "");
    let run = sandbox.ok(&["--upgrade"]);
    let ids = ["engine", "extra1", "home", "ide", "labels", "themes"];
    assert_eq!(run.stdout, plugin_lines(&ids));
    assert_eq!(sandbox.selection(), selected(Some("full"), &ids));
    assert!(!sandbox.config_file().exists());
}

#[test]
fn an_upgrade_keeps_a_recorded_list_without_what_has_no_bundle_any_more() {
    let sandbox = Sandbox::new();
    sandbox.record(None, &["themes", "retired"]);
    let run = sandbox.ok(&["--upgrade"]);
    // The list is kept as it is: what `themes` requires is not added.
    assert_eq!(run.stdout, plugin_lines(&["themes"]));
    assert_eq!(
        run.stderr,
        format!(
            "tx setup: retired has no bundle in {}: dropped\n",
            sandbox.bundles().display()
        )
    );
    assert_eq!(sandbox.selection(), selected(None, &["themes"]));
}

#[test]
fn an_upgrade_of_an_install_that_recorded_nothing_is_full() {
    let sandbox = Sandbox::new();
    let run = sandbox.ok(&["--upgrade"]);
    assert_eq!(run.stdout, plugin_lines(&ALL));
    assert_eq!(sandbox.selection(), selected(Some("full"), &ALL));
    assert!(!sandbox.config_file().exists());
}

#[test]
fn an_upgrade_keeps_the_plugins_of_a_preset_that_no_longer_exists() {
    let sandbox = Sandbox::new();
    sandbox.record(Some("mine"), &["labels", "retired", "themes"]);
    let run = sandbox.ok(&["--upgrade"]);
    assert_eq!(run.stdout, plugin_lines(&["labels", "themes"]));
    assert_eq!(
        run.stderr,
        format!(
            "tx setup: preset \"mine\" no longer exists: keeping its plugins\n\
             tx setup: retired has no bundle in {}: dropped\n",
            sandbox.bundles().display()
        )
    );
    assert_eq!(sandbox.selection(), selected(None, &["labels", "themes"]));
}
