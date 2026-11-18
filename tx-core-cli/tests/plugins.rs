//! Which plugins tx has and how their verbs run: the `plugins:` section of config.yaml switches
//! the compiled-in ones and the installed ones alike; a directory with a `plugin.yaml` and a
//! program is a plugin, and its verb replaces the `tx` process.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::{lists_verb, write_plugin};

struct Cli {
    dir: tempfile::TempDir,
    /// The plugin roots every `tx` here is given, in order.
    plugin_dirs: Vec<PathBuf>,
}

impl Cli {
    /// The shared root of the shipped bundles, then the sandbox's own `<config>/plugins`.
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("tx")
            .tempdir_in("/tmp")
            .unwrap();
        let plugin_dirs = vec![
            common::shipped().to_path_buf(),
            dir.path().join("state").join("plugins"),
        ];
        Self { dir, plugin_dirs }
    }
    /// Two roots of the test's own, outside the five directories: `first`, then `second`.
    fn with_two_roots() -> Self {
        let mut cli = Self::new();
        cli.plugin_dirs = vec![cli.dir.path().join("first"), cli.dir.path().join("second")];
        cli
    }
    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    /// The sandbox's own plugin root: the last one.
    fn plugins_dir(&self) -> &Path {
        self.plugin_dirs.last().unwrap()
    }
    fn write_yaml(&self, text: &str) {
        std::fs::create_dir_all(self.state()).unwrap();
        std::fs::write(self.state().join("config.yaml"), text).unwrap();
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tx"));
        command
            .args(common::directory_flags(&self.state()))
            .args(common::plugin_flags(&self.plugin_dirs))
            .args(args)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID")
            .env_remove("TX_APPNAME")
            .env_remove("TX_PLUGIN_TRACE");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "tx {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    fn fails(&self, args: &[&str], code: i32) -> String {
        let output = self.run(args);
        assert_eq!(
            output.status.code(),
            Some(code),
            "tx {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
    /// The row of `id` in `tx plugins --json`; panics unless there is exactly one.
    fn json_row(&self, id: &str) -> serde_json::Value {
        let rows: serde_json::Value =
            serde_json::from_str(&self.ok(&["plugins", "--json"])).unwrap();
        let mut found = rows
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["id"] == id)
            .cloned();
        let row = found
            .next()
            .unwrap_or_else(|| panic!("no row for {id}: {rows}"));
        assert!(found.next().is_none(), "two rows for {id}: {rows}");
        row
    }
}

fn rows(listing: &str) -> Vec<&str> {
    listing.lines().collect()
}

fn position(rows: &[&str], row: &str) -> usize {
    rows.iter()
        .position(|candidate| *candidate == row)
        .unwrap_or_else(|| panic!("{row:?} missing from {rows:?}"))
}

/// The manifest of a plugin `id` with one verb of the same name, run by `bin/<id>`.
fn manifest(id: &str) -> String {
    format!(
        "api: 1\nid: {id}\ndescription: Says {id}.\nverbs:\n  - name: {id}\n    \
         usage: \"{id} [WORDS]   print the words and where I run\"\n    bin: {id}\n"
    )
}

/// What the `hello` program prints, then does: `fail` exits 7, `cat` copies its input.
const HELLO: &str = r#"echo "args: $*"; echo "id: $TX_PLUGIN_ID"; echo "settings: $TX_PLUGIN_SETTINGS"
echo "data: $TX_DATA_DIR"; echo "tx: $TX_BIN"
[ "$1" = fail ] && exit 7; [ "$1" = cat ] && cat; exit 0"#;

/// The `hello` plugin, in the sandbox's own root.
fn hello(cli: &Cli) -> PathBuf {
    write_plugin(
        cli.plugins_dir(),
        "hello",
        &manifest("hello"),
        &[("hello", HELLO)],
    )
}

#[test]
fn without_a_yaml_every_plugin_is_active() {
    let cli = Cli::new();
    let listing = cli.ok(&["plugins"]);
    let rows = rows(&listing);
    for row in [
        "engine-claude\tactive",
        "engine-codex\tactive",
        "ide\tactive",
        "labels\tactive",
        "kill\tactive",
        "picker\tactive",
        "edit\tactive",
    ] {
        position(&rows, row);
    }
    assert!(
        !rows.iter().any(|row| row.starts_with("views\t")),
        "{rows:?}"
    );
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\n");
}

