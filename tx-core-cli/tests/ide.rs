//! The ide plugin through the binary: its updater (`tx update`) and `ui attach`.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Cli;

impl Cli {
    fn conf(&self) -> PathBuf {
        self.state().join("tmux.conf")
    }
    fn tmux_with_conf(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-S")
            .arg(self.sock())
            .arg("-f")
            .arg(self.conf())
            .args(args)
            .env_remove("TMUX")
            .output()
            .unwrap()
    }
}

fn exe() -> PathBuf {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_tx")).unwrap()
}

fn expected_conf(cli: &Cli) -> String {
    let state = cli.state();
    let state = state.display();
    format!(
        "# tx-core server config, written by `tx update`. Re-run to update.\n\
         set -s exit-empty on\n\
         set -g history-limit 50000\n\
         set -g detach-on-destroy on\n\
         set -g @tx-bin \"{exe}\"\n\
         set -g @tx-data \"{state}\"\n\
         set -g @tx-arguments \"{arguments}\"\n\
         set-environment -g TX_BIN \"{exe}\"\n\
         source-file \"{state}/conf/tmux.conf\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/ide.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/artifacts.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/assistant.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/chat.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/edit.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/graph.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/home.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/kill.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/labels.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/picker.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/spawn.tmux\"\n\
         run-shell \"/usr/bin/env bash {state}/keys/timeline.tmux\"\n",
        exe = exe().display(),
        arguments = cli.arguments(),
    )
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o111 != 0
}

fn installed(cli: &Cli) -> String {
    std::fs::read_to_string(cli.state().join("keys/.installed")).unwrap()
}

/// Starts the server from the installed conf and waits until every key fragment has run.
fn start_server(cli: &Cli) {
    assert!(
        cli.tmux_with_conf(&["new-session", "-d", "-s", "probe", "sleep", "60"])
            .status
            .success()
    );
    assert!(
        cli.wait_for(5, || {
            cli.tmux_out(&["list-keys", "-T", "prefix", "t"])
                .contains("pick")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "X"])
                    .contains("kill")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "e"])
                    .contains("edit")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "s"])
                    .contains("_relabel")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "n"])
                    .contains("tmux-new-agent")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "a"])
                    .contains("tmux-artifact-menu")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "T"])
                    .contains("_timeline-attach")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "m"])
                    .contains("_home-open")
                && cli
                    .tmux_out(&["show-option", "-gv", "pane-border-format"])
                    .contains("_pane-label")
        }),
        "fragments never loaded"
    );
}

/// What `tx update` prints when no update step is pending and only `ide` prints: the ide
/// updater's lines, each prefixed with the plugin id.
fn updated(ide_lines: &[&str]) -> String {
    let mut out = format!(
        "update steps are at {}: nothing pending\n",
        tx_core::updates::latest()
    );
    for line in ide_lines {
        out.push_str(&format!("ide: {line}\n"));
    }
    out
}

fn config_disabling(id: &str) -> String {
    format!("plugins:\n  {id}: {{ enabled: false }}\n")
}

/// The one row of `ide` in `tx plugins --json`.
fn ide_row(cli: &Cli) -> serde_json::Value {
    let rows: serde_json::Value = serde_json::from_str(&cli.ok(&["plugins", "--json"])).unwrap();
    let found: Vec<&serde_json::Value> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == "ide")
        .collect();
    assert_eq!(found.len(), 1, "{rows}");
    found[0].clone()
}

/// What `tx help` says of the plugin's verbs, in its order (by name): the same lines whichever
/// way the plugin runs.
const HELP_LINES: [&str; 2] = [
    "  _resource NAME   print a plugin resource (internal)",
    "  ui attach | view <open|ls|close>   enter the IDE's server; manage views",
];

/// The lines of `tx help` that start with one of the plugin's verbs.
fn help_lines(cli: &Cli) -> Vec<String> {
    cli.ok(&["help"])
        .lines()
        .filter(|line| line.starts_with("  ui ") || line.starts_with("  _resource "))
        .map(str::to_owned)
        .collect()
}

/// `tx <args>` with the plugin programs it starts traced: its output, and the trace.
fn traced(cli: &Cli, args: &[&str]) -> (String, String) {
    let trace = cli.dir.path().join("trace");
    let _ = std::fs::remove_file(&trace);
    let output = cli.run(args, &[("TX_PLUGIN_TRACE", trace.to_str().unwrap())], "");
    assert!(
        output.status.success(),
        "tx {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        std::fs::read_to_string(&trace).unwrap_or_default(),
    )
}

#[test]
fn the_ide_bundle_is_the_shipped_plugin_and_its_verbs_and_updater_are_its_program() {
    let cli = Cli::new();
    let bundle = common::shipped().join("ide");
    let program = bundle.join("bin/tx-ide");
    let row = ide_row(&cli);
    assert_eq!(row["origin"], "shipped");
    assert_eq!(row["directory"], bundle.to_str().unwrap());
    assert_eq!(row["tier"], "ide");
    assert_eq!(row["requires"], serde_json::json!([]));
    assert_eq!(row["needs"], serde_json::json!(["tmux", "nvim"]));
    assert_eq!(row["verbs"], serde_json::json!(["ui", "_resource"]));
    assert_eq!(help_lines(&cli), HELP_LINES);
    // A verb is the program, once; so is the updater.
    let (listed, trace) = traced(&cli, &["ui", "view", "ls"]);
    assert_eq!(listed, "");
    assert_eq!(trace, format!("verb\tide\t{}\n", program.display()));
    let (out, trace) = traced(&cli, &["update"]);
    assert_eq!(out, updated(&[&cli.conf().display().to_string()]));
    assert_eq!(trace, format!("update\tide\t{}\n", program.display()));
    assert_eq!(
        std::fs::read_to_string(cli.conf()).unwrap(),
        expected_conf(&cli),
        "the program names the tx binary, not itself"
    );
}

