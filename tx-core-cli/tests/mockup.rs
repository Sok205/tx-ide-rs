//! `txc mockup`: the session dir, the `mockup-<agent>` view, switching and the viewer.

mod common;

use std::path::{Path, PathBuf};

use common::{Cli, lists_verb};

fn project(cli: &Cli) -> PathBuf {
    let dir = cli.dir.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `txc mockup start --project <proj> --for a`, returning the printed JSON line.
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
        stderr.contains("txc mockup: run 'txc mockup start' first"),
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
        stderr.contains("txc mockup: no agent session (use --for)"),
        "{stderr}"
    );
}

#[test]
fn disabled_plugin_hides_verb() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  mockup: { enabled: false }\n");
    let stderr = cli.fails(&["mockup", "stop", "--for", "a"], 2);
    assert!(stderr.contains("unknown verb \"mockup\""), "{stderr}");
    let help = cli.ok(&["help"]);
    assert!(!lists_verb(&help, "mockup"), "{help}");
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