#[test]
fn a_disabled_engine_disappears() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  engine-codex: { enabled: false }\n");
    let listing = cli.ok(&["plugins"]);
    let rows = rows(&listing);
    position(&rows, "engine-codex\tdisabled");
    position(&rows, "engine-claude\tactive");
    position(&rows, "kill\tactive");
    assert_eq!(cli.ok(&["engines"]), "claude\n");
}

/// An entry for a plugin that is not there (a typo, a plugin of another install, a retired one)
/// stops nothing: it is listed last, as `missing`.
#[test]
fn an_entry_that_names_no_plugin_is_listed_as_missing_and_every_command_works() {
    let cli = Cli::new();
    cli.write_yaml(
        "plugins:\n  pikcer: { enabled: false }\n  engine-cloud: {}\n  \
         hooks:\n    settings: { stuck_after_seconds: 600 }\n  kill: { enabled: false }\n",
    );
    let listing = cli.ok(&["plugins"]);
    let rows = rows(&listing);
    assert_eq!(
        rows[rows.len() - 3..],
        ["engine-cloud\tmissing", "hooks\tmissing", "pikcer\tmissing"],
        "{rows:?}"
    );
    position(&rows, "kill\tdisabled");
    position(&rows, "picker\tactive");
    assert_eq!(
        cli.json_row("pikcer"),
        serde_json::json!({
            "id": "pikcer", "status": "missing", "detail": null, "origin": null,
            "directory": null, "description": null, "tier": null, "requires": [], "needs": [],
            "verbs": [], "engine": null,
        })
    );
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\n");
    assert!(cli.ok(&["help"]).contains("plugin verbs:"));
    cli.ok(&["ls"]);
    cli.ok(&["layout", "ls"]);
}

#[test]
fn malformed_yaml_is_a_usage_error() {
    let cli = Cli::new();
    cli.write_yaml("plugins: [\n");
    for verb in ["engines", "plugins", "whoami"] {
        let stderr = cli.fails(&[verb], 2);
        assert!(stderr.contains("config.yaml"), "{verb}: {stderr}");
    }
}

#[test]
fn config_flag_overrides_the_default_file() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  engine-codex: { enabled: false }\n");
    let other = cli.dir.path().join("other.yaml");
    std::fs::write(&other, "plugins:\n  engine-claude: { enabled: false }\n").unwrap();
    assert_eq!(
        cli.ok(&["--config", other.to_str().unwrap(), "engines"]),
        "codex\n"
    );
    // The variable stands for the flag, and the flag beats it.
    let run = |args: &[&str]| {
        let output = cli
            .command(args)
            .env("TX_CONFIG_FILE", &other)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    assert_eq!(run(&["engines"]), "codex\n");
    let default = cli.state().join("config.yaml");
    assert_eq!(
        run(&["--config", default.to_str().unwrap(), "engines"]),
        "claude\n"
    );
}

#[test]
fn unknown_verb_still_exits_2() {
    let cli = Cli::new();
    let stderr = cli.fails(&["frobnicate"], 2);
    assert!(stderr.contains("unknown verb \"frobnicate\""), "{stderr}");
}

#[test]
fn help_lists_plugin_verbs() {
    let cli = Cli::new();
    let help = cli.ok(&["help"]);
    for verb in ["pick", "_edit-tag", "kill", "whoami", "ui"] {
        assert!(lists_verb(&help, verb), "{verb} missing from help:\n{help}");
    }
}

#[test]
fn disabled_ide_hides_verbs() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  ide: { enabled: false }\n");
    cli.fails(&["ui", "attach"], 2);
    let listing = cli.ok(&["plugins"]);
    let rows = rows(&listing);
    position(&rows, "ide\tdisabled");
    position(&rows, "kill\tactive");
}

#[test]
fn a_directory_with_a_manifest_and_a_program_is_a_plugin() {
    let cli = Cli::new();
    let directory = hello(&cli);
    cli.write_yaml("plugins:\n  hello:\n    settings: { greeting: hi }\n");
    assert_eq!(
        cli.ok(&["hello", "a", "b"]),
        format!(
            "args: a b\nid: hello\nsettings: {{\"greeting\":\"hi\"}}\ndata: {}\ntx: {}\n",
            cli.state().display(),
            env!("CARGO_BIN_EXE_tx")
        )
    );
    // The exit code and the standard input are the program's own: `tx` became it.
    cli.fails(&["hello", "fail"], 7);
    let mut child = cli
        .command(&["hello", "cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), b"piped\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&output.stdout).ends_with("\npiped\n"),
        "{output:?}"
    );

    let help = cli.ok(&["help"]);
    assert!(
        help.contains("\n  hello [WORDS]   print the words and where I run\n"),
        "{help}"
    );
    let listing = cli.ok(&["plugins"]);
    position(&rows(&listing), "hello\tactive");
    assert_eq!(
        cli.json_row("hello"),
        serde_json::json!({
            "id": "hello", "status": "active", "detail": null, "origin": "user",
            "directory": directory, "description": "Says hello.", "tier": "extra",
            "requires": [], "needs": [], "verbs": ["hello"], "engine": null,
        })
    );
}