/// Without the bundle the plugin runs compiled into `tx`, complete.
#[test]
fn without_the_bundle_ide_runs_compiled_into_tx() {
    let cli = Cli::compiled_in();
    assert_eq!(ide_row(&cli)["origin"], "built-in");
    assert_eq!(help_lines(&cli), HELP_LINES);
    let (listed, trace) = traced(&cli, &["ui", "view", "ls"]);
    assert_eq!(listed, "");
    assert_eq!(trace, "", "no plugin program was started");
    let (out, trace) = traced(&cli, &["update"]);
    assert_eq!(out, updated(&[&cli.conf().display().to_string()]));
    assert_eq!(trace, "", "the updater ran in tx");
    let conf = std::fs::read_to_string(cli.conf()).unwrap();
    assert!(
        conf.contains(&format!("set -g @tx-bin \"{}\"\n", exe().display())),
        "{conf}"
    );
    assert_eq!(
        cli.ok(&["_resource", "keys/ide.tmux"]).as_bytes(),
        include_bytes!("../../plugins/ide/keys/ide.tmux")
    );
}

#[test]
fn update_materialises_resources() {
    let cli = Cli::new();
    let out = cli.ok(&["update"]);
    assert_eq!(out, updated(&[&cli.conf().display().to_string()]));
    let state = cli.state();
    for script in [
        "tmux-kill-tx-session",
        "tmux-edit-tx-session",
        "tmux-nav",
        "tmux-system-resources",
        "tmux-new-agent",
        "tmux-artifact-menu",
    ] {
        let path = state.join("bin").join(script);
        assert!(path.is_file(), "{path:?}");
        assert!(is_executable(&path), "{path:?}");
    }
    for id in [
        "ide",
        "picker",
        "edit",
        "kill",
        "labels",
        "spawn",
        "artifacts",
        "timeline",
        "graph",
        "home",
    ] {
        let path = state.join(format!("keys/{id}.tmux"));
        assert!(path.is_file(), "{path:?}");
        assert!(is_executable(&path), "{path:?}");
    }
    assert!(state.join("conf/tmux.conf").is_file());
    for role in [
        "roles/COMMON.md",
        "definitions/agents/developer.md",
        "definitions/agents/layout-author.md",
        "roles/TX-ASSISTANT.md",
        "definitions/skills/tx-sessions/SKILL.md",
        "definitions/mcp.json",
    ] {
        let path = state.join(role);
        assert!(path.is_file(), "{path:?}");
        assert!(!is_executable(&path), "{path:?}");
    }
    assert_eq!(
        std::fs::read_to_string(cli.conf()).unwrap(),
        expected_conf(&cli)
    );
    let installed = installed(&cli);
    for (id, keys) in [
        ("labels", "keys=s"),
        ("kill", "keys=X"),
        ("picker", "keys=t"),
        ("edit", "keys=e"),
        ("spawn", "keys=n"),
        ("artifacts", "keys=a"),
        ("timeline", "keys=T"),
        ("graph", "keys=g"),
        ("home", "keys=m"),
    ] {
        assert!(
            installed
                .lines()
                .any(|line| line.starts_with(&format!("{id}\t{keys}"))),
            "{id}: {installed:?}"
        );
    }
    assert!(installed.lines().any(|line| line.starts_with("ide\t")));
}

#[test]
fn update_deletes_files_no_plugin_provides() {
    let cli = Cli::new();
    cli.ok(&["update"]);
    let stale = [
        cli.state().join("bin/old-script"),
        cli.state().join("keys/old.tmux"),
        cli.state().join("conf/old.conf"),
        cli.state().join("roles/OLD.md"),
    ];
    for path in &stale {
        std::fs::write(path, "x").unwrap();
    }
    cli.ok(&["update"]);
    for path in &stale {
        assert!(!path.exists(), "{path:?}");
    }
    assert!(cli.state().join("keys/.installed").is_file());
}

#[test]
fn prune_removes_a_symlink_but_never_follows_it() {
    let cli = Cli::new();
    cli.ok(&["update"]);
    let victim = cli.dir.path().join("victim-dir");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("precious"), "x").unwrap();
    let link = cli.state().join("bin/link");
    std::os::unix::fs::symlink(&victim, &link).unwrap();
    cli.ok(&["update"]);
    assert!(victim.join("precious").is_file());
    assert!(link.symlink_metadata().is_err(), "link is gone");
}

/// `tx update` itself refuses, before a byte is written and before the updaters: none of them
/// runs on resources that were not published.
#[test]
fn a_symlinked_kind_directory_is_refused() {
    let cli = Cli::new();
    cli.ok(&["update"]);
    std::fs::remove_file(cli.conf()).unwrap();
    let victim = cli.dir.path().join("victim-dir");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("precious"), "x").unwrap();
    let keys = cli.state().join("keys");
    std::fs::remove_dir_all(&keys).unwrap();
    std::os::unix::fs::symlink(&victim, &keys).unwrap();
    let output = cli.run(&["update"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "tx: update: {} is a symlink; refusing to prune\n",
            keys.display()
        )
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!(
            "update steps are at {}: nothing pending\n",
            tx_core::updates::latest()
        ),
        "no updater ran"
    );
    // Opening the core wrote its default config: not the IDE's.
    let conf = std::fs::read_to_string(cli.conf()).unwrap_or_default();
    assert!(!conf.contains("@tx-bin"), "{conf:?}");
    assert!(victim.join("precious").is_file());
    assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);
}

