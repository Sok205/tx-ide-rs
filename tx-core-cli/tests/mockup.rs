//! `tx mockup`: the session dir, the `mockup-<agent>` view, switching and the viewer. The
//! plugin ships as a bundle, so here its verbs run as the program `tx-mockup`; a sandbox
//! without the bundle runs it compiled into `tx`, as before.

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{Cli, lists_verb};

fn project(cli: &Cli) -> PathBuf {
    let dir = cli.dir.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `tx mockup start --project <proj> --for a`, returning the printed JSON line.
fn start(cli: &Cli) -> serde_json::Value {
    let project = project(cli);
    let out = cli.ok(&[
        "mockup",
        "start",
        "--project",
        project.to_str().unwrap(),
        "--for",
        "a",
    ]);
    serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{e}: {out:?}"))
}

fn has_view(cli: &Cli) -> bool {
    cli.tmux(&["has-session", "-t", "=mockup-a"])
        .status
        .success()
}

#[test]
fn help_lists_mockup() {
    let cli = Cli::new();
    let help = cli.ok(&["help"]);
    assert!(lists_verb(&help, "mockup"), "{help}");
    assert!(lists_verb(&help, "_mockup-view"), "{help}");
    assert!(help.contains("mockup start --project DIR"), "{help}");
}

#[test]
fn start_creates_dirs_view_and_ext() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    let json = start(&cli);
    assert_eq!(json["view"], "mockup-a");
    let content_dir = Path::new(json["content_dir"].as_str().unwrap());
    let state_dir = Path::new(json["state_dir"].as_str().unwrap());
    let session_dir = project(&cli).join(".superpowers/mockup").join(&id);
    assert_eq!(content_dir, session_dir.join("content"));
    assert_eq!(state_dir, session_dir.join("state"));
    assert!(content_dir.is_dir());
    assert!(state_dir.is_dir());
    assert!(has_view(&cli), "view mockup-a missing");
    assert_eq!(
        cli.tmux_out(&["show-options", "-vt", "=mockup-a:", "@tx-view"]),
        "1"
    );
    assert_eq!(
        cli.tmux_out(&["show-options", "-vt", "=mockup-a:", "@tx-mockup-dir"]),
        session_dir.to_str().unwrap()
    );
    let shown: serde_json::Value = serde_json::from_str(&cli.ok(&["show", "a"])).unwrap();
    assert_eq!(shown["ext"]["mockup"]["dir"], session_dir.to_str().unwrap());
    // A second start reuses the view.
    start(&cli);
    assert_eq!(
        cli.tmux_out(&["list-sessions", "-F", "#{session_name}"])
            .lines()
            .filter(|name| *name == "mockup-a")
            .count(),
        1
    );
}

#[test]
fn show_without_client_prints_ready() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    start(&cli);
    let out = cli.ok(&["mockup", "show", "--for", "a"]);
    assert_eq!(out, "mockup ready in view mockup-a\n");
}

#[test]
fn show_before_start_exits_1() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    let stderr = cli.fails(&["mockup", "show", "--for", "a"], 1);
    assert!(
        stderr.contains("tx mockup: run 'tx mockup start' first"),
        "{stderr}"
    );
}

#[test]
fn show_recreates_dead_view() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    start(&cli);
    assert!(
        cli.tmux(&["kill-session", "-t", "=mockup-a"])
            .status
            .success()
    );
    assert!(!has_view(&cli));
    cli.ok(&["mockup", "show", "--for", "a"]);
    assert!(cli.wait_for(5, || has_view(&cli)), "view not recreated");
    assert_eq!(
        cli.tmux_out(&["show-options", "-vt", "=mockup-a:", "@tx-view"]),
        "1"
    );
}

#[test]
fn wait_writes_waiting_file() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    let json = start(&cli);
    let content_dir = PathBuf::from(json["content_dir"].as_str().unwrap());
    cli.ok(&["mockup", "wait", "--for", "a"]);
    let waiting: Vec<PathBuf> = std::fs::read_dir(&content_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            name.starts_with("waiting-") && name.ends_with(".json")
        })
        .collect();
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(std::fs::read_to_string(&waiting[0]).unwrap(), "{}");
}