#[test]
fn a_plugin_program_is_told_which_core_and_which_plugin_it_runs_for() {
    let cli = Cli::new();
    let directory = write_plugin(
        cli.plugins_dir(),
        "probe",
        &manifest("probe").replace(
            "    bin: probe\n",
            "    bin: probe\n    args: [first, second]\n",
        ),
        &[("probe", "echo \"args: $*\"; env | grep '^TX_' | sort")],
    );
    let other = cli.dir.path().join("other.yaml");
    std::fs::write(&other, "plugins:\n  probe:\n    settings: [1, 2]\n").unwrap();
    let state = cli.state();
    let output = cli.ok(&[
        "--config",
        other.to_str().unwrap(),
        "--env",
        "A=1",
        "--env",
        "B=x y",
        "--profile-dir",
        "/roles",
        "--skill-dir",
        "/skills/a",
        "--skill-dir",
        "/skills/b",
        "probe",
        "third",
    ]);
    let expected = [
        "args: first second third".to_owned(),
        "TX_APPNAME=tx-ide".to_owned(),
        format!("TX_BIN={}", env!("CARGO_BIN_EXE_tx")),
        format!("TX_CACHE_DIR={}", state.display()),
        format!("TX_CONFIG_DIR={}", state.display()),
        format!("TX_CONFIG_FILE={}", other.display()),
        format!("TX_DATA_DIR={}", state.display()),
        "TX_PLUGIN_API=1".to_owned(),
        format!(
            "TX_PLUGIN_DATA_DIR={}",
            state.join("plugins/probe").display()
        ),
        format!("TX_PLUGIN_DIR={}", directory.display()),
        "TX_PLUGIN_ID=probe".to_owned(),
        format!(
            "TX_PLUGIN_PATH={}:{}",
            common::shipped().display(),
            cli.plugins_dir().display()
        ),
        "TX_PLUGIN_SETTINGS=[1,2]".to_owned(),
        "TX_PROFILE_PATH=/roles".to_owned(),
        format!("TX_RUN_DIR={}", state.display()),
        "TX_SESSION_ENV={\"A\":\"1\",\"B\":\"x y\"}".to_owned(),
        "TX_SKILL_PATH=/skills/a:/skills/b".to_owned(),
        format!("TX_STATE_DIR={}", state.display()),
        format!("TX_TMUX_SOCKET={}", state.join("tmux.sock").display()),
    ];
    let lines: Vec<&str> = output
        .lines()
        .filter(|line| {
            // What the test process itself may carry when it runs inside a tx session.
            !line.starts_with("TX_SESSION_ID=") && !line.starts_with("TX_PLUGIN_TRACE=")
        })
        .collect();
    assert_eq!(lines, expected, "{output}");
}

/// With its variables a plugin program runs `tx` again on the same core, plugins included.
#[test]
fn a_plugin_runs_tx_again_through_tx_bin_and_finds_the_same_core() {
    let cli = Cli::new();
    write_plugin(
        cli.plugins_dir(),
        "again",
        &manifest("again"),
        &[("again", "exec \"$TX_BIN\" plugins")],
    );
    hello(&cli);
    cli.write_yaml("plugins:\n  hello: { enabled: false }\n");
    let listing = cli.ok(&["again"]);
    let rows = rows(&listing);
    position(&rows, "again\tactive");
    position(&rows, "hello\tdisabled");
    position(&rows, "kill\tactive");
    // The variable alone names the roots as the flags do.
    let output = Command::new(env!("CARGO_BIN_EXE_tx"))
        .args(common::directory_flags(&cli.state()))
        .arg("plugins")
        .env("TX_PLUGIN_PATH", cli.plugins_dir())
        .env_remove("TMUX")
        .env_remove("TX_SESSION_ID")
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        listing.starts_with("again\tactive\nhello\tdisabled\n"),
        "{listing}"
    );
}