#[test]
fn ui_install_is_gone() {
    let cli = Cli::new();
    assert!(
        cli.fails(&["ui", "install"], 2)
            .starts_with("usage: tx ui attach | view")
    );
    cli.fails(&["update", "x"], 2);
    let conf = std::fs::read_to_string(cli.conf()).unwrap_or_default();
    assert!(!conf.contains("@tx-bin"), "{conf:?}");
}

#[test]
fn update_fragment_loads() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["update"]);
    start_server(&cli);
    let bound = |table: &str, key: &str| cli.tmux_out(&["list-keys", "-T", table, key]);
    assert!(bound("prefix", "t").contains("pick"));
    assert!(bound("prefix", "X").contains("tmux-kill-tx-session"));
    assert!(bound("prefix", "e").contains("tmux-edit-tx-session"));
    assert!(bound("prefix", "s").contains("_relabel"));
    assert!(bound("root", "C-h").contains("tmux-nav"));
    let format = cli.tmux_out(&["show-option", "-gv", "pane-border-format"]);
    assert!(format.contains("_pane-label"), "{format:?}");
    assert!(
        format.contains("/tx-labels "),
        "the label runs the plugin's program directly: {format:?}"
    );
    assert!(
        cli.tmux_out(&["show-option", "-gv", "status-right"])
            .contains("tmux-system-resources")
    );
    assert_eq!(
        cli.tmux_out(&["show-option", "-gv", "@tx-bin"]),
        exe().to_str().unwrap()
    );
    assert_eq!(
        cli.tmux_out(&["show-option", "-gv", "@tx-data"]),
        cli.state().to_str().unwrap()
    );
    assert_eq!(
        cli.tmux_out(&["show-option", "-gv", "@tx-arguments"]),
        cli.arguments()
    );
    // What a plugin program started by a key, a popup or a pane runs `tx` with.
    assert_eq!(
        cli.tmux_out(&["show-environment", "-g", "TX_BIN"]),
        format!("TX_BIN={}", exe().display())
    );
    assert!(bound("prefix", "n").contains("tmux-new-agent"));
    assert!(bound("prefix", "a").contains("tmux-artifact-menu"));
    assert!(bound("prefix", "T").contains("_timeline-attach"));
    assert!(bound("prefix", "m").contains("_home-open"));
    cli.tmux(&["kill-server"]);
}

#[test]
fn fragments_do_nothing_outside_an_installed_server() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["update"]);
    let other = cli.outer_sock();
    let on_other = |args: &[&str]| {
        Command::new("tmux")
            .arg("-S")
            .arg(&other)
            .args(["-f", "/dev/null"])
            .args(args)
            .env_remove("TMUX")
            .output()
            .unwrap()
    };
    assert!(
        on_other(&["new-session", "-d", "-s", "other", "sleep", "60"])
            .status
            .success()
    );
    for id in [
        "ide",
        "labels",
        "kill",
        "picker",
        "edit",
        "spawn",
        "artifacts",
        "timeline",
        "graph",
        "home",
    ] {
        let output = Command::new(cli.state().join(format!("keys/{id}.tmux")))
            .env("TMUX", format!("{},1,0", other.display()))
            .output()
            .unwrap();
        assert!(output.status.success(), "{id}: {output:?}");
    }
    let bound = on_other(&["list-keys", "-T", "prefix", "X"]);
    let root = on_other(&["list-keys", "-T", "root", "C-h"]);
    on_other(&["kill-server"]);
    assert!(bound.stdout.is_empty(), "{bound:?}");
    assert!(root.stdout.is_empty(), "{root:?}");
}

#[test]
fn disabling_kill_removes_its_key_on_reload() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["update"]);
    start_server(&cli);
    cli.write_yaml(&config_disabling("kill"));
    let out = cli.ok(&["update"]);
    assert_eq!(
        out,
        updated(&[&cli.conf().display().to_string(), "reloaded"])
    );
    let kill = cli.tmux_out(&["list-keys", "-T", "prefix", "X"]);
    assert!(!kill.contains("tmux-kill-tx-session"), "{kill:?}");
    assert!(!cli.state().join("keys/kill.tmux").exists());
    assert!(!cli.state().join("bin/tmux-kill-tx-session").exists());
    assert!(
        !installed(&cli)
            .lines()
            .any(|line| line.starts_with("kill\t"))
    );
    assert!(
        !std::fs::read_to_string(cli.conf())
            .unwrap()
            .contains("kill")
    );
    // The others survive the reload.
    assert!(cli.wait_for(5, || {
        cli.tmux_out(&["list-keys", "-T", "prefix", "t"])
            .contains("pick")
    }));
    assert!(
        cli.tmux_out(&["list-keys", "-T", "prefix", "e"])
            .contains("tmux-edit-tx-session")
    );
}

/// A key fragment an installed plugin publishes is run like a shipped one, and its key goes
/// when the plugin is disabled.
#[test]
fn an_installed_plugins_key_is_bound_and_unbound_on_reload() {
    require_tmux!();
    let cli = Cli::new();
    let hello = cli.plugin(
        "hello",
        "api: 1\nid: hello\ndescription: A key.\nresources:\n  - keys/hello.tmux\n",
        &[],
    );
    std::fs::create_dir_all(hello.join("keys")).unwrap();
    std::fs::write(
        hello.join("keys/hello.tmux"),
        "#!/usr/bin/env bash\n# keys: H\ntmux bind H display-message hello\n",
    )
    .unwrap();
    cli.ok(&["update"]);
    let line = format!(
        "run-shell \"/usr/bin/env bash {}/keys/hello.tmux\"\n",
        cli.state().display()
    );
    assert!(std::fs::read_to_string(cli.conf()).unwrap().contains(&line));
    assert!(installed(&cli).contains("hello\tkeys=H\t"));
    start_server(&cli);
    let bound = || cli.tmux_out(&["list-keys", "-T", "prefix", "H"]);
    assert!(
        cli.wait_for(5, || bound().contains("display-message hello")),
        "{:?}",
        bound()
    );

    cli.write_yaml(&config_disabling("hello"));
    let out = cli.ok(&["update"]);
    assert_eq!(
        out,
        updated(&[&cli.conf().display().to_string(), "reloaded"])
    );
    assert!(!bound().contains("display-message hello"), "{:?}", bound());
    assert!(!cli.state().join("keys/hello.tmux").exists());
    assert!(
        !std::fs::read_to_string(cli.conf())
            .unwrap()
            .contains("hello")
    );
    assert!(!installed(&cli).contains("hello"));
}