#[test]
fn stop_kills_view_and_is_idempotent() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    start(&cli);
    assert!(has_view(&cli));
    cli.ok(&["mockup", "stop", "--for", "a"]);
    assert!(!has_view(&cli), "view still there after stop");
    cli.ok(&["mockup", "stop", "--for", "a"]);
}

#[test]
fn no_agent_exits_2() {
    require_tmux!();
    let cli = Cli::new();
    let project = project(&cli);
    let stderr = cli.fails(
        &["mockup", "start", "--project", project.to_str().unwrap()],
        2,
    );
    assert!(
        stderr.contains("tx mockup: no agent session (use --for)"),
        "{stderr}"
    );
}

/// Switched off, the plugin has no verb, as a bundle and compiled in alike.
#[test]
fn disabled_plugin_hides_verb() {
    for cli in [Cli::new(), Cli::compiled_in()] {
        cli.write_yaml("plugins:\n  mockup: { enabled: false }\n");
        let stderr = cli.fails(&["mockup", "stop", "--for", "a"], 2);
        assert!(stderr.contains("unknown verb \"mockup\""), "{stderr}");
        let help = cli.ok(&["help"]);
        assert!(!lists_verb(&help, "mockup"), "{help}");
        let listing = cli.ok(&["plugins"]);
        let rows: Vec<&str> = listing
            .lines()
            .filter(|row| row.starts_with("mockup\t"))
            .collect();
        assert_eq!(rows, ["mockup\tdisabled"]);
    }
}

/// The program of the bundle, where the build left it.
fn tx_mockup() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_tx")).with_file_name("tx-mockup")
}

/// The bundle every sandbox shares, and its program as `tx` runs it.
fn bundle() -> PathBuf {
    common::shipped().join("mockup")
}

/// The one row of `mockup` in `tx plugins --json`.
fn row(cli: &Cli) -> serde_json::Value {
    let rows: serde_json::Value = serde_json::from_str(&cli.ok(&["plugins", "--json"])).unwrap();
    let found: Vec<&serde_json::Value> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == "mockup")
        .collect();
    assert_eq!(found.len(), 1, "{rows}");
    found[0].clone()
}

/// The command line the view's pane was started with.
fn viewer_command(cli: &Cli) -> String {
    cli.tmux_out(&[
        "display-message",
        "-p",
        "-t",
        "=mockup-a:",
        "#{pane_start_command}",
    ])
}

/// `tx mockup show --for a` with every plugin program it starts traced into `<dir>/trace`: the
/// output of the command, and the trace.
fn traced_show(cli: &Cli) -> (Output, String) {
    let trace = cli.dir.path().join("trace");
    let output = cli.run(
        &["mockup", "show", "--for", "a"],
        &[("TX_PLUGIN_TRACE", trace.to_str().unwrap())],
        "",
    );
    (output, std::fs::read_to_string(&trace).unwrap_or_default())
}