#[test]
fn a_disabled_installed_plugin_has_no_verb() {
    let cli = Cli::new();
    hello(&cli);
    cli.write_yaml("plugins:\n  hello: { enabled: false }\n");
    let stderr = cli.fails(&["hello"], 2);
    assert!(stderr.contains("unknown verb \"hello\""), "{stderr}");
    assert!(!lists_verb(&cli.ok(&["help"]), "hello"));
    position(&rows(&cli.ok(&["plugins"])), "hello\tdisabled");
    assert_eq!(cli.json_row("hello")["status"], "disabled");
}

#[test]
fn the_index_is_kept_and_follows_the_plugin_directory() {
    let cli = Cli::new();
    let directory = hello(&cli);
    let file = directory.join("plugin.yaml");
    let index = cli.state().join("plugins-index.json");
    assert!(!index.exists());
    cli.ok(&["hello"]);
    assert!(index.is_file(), "kept after the first run");

    // An edit of the same size shows on the next run.
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, text.replace("print the words", "PRINT THE WORDS")).unwrap();
    assert!(
        cli.ok(&["help"])
            .contains("\n  hello [WORDS]   PRINT THE WORDS and where I run\n")
    );

    std::fs::write(&file, text.replace("api: 1", "api: 2")).unwrap();
    position(
        &rows(&cli.ok(&["plugins"])),
        "hello\terror\tapi 2 is not supported (this tx: 1)",
    );
    let stderr = cli.fails(&["hello"], 2);
    assert!(stderr.contains("unknown verb \"hello\""), "{stderr}");

    std::fs::write(&file, format!("{text}colour: red\n")).unwrap();
    let listing = cli.ok(&["plugins"]);
    let row = listing
        .lines()
        .find(|row| row.starts_with("hello\t"))
        .unwrap();
    assert!(
        row.starts_with("hello\terror\tline 8: unknown field `colour`"),
        "{row}"
    );
    let json = cli.json_row("hello");
    assert_eq!(json["status"], "error");
    assert_eq!(json["origin"], "user");
    assert_eq!(json["description"], serde_json::Value::Null);
    assert!(
        json["detail"].as_str().unwrap().contains("colour"),
        "{json}"
    );

    std::fs::write(&file, &text).unwrap();
    cli.ok(&["hello"]);
    std::fs::remove_dir_all(&directory).unwrap();
    let stderr = cli.fails(&["hello"], 2);
    assert!(stderr.contains("unknown verb \"hello\""), "{stderr}");
    assert!(!cli.ok(&["plugins"]).contains("hello"));
}

/// A plugin written into a directory that existed before its manifest did.
#[test]
fn a_manifest_added_to_a_directory_already_there_is_found() {
    let cli = Cli::new();
    let directory = cli.plugins_dir().join("hello");
    std::fs::create_dir_all(directory.join("bin")).unwrap();
    cli.fails(&["hello"], 2);
    cli.ok(&["plugins"]);
    hello(&cli);
    assert!(cli.ok(&["hello"]).starts_with("args: \n"));
}

#[test]
fn a_plugin_takes_neither_a_built_in_verb_nor_another_plugins() {
    let cli = Cli::new();
    hello(&cli);
    let second = format!(
        "{}  - name: create\n    usage: \"create\"\n    bin: hello2\n",
        manifest("hello2").replace("  - name: hello2\n", "  - name: hello\n")
    );
    write_plugin(
        cli.plugins_dir(),
        "hello2",
        &second,
        &[("hello2", "echo the second plugin ran")],
    );
    assert!(cli.ok(&["hello", "x"]).starts_with("args: x\nid: hello\n"));
    // `create` is still the built-in: it asks for its own arguments.
    let stderr = cli.fails(&["create"], 2);
    assert!(stderr.contains("create needs a session name"), "{stderr}");
    let row = cli.json_row("hello2");
    assert_eq!(row["status"], "active", "it only lost the two verbs");
    assert_eq!(
        row["detail"],
        "verb \"hello\" is already \"hello\"'s; verb \"create\" is a built-in"
    );
    assert_eq!(row["verbs"], serde_json::json!(["hello", "create"]));
    position(
        &rows(&cli.ok(&["plugins"])),
        "hello2\tactive\tverb \"hello\" is already \"hello\"'s; verb \"create\" is a built-in",
    );
    assert_eq!(cli.json_row("hello")["detail"], serde_json::Value::Null);
}

