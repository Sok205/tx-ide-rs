//! `txc timeline`, `_timeline-json`, `_timeline-attach` through the binary: main's fixture
//! payload and frame, malformed ext tolerance, and prefix+T opening the resident popup.

mod common;

#[path = "../../plugins/timeline/tests/world/mod.rs"]
mod world;

use std::path::PathBuf;
use std::process::{Command, Output};

use common::{Cli, lists_verb};
use serde_json::Value;

fn fixtures() -> PathBuf {
    world::fixture_dir_from(env!("CARGO_MANIFEST_DIR"))
}

/// The fixture world's records in the state dir.
fn seed(cli: &Cli) -> Value {
    let dir = cli.state().join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    for record in world::records(&fixtures()) {
        let id = record["id"].as_str().unwrap();
        std::fs::write(dir.join(format!("{id}.json")), record.to_string()).unwrap();
    }
    world::world(&fixtures())
}

fn utc(cli: &Cli, args: &[&str]) -> Output {
    cli.run(args, &[("TZ", "UTC")], "")
}

fn stdout(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn help_lists_timeline() {
    let cli = Cli::new();
    let help = cli.ok(&["help"]);
    for verb in [
        "timeline",
        "_timeline-resident",
        "_timeline-attach",
        "_timeline-json",
    ] {
        assert!(lists_verb(&help, verb), "{verb}: {help}");
    }
}

#[test]
fn timeline_json_matches_mains_fixture() {
    let cli = Cli::new();
    let world = seed(&cli);
    let now = world["now"].to_string();
    let out = stdout(&utc(
        &cli,
        &["_timeline-json", "--days", "1", "--now", &now],
    ));
    let ours: Value = serde_json::from_str(&out).unwrap();
    let main: Value = serde_json::from_str(&world::expected(&fixtures(), "timeline.json")).unwrap();
    assert_eq!(ours, main);
}

#[test]
fn timeline_snapshot_matches_mains_frame() {
    let cli = Cli::new();
    let world = seed(&cli);
    let now = world["now"].to_string();
    let args = [
        "timeline",
        "--snapshot",
        "--days",
        "1",
        "--last",
        "4h",
        "--cols",
        "120",
        "--rows",
        "40",
        "--now",
        &now,
    ];
    let out = stdout(&utc(&cli, &args));
    let main = world::expected(&fixtures(), "snapshot-120x40-4h.txt");
    assert_eq!(out, format!("{main}\n"));
}

#[test]
fn a_malformed_ext_never_fails_the_view_and_health_reports_it() {
    let cli = Cli::new();
    seed(&cli);
    let path = cli
        .state()
        .join("sessions/a0000000-0000-4000-8000-00000000000a.json");
    let mut record: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    record["ext"]["chat"] = Value::from("not an object");
    record["ext"]["hooks"] = Value::from(7);
    std::fs::write(&path, record.to_string()).unwrap();
    let out = stdout(&utc(
        &cli,
        &["_timeline-json", "--days", "1", "--now", "1791720000"],
    ));
    let payload: Value = serde_json::from_str(&out).unwrap();
    let lead = payload["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lane| lane["id"] == "lead")
        .expect("the lane survives without its transcript");
    assert_eq!(lead["state"], "idle");
    let snapshot = utc(&cli, &["timeline", "--snapshot", "--now", "1791720000"]);
    stdout(&snapshot);
    let health = cli.run(&["timeline", "health"], &[], "");
    assert_eq!(health.status.code(), Some(1));
    let text = String::from_utf8_lossy(&health.stdout);
    let line = text
        .lines()
        .find(|line| line.starts_with("lead\t"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(line.contains("ext.chat malformed"), "{line}");
    assert!(line.contains("ext.hooks malformed"), "{line}");
    assert!(
        text.lines()
            .any(|line| line.starts_with("worker\t") && line.ends_with("1/1 transcripts\tok")),
        "{text}"
    );
}

#[test]
fn usage_mistakes_exit_2() {
    let cli = Cli::new();
    for args in [
        &["timeline", "--last", "soon"][..],
        &["timeline", "--bogus"],
        &["timeline", "health", "x"],
        &["_timeline-json", "--snapshot"],
        &["_timeline-attach", "x"],
    ] {
        cli.fails(args, 2);
    }
}

fn outer(cli: &Cli, args: &[&str]) -> Output {
    Command::new("tmux")
        .arg("-S")
        .arg(cli.outer_sock())
        .args(["-f", "/dev/null"])
        .args(args)
        .env_remove("TMUX")
        .output()
        .unwrap()
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

#[test]
fn prefix_shift_t_opens_the_resident_popup_and_q_detaches() {
    require_tmux!();
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  assistant: { enabled: false }\n");
    cli.ok(&["ui", "install"]);
    let conf = cli.state().join("tmux.conf");
    let started = Command::new("tmux")
        .arg("-S")
        .arg(cli.sock())
        .arg("-f")
        .arg(&conf)
        .args([
            "new-session",
            "-d",
            "-s",
            "home",
            "-x",
            "120",
            "-y",
            "40",
            "sh",
        ])
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    assert!(
        cli.wait_for(5, || cli
            .tmux_out(&["list-keys", "-T", "prefix", "T"])
            .contains("_timeline-attach")),
        "prefix+T never bound"
    );
    let sock = cli.sock().display().to_string();
    cli.in_outer_pane(&format!("TMUX= tmux -S {sock} attach -t =home"));
    let settled = |want: &[&str]| cli.wait_for(10, || client_sessions(&cli) == want);
    assert!(settled(&["home"]), "{:?}", client_sessions(&cli));

    let press = |keys: &[&str]| {
        let mut args = vec!["send-keys", "-t", "=outer:"];
        args.extend_from_slice(keys);
        assert!(outer(&cli, &args).status.success());
    };
    press(&["C-a", "T"]);
    assert!(
        settled(&["home", "tx-timeline"]),
        "the popup never attached the resident: {:?}",
        client_sessions(&cli)
    );
    assert_eq!(
        cli.tmux_out(&["show-options", "-vt", "=tx-timeline:", "@tx-view"]),
        "1"
    );
    let resident = cli.tmux_out(&["list-panes", "-t", "=tx-timeline:", "-F", "#{pane_pid}"]);
    // The resident is drawing: its axis carries the clock.
    assert!(
        cli.wait_for(10, || cli
            .tmux_out(&["capture-pane", "-p", "-t", "=tx-timeline:"])
            .contains("now ")),
        "{}",
        cli.tmux_out(&["capture-pane", "-p", "-t", "=tx-timeline:"])
    );

    press(&["q"]);
    assert!(
        settled(&["home"]),
        "q did not detach the popup: {:?}",
        client_sessions(&cli)
    );
    assert!(
        cli.tmux(&["has-session", "-t", "=tx-timeline"])
            .status
            .success(),
        "the resident stays warm"
    );

    press(&["C-a", "T"]);
    assert!(
        settled(&["home", "tx-timeline"]),
        "{:?}",
        client_sessions(&cli)
    );
    assert_eq!(
        cli.tmux_out(&["list-panes", "-t", "=tx-timeline:", "-F", "#{pane_pid}"]),
        resident,
        "the second popup reuses the same resident"
    );
    press(&["q"]);
    assert!(settled(&["home"]), "{:?}", client_sessions(&cli));
    outer(&cli, &["kill-server"]);
}