#[test]
fn the_bundle_is_the_shipped_plugin_and_its_verbs_are_its_program() {
    require_tmux!();
    let cli = Cli::new();
    assert_eq!(
        row(&cli),
        serde_json::json!({
            "id": "mockup", "status": "active", "detail": null, "origin": "shipped",
            "directory": bundle(),
            "description": "Ratatui mockups for brainstorming, shown in a per-agent view.",
            "tier": "extra", "requires": ["ide"], "needs": [],
            "verbs": ["mockup", "_mockup-view"], "engine": null,
        })
    );
    let help = cli.ok(&["help"]);
    assert_eq!(
        help.lines()
            .filter(|line| line.starts_with("  mockup "))
            .count(),
        1,
        "{help}"
    );
    cli.create("a");
    start(&cli);
    let (output, trace) = traced_show(&cli);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "mockup ready in view mockup-a\n"
    );
    let program = bundle().join("bin/tx-mockup");
    assert_eq!(
        trace,
        format!("verb\tmockup\t{}\n", program.display()),
        "one plugin program was started: tx became it"
    );
    // The viewer is the plugin's own verb, so the view runs the plugin's program, on this core
    // and these plugin roots.
    let command = viewer_command(&cli);
    assert!(
        command.contains(&format!("{} --config-dir ", program.display())),
        "{command}"
    );
    assert!(
        command.contains(&format!("{} _mockup-view ", cli.arguments())),
        "{command}"
    );
    // A compiled-in verb starts no plugin program.
    let trace_file = cli.dir.path().join("trace");
    std::fs::remove_file(&trace_file).unwrap();
    let output = cli.run(
        &["layout", "ls"],
        &[("TX_PLUGIN_TRACE", trace_file.to_str().unwrap())],
        "",
    );
    assert!(output.status.success());
    assert!(!trace_file.exists());
}

#[test]
fn without_the_bundle_the_plugin_runs_compiled_into_tx() {
    require_tmux!();
    let cli = Cli::compiled_in();
    let row = row(&cli);
    assert_eq!(row["origin"], "built-in");
    assert_eq!(row["status"], "active");
    assert!(lists_verb(&cli.ok(&["help"]), "mockup"));
    cli.create("a");
    let json = start(&cli);
    assert_eq!(json["view"], "mockup-a");
    let (output, trace) = traced_show(&cli);
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "mockup ready in view mockup-a\n"
    );
    assert_eq!(trace, "", "no plugin program was started");
    let command = viewer_command(&cli);
    assert!(
        command.contains(&format!(
            "{} {} _mockup-view ",
            env!("CARGO_BIN_EXE_tx"),
            cli.arguments()
        )),
        "{command}"
    );
    cli.ok(&["mockup", "stop", "--for", "a"]);
    assert!(!has_view(&cli));
}

#[test]
fn a_directory_of_the_users_with_the_same_id_is_in_error_and_the_shipped_one_runs() {
    require_tmux!();
    let cli = Cli::new();
    let manifest = std::fs::read_to_string(bundle().join("plugin.yaml")).unwrap();
    cli.plugin("mockup", &manifest, &[]);
    let listing = cli.ok(&["plugins"]);
    let rows: Vec<&str> = listing
        .lines()
        .filter(|row| row.starts_with("mockup\t"))
        .collect();
    assert_eq!(
        rows,
        [
            "mockup\tactive".to_owned(),
            format!("mockup\terror\talso installed in {}", bundle().display())
        ]
    );
    cli.create("a");
    start(&cli);
    let (output, trace) = traced_show(&cli);
    assert!(output.status.success());
    assert_eq!(
        trace,
        format!(
            "verb\tmockup\t{}\n",
            bundle().join("bin/tx-mockup").display()
        )
    );
}

/// The round the `tx-mockup` skill describes, run by the agent itself: no `--for`, and `tx` is
/// the one its session is told about. As a bundle and compiled in.
#[test]
fn the_round_works_from_inside_an_agents_session() {
    require_tmux!();
    for cli in [Cli::new(), Cli::compiled_in()] {
        let checkout = cli.repository("a");
        let path = cli.shim_path(&[(
            "claude",
            "\"$TX_BIN\" mockup start --project \"$PWD\" > start.out 2> start.err; \
             \"$TX_BIN\" mockup show > show.out 2> show.err; echo done > round.done; exec sleep 60",
        )]);
        let output = cli.run(
            &[
                "create",
                "a",
                "--cwd",
                checkout.to_str().unwrap(),
                "--engine",
                "claude",
                "--prompt",
                "start",
            ],
            &[("PATH", path.as_str())],
            "",
        );
        assert!(output.status.success(), "{output:?}");
        let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let worktree = common::cwd_of(&cli, "a");
        assert!(
            cli.wait_for(10, || worktree.join("round.done").exists()),
            "the agent never finished the round"
        );
        let read = |name: &str| std::fs::read_to_string(worktree.join(name)).unwrap();
        assert_eq!(read("start.err"), "");
        let started: serde_json::Value = serde_json::from_str(read("start.out").trim()).unwrap();
        assert_eq!(started["view"], "mockup-a");
        let session_dir = worktree.join(".superpowers/mockup").join(&id);
        assert_eq!(
            started["content_dir"],
            session_dir.join("content").to_str().unwrap()
        );
        assert_eq!(read("show.err"), "");
        assert_eq!(read("show.out"), "mockup ready in view mockup-a\n");
        assert!(has_view(&cli));
        let views = cli.ok(&["ui", "view", "ls"]);
        assert!(views.contains("mockup-a"), "{views}");
    }
}