#[test]
fn disabling_labels_unsets_border_format_on_reload() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["update"]);
    start_server(&cli);
    assert!(
        cli.tmux_out(&["show-hooks", "-g", "after-new-window"])
            .contains("@tx-view")
    );
    cli.write_yaml(&config_disabling("labels"));
    let out = cli.ok(&["update"]);
    assert!(out.contains("reloaded"), "{out:?}");
    assert!(!out.contains("legacy"), "{out:?}");
    let format = cli.tmux_out(&["show-option", "-gv", "pane-border-format"]);
    assert!(!format.contains("_pane-label"), "{format:?}");
    // An unset hook is listed by name only, with no command behind it.
    let hook = cli.tmux_out(&["show-hooks", "-g", "after-new-window"]);
    assert!(!hook.contains("@tx-view"), "{hook:?}");
    let relabel = cli.tmux_out(&["list-keys", "-T", "prefix", "s"]);
    assert!(!relabel.contains("_relabel"), "{relabel:?}");
    // The border look stays: it belongs to ide.
    assert_eq!(
        cli.tmux_out(&["show-option", "-gv", "pane-border-lines"]),
        "heavy"
    );
}

/// What a retire prints first, and what it adds when a server runs.
const RESET: &str = "tmux.conf reset to the core default";
const RESTART_HINT: &str = "restart the server to drop the rest of the IDE's look";

/// Disabling `ide` retires it: its `update` program is told to take the config back.
#[test]
fn disabling_ide_gives_the_config_back_to_core() {
    let cli = Cli::new();
    cli.ok(&["update"]);
    assert!(cli.state().join("keys/.installed").is_file());

    cli.write_yaml(&config_disabling("ide"));
    let (out, trace) = traced(&cli, &["update"]);
    assert_eq!(out, updated(&[RESET]), "no server runs: nothing to restart");
    assert_eq!(
        trace,
        format!(
            "update\tide\t{}\n",
            common::shipped().join("ide/bin/tx-ide").display()
        ),
        "the retire is the plugin's update program"
    );
    assert_eq!(
        std::fs::read_to_string(cli.conf()).unwrap(),
        tx_core::core::DEFAULT_TMUX_CONF
    );
    assert!(!cli.state().join("keys/.installed").exists());
    // The ide's own resources went with the publish; the other plugins' are still there.
    assert!(!cli.state().join("conf/tmux.conf").exists());
    assert!(!cli.state().join("keys/ide.tmux").exists());
    assert!(cli.state().join("keys/kill.tmux").is_file());

    // Safe to repeat: nothing is left to take back, and a config edited since stays.
    std::fs::write(cli.conf(), "# my own\n").unwrap();
    assert_eq!(cli.ok(&["update"]), updated(&[]));
    assert_eq!(std::fs::read_to_string(cli.conf()).unwrap(), "# my own\n");

    std::fs::remove_file(cli.state().join("config.yaml")).unwrap();
    assert_eq!(
        cli.ok(&["update"]),
        updated(&[&cli.conf().display().to_string()])
    );
    assert_eq!(
        std::fs::read_to_string(cli.conf()).unwrap(),
        expected_conf(&cli)
    );
    assert!(cli.state().join("keys/.installed").is_file());
}

/// In a running server a retire unbinds every key, unsets every option and removes every hook
/// the record lists, whichever plugin's fragment declared it: no config runs the fragments any
/// more.
#[test]
fn disabling_ide_takes_the_keys_out_of_a_running_server_and_enabling_it_brings_them_back() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["update"]);
    start_server(&cli);
    let bound = |key: &str| cli.tmux_out(&["list-keys", "-T", "prefix", key]);

    cli.write_yaml(&config_disabling("ide"));
    assert_eq!(cli.ok(&["update"]), updated(&[RESET, RESTART_HINT]));
    assert_eq!(
        std::fs::read_to_string(cli.conf()).unwrap(),
        tx_core::core::DEFAULT_TMUX_CONF
    );
    assert!(!cli.state().join("keys/.installed").exists());
    for (key, command) in [
        ("t", "pick"),
        ("m", "_home-open"),
        ("X", "tmux-kill-tx-session"),
        ("e", "tmux-edit-tx-session"),
        ("s", "_relabel"),
    ] {
        assert!(!bound(key).contains(command), "{key}: {:?}", bound(key));
    }
    let format = cli.tmux_out(&["show-option", "-gv", "pane-border-format"]);
    assert!(!format.contains("_pane-label"), "{format:?}");
    let hook = cli.tmux_out(&["show-hooks", "-g", "after-new-window"]);
    assert!(!hook.contains("@tx-view"), "{hook:?}");
    // What the composed config's own first lines set went with the keys.
    for option in ["@tx-bin", "@tx-data", "@tx-arguments"] {
        assert_eq!(
            cli.tmux_out(&["show-option", "-gqv", option]),
            "",
            "{option}"
        );
    }
    let environment = cli.tmux_out(&["show-environment", "-g"]);
    assert!(
        !environment.lines().any(|line| line.starts_with("TX_BIN=")),
        "{environment}"
    );
    // The rest of the look is the server's until it restarts.
    assert_eq!(
        cli.tmux_out(&["show-option", "-gv", "pane-border-lines"]),
        "heavy"
    );
    assert_eq!(cli.ok(&["update"]), updated(&[]), "retired once");

    // The server is a plain core server again: no hint about bindings that were never recorded,
    // which would be false here (every recorded one was removed).
    std::fs::remove_file(cli.state().join("config.yaml")).unwrap();
    assert_eq!(
        cli.ok(&["update"]),
        updated(&[&cli.conf().display().to_string(), "reloaded"])
    );
    assert!(
        cli.tmux_out(&["show-option", "-gqv", "@tx-bin"])
            .ends_with("/tx"),
        "the config names the tx binary again"
    );
    assert_eq!(
        std::fs::read_to_string(cli.conf()).unwrap(),
        expected_conf(&cli)
    );
    assert!(
        cli.wait_for(5, || {
            bound("t").contains("pick")
                && bound("m").contains("_home-open")
                && bound("X").contains("tmux-kill-tx-session")
                && cli
                    .tmux_out(&["show-option", "-gv", "pane-border-format"])
                    .contains("_pane-label")
        }),
        "the keys never came back"
    );
}