#[test]
fn the_same_id_in_a_later_root_is_in_error_and_the_first_one_runs() {
    let cli = Cli::with_two_roots();
    let first = write_plugin(
        &cli.plugin_dirs[0],
        "hello",
        &manifest("hello"),
        &[("hello", "echo the first root")],
    );
    let second = write_plugin(
        &cli.plugin_dirs[1],
        "hello",
        &manifest("hello"),
        &[("hello", "echo the second root")],
    );
    assert_eq!(cli.ok(&["hello"]), "the first root\n");
    let listing = cli.ok(&["plugins"]);
    assert!(
        listing.starts_with(&format!(
            "hello\tactive\nhello\terror\talso installed in {}\n",
            first.display()
        )),
        "{listing}"
    );
    assert_eq!(cli.ok(&["help"]).matches("\n  hello [WORDS]").count(), 1);
    let rows: serde_json::Value = serde_json::from_str(&cli.ok(&["plugins", "--json"])).unwrap();
    let hellos: Vec<&serde_json::Value> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == "hello")
        .collect();
    assert_eq!(hellos.len(), 2);
    assert_eq!(hellos[0]["origin"], "shipped", "the first root's");
    assert_eq!(hellos[0]["status"], "active");
    assert_eq!(hellos[1]["origin"], "user");
    assert_eq!(hellos[1]["status"], "error");
    assert_eq!(hellos[1]["directory"], second.to_str().unwrap());
    // Plugin order is root order, then id: the later root's `alpha` comes after `hello`.
    write_plugin(&cli.plugin_dirs[1], "alpha", &manifest("alpha"), &[]);
    assert!(
        cli.ok(&["plugins"])
            .starts_with("hello\tactive\nalpha\tactive\nhello\terror\t")
    );
}

#[test]
fn a_program_that_cannot_run_is_named_and_exits_1() {
    let cli = Cli::new();
    let directory = write_plugin(cli.plugins_dir(), "hello", &manifest("hello"), &[]);
    assert_eq!(
        cli.fails(&["hello", "x"], 1),
        format!(
            "tx: hello: cannot run {}: No such file or directory (os error 2)\n",
            directory.join("bin/hello").display()
        )
    );
}

#[test]
fn a_traced_run_appends_one_line_per_plugin_program_started() {
    let cli = Cli::new();
    let directory = hello(&cli);
    let trace = cli.dir.path().join("trace");
    let traced = |args: &[&str]| {
        let output = cli
            .command(args)
            .env("TX_PLUGIN_TRACE", &trace)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };
    traced(&["hello", "a"]);
    let line = format!("verb\thello\t{}\n", directory.join("bin/hello").display());
    assert_eq!(std::fs::read_to_string(&trace).unwrap(), line);
    // A compiled-in verb and a built-in start no plugin program.
    traced(&["layout", "ls"]);
    traced(&["plugins"]);
    traced(&["hello"]);
    assert_eq!(std::fs::read_to_string(&trace).unwrap(), line.repeat(2));
}

/// A core that refuses to open has created nothing, the index included.
#[test]
fn a_command_the_core_refuses_keeps_no_index() {
    let cli = Cli::with_two_roots();
    write_plugin(
        &cli.plugin_dirs[1],
        "hello",
        &manifest("hello"),
        &[("hello", "echo hello")],
    );
    // A later flag wins: the socket of this run directory cannot fit a socket address.
    let run = cli.dir.path().join("r".repeat(120));
    let stderr = cli.fails(&["--run-dir", run.to_str().unwrap(), "engines"], 1);
    assert!(stderr.contains("tmux socket path is"), "{stderr}");
    assert!(!cli.state().exists(), "the refused command left files");
    cli.ok(&["engines"]);
    assert!(cli.state().join("plugins-index.json").is_file());
}

/// Looking a verb up never opens the core: help and misuse create nothing, and a plugin verb
/// leaves only the index behind.
#[test]
fn help_and_an_unknown_verb_create_nothing_and_a_plugin_verb_only_the_index() {
    let cli = Cli::with_two_roots();
    write_plugin(
        &cli.plugin_dirs[1],
        "hello",
        &manifest("hello"),
        &[("hello", "echo hello")],
    );
    assert!(lists_verb(&cli.ok(&["help"]), "hello"));
    cli.fails(&["frobnicate"], 2);
    cli.fails(&["--plugin-dir"], 2);
    assert!(
        !cli.state().exists(),
        "help or misuse created the directories"
    );
    assert_eq!(cli.ok(&["hello"]), "hello\n");
    let created: Vec<String> = std::fs::read_dir(cli.state())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(created, ["plugins-index.json"]);
}