/// `tx-mockup` resolves the core itself, so it runs without `tx` in front for its own verbs,
/// and only for those.
#[test]
fn the_program_runs_directly_for_its_own_verbs_only() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    start(&cli);
    let direct = |args: &[&str]| {
        Command::new(tx_mockup())
            .args(common::directory_flags(&cli.state()))
            .args(common::plugin_flags(&cli.plugin_dirs))
            .args(args)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID")
            .env_remove("TX_BIN")
            .output()
            .unwrap()
    };
    let output = direct(&["mockup", "show", "--for", "a"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        cli.ok(&["mockup", "show", "--for", "a"]),
        "as `tx mockup show`"
    );
    // What goes wrong in tx reads as `tx` words it, to the byte.
    let output = direct(&["mockup", "show", "--for", "nobody"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        cli.fails(&["mockup", "show", "--for", "nobody"], 1)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "tx: no session \"nobody\"\n"
    );
    // Another plugin's verb is not this program's, though the core knows it.
    let output = direct(&["kill", "x"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "tx-mockup: not a verb of this plugin: kill\n"
    );
    assert!(output.stdout.is_empty());
    let output = Command::new(tx_mockup())
        .arg("--tx-probe")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        output.stdout.is_empty() && output.stderr.is_empty(),
        "{output:?}"
    );
}

fn client_sessions(cli: &Cli) -> Vec<String> {
    let mut sessions: Vec<String> = cli
        .tmux_out(&["list-clients", "-F", "#{client_session}"])
        .lines()
        .map(str::to_owned)
        .collect();
    sessions.sort();
    sessions
}

/// The operator's usual setup: an outer terminal on `home`, whose pane nest-attaches `a`.
/// A second `show` must not move the hidden nested client; `stop` returns the outer one.
#[test]
fn show_twice_under_nesting_keeps_the_operator_and_stop_returns_them() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    start(&cli);
    let sock = cli.sock().display().to_string();
    // Like `Ui::nest_attach_command`: job control on, or the nested attach exits at once.
    let nest = format!("bash -c 'set -m; TMUX= tmux -S {sock} attach -t =a; exec sh'");
    assert!(
        cli.tmux(&[
            "new-session",
            "-d",
            "-s",
            "home",
            "-x",
            "120",
            "-y",
            "40",
            &nest
        ])
        .status
        .success()
    );
    cli.in_outer_pane(&format!("TMUX= tmux -S {sock} attach -t =home"));
    let settled = |want: &[&str]| cli.wait_for(5, || client_sessions(&cli) == want);
    assert!(settled(&["a", "home"]), "{:?}", client_sessions(&cli));
    cli.ok(&["mockup", "show", "--for", "a"]);
    assert!(settled(&["a", "mockup-a"]), "{:?}", client_sessions(&cli));
    cli.ok(&["mockup", "show", "--for", "a"]);
    assert_eq!(client_sessions(&cli), ["a", "mockup-a"]);
    assert_eq!(
        cli.tmux_out(&["show-options", "-vt", "=mockup-a:", "@tx-mockup-return"]),
        "home"
    );
    cli.ok(&["mockup", "stop", "--for", "a"]);
    assert!(settled(&["a", "home"]), "{:?}", client_sessions(&cli));
    assert!(!has_view(&cli));
}