/// `tx` on the default plugin roots of `cli`'s directories (`<data>/shipped`, then
/// `<config>/plugins`), with nothing of the session the tests run in: the shipped plugins are
/// then what `tx update --bundles` laid out.
fn shipped_tx(cli: &Cli, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tx"));
    command
        .args(common::directory_flags(&cli.state()))
        .args(args);
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
    command.output().unwrap()
}

/// A shipped `ide` that leaves the selection is retired before its directory is removed.
#[test]
fn an_ide_that_left_the_selection_is_retired_before_it_is_removed() {
    let cli = Cli::compiled_in();
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let bundles = workspace.join("plugins");
    let binaries = Path::new(env!("CARGO_BIN_EXE_tx")).parent().unwrap();
    let update = || {
        let output = shipped_tx(
            &cli,
            &[
                "update",
                "--bundles",
                bundles.to_str().unwrap(),
                "--binaries",
                binaries.to_str().unwrap(),
                "--link",
            ],
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    };
    let out = update();
    assert!(out.contains("shipped: ide\n"), "{out}");
    let shipped = cli.state().join("shipped");
    assert!(shipped.join("ide/plugin.yaml").exists());
    assert!(cli.state().join("keys/.installed").is_file());

    let others: Vec<String> = std::fs::read_dir(&bundles)
        .unwrap()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.path().join("plugin.yaml").is_file())
        .map(|entry| entry.file_name().into_string().unwrap())
        .filter(|id| id != "ide")
        .collect();
    std::fs::write(
        shipped.join(".selection"),
        serde_json::json!({ "preset": null, "plugins": others }).to_string(),
    )
    .unwrap();
    let out = update();
    assert!(
        out.starts_with(&format!("ide: {RESET}\nshipped: ide removed\n")),
        "{out}"
    );
    assert!(!shipped.join("ide").exists());
}

#[test]
fn legacy_hint_when_no_installed_file() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["update"]);
    start_server(&cli);
    let fresh = cli.ok(&["update"]);
    assert!(!fresh.contains("legacy"), "{fresh:?}");
    std::fs::remove_file(cli.state().join("keys/.installed")).unwrap();
    assert!(
        cli.tmux(&["set-option", "-g", "@tx-bin", exe().to_str().unwrap()])
            .status
            .success()
    );
    let out = cli.ok(&["update"]);
    assert_eq!(
        out,
        updated(&[
            &cli.conf().display().to_string(),
            "reloaded",
            "legacy bindings may remain; restart the server to drop them",
        ])
    );
    assert!(cli.state().join("keys/.installed").is_file());
}

#[test]
fn no_legacy_hint_when_the_server_never_loaded_an_install_conf() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["update"]);
    start_server(&cli);
    std::fs::remove_file(cli.state().join("keys/.installed")).unwrap();
    assert!(
        cli.tmux(&["set-option", "-gu", "@tx-bin"]).status.success(),
        "unset @tx-bin"
    );
    let out = cli.ok(&["update"]);
    assert_eq!(
        out,
        updated(&[&cli.conf().display().to_string(), "reloaded"])
    );
    assert!(cli.state().join("keys/.installed").is_file());
}

#[test]
fn reload_unbinds_a_key_a_fragment_stopped_declaring() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["update"]);
    start_server(&cli);
    assert!(
        cli.tmux(&["bind-key", "F12", "display-message", "bogus"])
            .status
            .success()
    );
    assert!(
        cli.tmux_out(&["list-keys", "-T", "prefix", "F12"])
            .contains("bogus")
    );
    let path = cli.state().join("keys/.installed");
    let text = installed(&cli);
    assert!(text.contains("kill\tkeys=X\t"), "{text:?}");
    std::fs::write(&path, text.replace("kill\tkeys=X\t", "kill\tkeys=X F12\t")).unwrap();
    let out = cli.ok(&["update"]);
    assert!(out.contains("reloaded"), "{out:?}");
    let f12 = cli.tmux_out(&["list-keys", "-T", "prefix", "F12"]);
    assert!(f12.is_empty(), "{f12:?}");
    assert!(
        installed(&cli).contains("kill\tkeys=X\t"),
        "{:?}",
        installed(&cli)
    );
    assert!(cli.wait_for(5, || {
        cli.tmux_out(&["list-keys", "-T", "prefix", "X"])
            .contains("tmux-kill-tx-session")
    }));
}

#[test]
fn no_reload_and_no_hint_without_a_server() {
    let cli = Cli::new();
    assert_eq!(
        cli.ok(&["update"]),
        updated(&[&cli.conf().display().to_string()])
    );
}

#[test]
fn resource_verb_prints_the_published_file_and_fails_on_unknown() {
    let cli = Cli::new();
    // Nothing is published before the first update.
    assert_eq!(
        cli.fails(&["_resource", "conf/tmux.conf"], 1),
        "tx _resource: no such resource: conf/tmux.conf\n"
    );
    cli.ok(&["update"]);
    let output = cli.run(&["_resource", "conf/tmux.conf"], &[], "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        include_bytes!("../../plugins/ide/conf/tmux.conf")
    );
    let output = cli.run(&["_resource", "bin/tmux-nav"], &[], "");
    assert!(output.stdout.starts_with(b"#!/usr/bin/env bash"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("@tx-ide-nav-keys"));
    // A role is published under `<data>/roles`; another plugin's resource is printed as well.
    assert_eq!(
        cli.ok(&["_resource", "agents/COMMON.md"]),
        std::fs::read_to_string(cli.state().join("roles/COMMON.md")).unwrap()
    );
    assert!(
        cli.ok(&["_resource", "keys/kill.tmux"])
            .contains("tmux-kill-tx-session")
    );
    // Not a resource's name, or one nothing published: the config next to `keys/` stays out of
    // reach.
    for name in ["nope/x", "keys/nope.tmux", "tmux.conf", "keys/../tmux.conf"] {
        let output = cli.run(&["_resource", name], &[], "");
        assert_eq!(output.status.code(), Some(1), "{name}");
        assert!(output.stdout.is_empty(), "{name}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            format!("tx _resource: no such resource: {name}\n")
        );
    }
    cli.fails(&["_resource"], 2);
    cli.write_yaml(&config_disabling("kill"));
    cli.ok(&["update"]);
    let stderr = cli.fails(&["_resource", "keys/kill.tmux"], 1);
    assert!(stderr.contains("no such resource"), "{stderr:?}");
}

#[test]
fn update_refuses_unsafe_paths() {
    let cli = Cli::new();
    let state = cli.dir.path().join("with space");
    let output = Command::new(env!("CARGO_BIN_EXE_tx"))
        .args(common::directory_flags(&state))
        .args(["update"])
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "tx: update: ide: path contains unsafe characters: {}\n",
            state.display()
        )
    );
    let conf = std::fs::read_to_string(state.join("tmux.conf")).unwrap_or_default();
    assert!(!conf.contains("@tx-bin"), "{conf:?}");
    // The resources are `tx update`'s own to publish, and no path of theirs goes into a config.
    assert!(state.join("roles/COMMON.md").is_file());
    assert!(!state.join("keys/.installed").exists());
}

/// The shell every `ui attach` of this suite runs its panes with: one that reads no start-up file,
/// so the runner's own shell (and the user's files behind it) never takes part. tmux reports it
/// as `SUITE_SHELL_NAME`.
const SUITE_SHELL: &str = "/bin/dash";
const SUITE_SHELL_NAME: &str = "dash";

fn attach_command(cli: &Cli) -> String {
    format!(
        "env SHELL={SUITE_SHELL} {} {} ui attach",
        env!("CARGO_BIN_EXE_tx"),
        cli.arguments()
    )
}

#[test]
fn ui_attach_lands_in_a_view_home() {
    require_tmux!();
    let cli = Cli::new();
    cli.write_yaml(&config_disabling("assistant"));
    cli.create("a");
    cli.in_outer_pane(&attach_command(&cli));
    assert!(
        cli.wait_for(5, || cli.tmux_out(&[
            "list-clients",
            "-F",
            "#{client_session}"
        ]) == "home"),
        "no client attached to home"
    );
    let option = |name: &str| cli.tmux_out(&["show-options", "-vt", "=home:", name]);
    assert_eq!(option("@tx-view"), "1");
    assert_eq!(option("@tx-view-kind"), "home");
    assert_eq!(option("status"), "on");
    assert_eq!(
        cli.tmux_out(&["show-window-options", "-vt", "=home:", "pane-border-status"]),
        "top"
    );
    assert_eq!(
        cli.tmux_out(&["display-message", "-p", "-t", "=home:", "#{window_name}"]),
        "home"
    );
}

/// The home pane's screen: the floor's TUI, or whatever runs there.
/// A tmux server keeps the environment of the call that starts it, and every pane inherits it.
/// `tx ui attach` is that call, and as a bundle it is the program `tx-ide`, which holds
/// everything `tx` gave it: the core's coordinates, the roots the harness names for this one
/// command (`TX_PROFILE_PATH`, `TX_PLUGIN_PATH`), its own `TX_PLUGIN_*`. None of that becomes
/// the server's: it holds what the caller of `tx` held, as when `tx` links `ide`, and the
/// `TX_BIN` the composed config sets.
#[test]
fn the_server_ui_attach_starts_holds_what_the_caller_of_tx_held() {
    require_tmux!();
    // What reaches `tx` from the suite's own environment (the harness drops `TX_SESSION_ID`),
    // one more variable this caller holds, and the config's `TX_BIN`.
    let mut expected: Vec<String> = std::env::vars()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("TX_") && name != "TX_SESSION_ID")
        .chain(["TX_APPNAME".to_owned(), "TX_BIN".to_owned()])
        .collect();
    expected.sort_unstable();
    expected.dedup();
    for (way, cli) in [("bundle", Cli::new()), ("compiled in", Cli::compiled_in())] {
        cli.write_yaml(&config_disabling("assistant"));
        cli.ok(&["update"]);
        cli.run(
            &["ui", "attach"],
            &[
                ("SHELL", SUITE_SHELL),
                ("TMUX", "/tmp/not-the-core.sock,1,0"),
                ("TX_APPNAME", "tx-ide"),
            ],
            "",
        );
        assert!(
            cli.tmux(&["has-session", "-t", "=home"]).status.success(),
            "{way}"
        );
        let mut held: Vec<String> = cli
            .tmux_out(&["show-environment", "-g"])
            .lines()
            .filter(|line| line.starts_with("TX_"))
            .map(|line| line.split('=').next().unwrap().to_owned())
            .collect();
        held.sort_unstable();
        assert_eq!(held, expected, "{way}");
    }
}

fn home_pane(cli: &Cli) -> String {
    cli.tmux_out(&["capture-pane", "-p", "-t", "=home:"])
}

fn home_tui_shown(cli: &Cli) -> bool {
    let screen = home_pane(cli);
    screen.contains("tx-ide") && screen.contains("enter attach")
}

#[test]
fn ui_attach_inside_another_tmux_says_its_prefix_wins() {
    require_tmux!();
    let cli = Cli::new();
    cli.write_yaml(&config_disabling("assistant"));
    let output = cli.run(
        &["ui", "attach"],
        &[
            ("SHELL", SUITE_SHELL),
            ("TMUX", "/tmp/not-the-core.sock,1,0"),
        ],
        "",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with(
            "tx ui attach: nesting inside another tmux (socket /tmp/not-the-core.sock); its prefix wins \u{2014} press the prefix twice to reach the IDE's keys\n"
        ),
        "{stderr:?}"
    );
    assert!(cli.tmux(&["has-session", "-t", "=home"]).status.success());
}

#[test]
fn ui_attach_inside_the_core_switches_the_client() {
    require_tmux!();
    let cli = Cli::new();
    cli.write_yaml(&config_disabling("assistant"));
    cli.create("other");
    cli.in_outer_pane(&format!(
        "TMUX= tmux -S {} attach -t =other",
        cli.sock().display()
    ));
    assert!(
        cli.wait_for(5, || cli.tmux_out(&[
            "list-clients",
            "-F",
            "#{client_session}"
        ]) == "other"),
        "client never attached"
    );
    let inside = format!("{},1,0", cli.sock().display());
    let output = cli.run(
        &["ui", "attach"],
        &[("SHELL", SUITE_SHELL), ("TMUX", &inside)],
        "",
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        cli.tmux_out(&["list-clients", "-F", "#{client_session}"]),
        "home"
    );
    assert_eq!(
        cli.tmux_out(&["show-options", "-vt", "=home:", "@tx-view"]),
        "1"
    );
}

#[test]
fn ui_attach_lands_on_the_home_tui_and_the_assistant_comes_up_behind_it() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["update"]);
    // The outer pane's tty drops a typed line over 1024 bytes, which a full PATH is: type a
    // short script path instead.
    let script = cli.dir.path().join("attach.sh");
    std::fs::write(
        &script,
        format!(
            "export PATH='{}' CLAUDE_CONFIG_DIR='{}'\nexec {}\n",
            cli.fake_claude_path(),
            cli.claude_config_dir().display(),
            attach_command(&cli)
        ),
    )
    .unwrap();
    cli.in_outer_pane(&format!("sh {}", script.display()));
    // `tx update` ran just before and wrote the key fragments as new executable files. The server
    // config runs each one through its interpreter: run directly, the first run of a new file is
    // assessed by macOS one file at a time for the whole machine (370 ms alone, seconds when many
    // run at once), the `home` session only appears after the twelve runs, and the attach would
    // wait for it for 20 to 30 s while the other suites write their own fragments.
    assert!(
        cli.wait_for(15, || cli.tmux_out(&[
            "list-clients",
            "-F",
            "#{client_session}"
        ]) == "home"),
        "no client attached to home"
    );
    assert!(
        cli.wait_for(10, || home_tui_shown(&cli)),
        "the home pane does not show the TUI:\n{}",
        home_pane(&cli)
    );
    // Starting the assistant (a repository, its worktree, the agent) can take over ten seconds
    // while the other suites load the machine; the TUI starts it and the header follows. The
    // fake claude never reports a turn, so the record stays `starting`.
    let assistant_row = || {
        let screen = home_pane(&cli);
        screen.contains("tx-assistant")
            && (screen.contains("assistant ◐ starting") || screen.contains("assistant ● ready"))
    };
    assert!(
        cli.wait_for(30, assistant_row),
        "the header never showed the assistant coming up:\n{}",
        home_pane(&cli)
    );
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["show", "tx-assistant"])).unwrap();
    assert_ne!(json["state"], "ended");
    assert_eq!(json["tags"], serde_json::json!(["tx-system"]));
    let cwd = std::path::PathBuf::from(json["working_directory"].as_str().unwrap());
    assert!(cwd.starts_with(cli.worktrees_dir()), "{cwd:?}");
    assert!(cwd.ends_with("home--tx-assistant"), "{cwd:?}");
    assert_eq!(json["roles"], serde_json::json!(["COMMON", "TX-ASSISTANT"]));
    assert_eq!(cli.ok(&["ls", "--tag", "tx-system"]).lines().count(), 1);
}

#[test]
fn ui_attach_without_the_roles_installed_shows_the_failed_assistant() {
    require_tmux!();
    let cli = Cli::new();
    let path = cli.fake_claude_path();
    let output = cli.run(
        &["ui", "attach"],
        &[
            ("PATH", &path),
            ("SHELL", SUITE_SHELL),
            ("TMUX", "/tmp/not-the-core.sock,1,0"),
        ],
        "",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("nesting inside another tmux"), "{stderr:?}");
    assert!(cli.tmux(&["has-session", "-t", "=home"]).status.success());
    assert!(
        cli.wait_for(20, || home_pane(&cli).contains("assistant ✗ failed")),
        "the header never reported the failed warm:\n{}",
        home_pane(&cli)
    );
    assert!(home_pane(&cli).contains("COMMON"), "{}", home_pane(&cli));
    assert_eq!(
        cli.run(&["show", "tx-assistant"], &[], "").status.code(),
        Some(1)
    );
    // `r` warms again: the roles are still missing, so it fails again and the home comes back.
    let pid = || cli.tmux_out(&["display-message", "-p", "-t", "=home:", "#{pane_pid}"]);
    let failed_pid = pid();
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(
        cli.tmux(&["send-keys", "-t", "=home:", "r"])
            .status
            .success()
    );
    assert!(
        cli.wait_for(20, || pid() != failed_pid
            && home_pane(&cli).contains("assistant ✗ failed")),
        "r did not warm again:\n{}",
        home_pane(&cli)
    );
    // With the roles installed, the next attach's install warms again and the failure clears.
    cli.ok(&["update"]);
    // tmux gives the respawned floor, and the assistant it warms, this command's PATH.
    let installed = cli.run(&["_home-install", "home"], &[("PATH", &path)], "");
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    assert!(
        cli.wait_for(30, || {
            let screen = home_pane(&cli);
            !screen.contains("failed")
                && (screen.contains("assistant ◐ starting") || screen.contains("assistant ● ready"))
        }),
        "the header never left failed:\n{}",
        home_pane(&cli)
    );
    assert_eq!(cli.ok(&["ls", "--tag", "tx-system"]).lines().count(), 1);
}

#[test]
fn ui_attach_installs_the_floor_on_an_existing_shell_home() {
    require_tmux!();
    let cli = Cli::new();
    cli.write_yaml(&config_disabling("assistant"));
    cli.ok(&["update"]);
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "home", "sh"])
            .status
            .success()
    );
    cli.create("other");
    cli.in_outer_pane(&format!(
        "TMUX= tmux -S {} attach -t =other",
        cli.sock().display()
    ));
    assert!(cli.wait_for(5, || cli.tmux_out(&[
        "list-clients",
        "-F",
        "#{client_session}"
    ]) == "other"));
    let inside = format!("{},1,0", cli.sock().display());
    let output = cli.run(
        &["ui", "attach"],
        &[("SHELL", SUITE_SHELL), ("TMUX", &inside)],
        "",
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        cli.wait_for(5, || home_tui_shown(&cli)),
        "the shell home did not get the floor:\n{}",
        home_pane(&cli)
    );
    let pane = cli.tmux_out(&["display-message", "-p", "-t", "=home:", "#{pane_id}"]);
    assert!(
        cli.tmux_out(&["show-hooks", "-p", "-t", &pane])
            .contains("_home-perform")
    );
}

#[test]
fn ui_attach_with_home_disabled_opens_a_shell() {
    require_tmux!();
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  assistant: { enabled: false }\n  home: { enabled: false }\n");
    cli.create("a");
    cli.in_outer_pane(&attach_command(&cli));
    assert!(
        cli.wait_for(5, || cli.tmux_out(&[
            "list-clients",
            "-F",
            "#{client_session}"
        ]) == "home"),
        "no client attached to home"
    );
    let shell = SUITE_SHELL_NAME;
    assert!(
        cli.wait_for(5, || cli.tmux_out(&[
            "display-message",
            "-p",
            "-t",
            "=home:",
            "#{pane_current_command}"
        ]) == shell),
        "{}",
        cli.tmux_out(&[
            "display-message",
            "-p",
            "-t",
            "=home:",
            "#{pane_current_command}"
        ])
    );
    let pane = cli.tmux_out(&["display-message", "-p", "-t", "=home:", "#{pane_id}"]);
    assert!(
        !cli.tmux_out(&["show-hooks", "-p", "-t", &pane])
            .contains("_home-perform")
    );
}

/// A home whose plugin was disabled after it held the floor: its pane keeps the hook of a verb
/// that is gone and dies with the TUI; the next attach takes the floor apart and gives the pane
/// its shell back.
#[test]
fn ui_attach_with_home_disabled_after_it_was_enabled_repairs_the_pane() {
    require_tmux!();
    let cli = Cli::new();
    cli.write_yaml(&config_disabling("assistant"));
    cli.in_outer_pane(&attach_command(&cli));
    assert!(
        cli.wait_for(10, || home_tui_shown(&cli)),
        "no home TUI:\n{}",
        home_pane(&cli)
    );
    let pane = cli.tmux_out(&["display-message", "-p", "-t", "=home:", "#{pane_id}"]);
    let pane_format = |format: &str| cli.tmux_out(&["display-message", "-p", "-t", &pane, format]);
    cli.write_yaml("plugins:\n  assistant: { enabled: false }\n  home: { enabled: false }\n");
    cli.ok(&["update"]);
    assert!(
        Command::new("kill")
            .arg(pane_format("#{pane_pid}"))
            .status()
            .unwrap()
            .success()
    );
    assert!(
        cli.wait_for(5, || pane_format("#{pane_dead}") == "1"),
        "the TUI did not die"
    );
    let inside = format!("{},1,0", cli.sock().display());
    let output = cli.run(
        &["ui", "attach"],
        &[("SHELL", SUITE_SHELL), ("TMUX", &inside)],
        "",
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shell = SUITE_SHELL_NAME;
    assert!(
        cli.wait_for(5, || pane_format("#{pane_dead}") == "0"
            && pane_format("#{pane_current_command}") == shell),
        "{} {}",
        pane_format("#{pane_dead}"),
        pane_format("#{pane_current_command}")
    );
    assert!(
        !cli.tmux_out(&["show-hooks", "-p", "-t", &pane])
            .contains("pane-died")
    );
    let option = |name: &str| cli.tmux_out(&["show-options", "-pv", "-t", &pane, name]);
    assert_eq!(option("remain-on-exit"), "off");
    assert_eq!(option("@tx-home-floor"), "");
    assert_eq!(option("remain-on-exit-format"), "");
}
