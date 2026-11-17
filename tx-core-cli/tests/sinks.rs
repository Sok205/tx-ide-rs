//! A plugin's `on-event` program, end to end through the binaries: `log-events`, a plugin of the
//! sandbox's own whose sink appends each event line to its data directory and prints `noise`.
//! The agents that need a session are the stand-in engine's (`fixtures/engine-fake`, whose
//! program is `sleep`); those tests skip themselves without tmux or `python3`. The others act on
//! a planted record.

mod common;

use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

use common::Cli;
use serde_json::Value;

const LOG_EVENTS: &str = "log-events";

/// The card's sink: every event line appended to `$TX_PLUGIN_DATA_DIR/events.jsonl`, and a word
/// on its output that nobody may see.
const SINK: &str =
    r#"mkdir -p "$TX_PLUGIN_DATA_DIR"; cat >> "$TX_PLUGIN_DATA_DIR/events.jsonl"; echo noise"#;

/// An id for a planted record.
const PLANTED: &str = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";

impl Cli {
    /// Installs `log-events` with `on-event: {events: <events>, bin: sink}` plus the lines
    /// `extra` under it, and `script` as `bin/sink`.
    fn log_events(&self, events: &str, extra: &str, script: &str) {
        self.plugin(
            LOG_EVENTS,
            &format!(
                "api: 1\nid: {LOG_EVENTS}\ndescription: Appends every event to a file.\n\
                 on-event:\n  events: {events}\n  bin: sink\n{extra}"
            ),
            &[("sink", script)],
        );
    }

    fn events_file(&self) -> PathBuf {
        self.state()
            .join("plugins")
            .join(LOG_EVENTS)
            .join("events.jsonl")
    }

    /// Every line the sink received, in order.
    fn sunk(&self) -> Vec<Value> {
        std::fs::read_to_string(self.events_file())
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn sunk_types(&self) -> Vec<String> {
        self.sunk()
            .iter()
            .map(|line| line["type"].as_str().unwrap().to_owned())
            .collect()
    }

    /// An agent of the stand-in engine named `name`: what `tx create` printed.
    fn create_fake(&self, name: &str) -> Output {
        let cwd = self.repository(name);
        self.run(
            &[
                "create",
                name,
                "--cwd",
                cwd.to_str().unwrap(),
                "--engine",
                "fake",
                "--prompt",
                "hi",
            ],
            &[],
            "",
        )
    }

    /// The trace lines `tx <args>` leaves.
    fn traced(&self, args: &[&str]) -> Vec<String> {
        let trace = self.dir.path().join("trace");
        std::fs::write(&trace, "").unwrap();
        let output = self.run(args, &[("TX_PLUGIN_TRACE", trace.to_str().unwrap())], "");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

/// A sandbox with the stand-in engine installed.
fn with_the_stand_in() -> Cli {
    let cli = Cli::new();
    cli.fixture("engine-fake");
    cli
}

fn sinks_started(lines: &[String]) -> usize {
    lines
        .iter()
        .filter(|line| line.starts_with("sink\t"))
        .count()
}

/// Acceptance 1 and 6: every operation reaches the sink, and `tx create` prints only the id.
#[test]
fn every_operation_reaches_the_sink_and_create_prints_only_the_id() {
    require_tmux!();
    require_python3!();
    let cli = with_the_stand_in();
    cli.log_events("all", "", SINK);
    let created = cli.create_fake("a");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let stdout = String::from_utf8(created.stdout).unwrap();
    let id = stdout.trim();
    assert_eq!(stdout, format!("{id}\n"), "the sink's output leaked");
    assert_eq!(cli.sunk_types(), ["created"]);
    let line = &cli.sunk()[0];
    assert_eq!(line["id"], id);
    assert_eq!(line["name"], "a");
    assert_eq!(line["actor"], "");

    let hook = cli.txi(&["hook", "Stop"], &[("TX_SESSION_ID", id)], "");
    assert!(hook.status.success());
    assert_eq!(
        String::from_utf8_lossy(&hook.stdout),
        "",
        "the sink's output leaked into a hook"
    );
    cli.ok(&["rename", "a", "a1"]);
    cli.ok(&["tag", "a1", "x"]);
    cli.ok(&["send", "a1", "--", "one more line"]);
    assert!(cli.create_fake("b").status.success());
    cli.ok(&["stop", "b"]);
    cli.ok(&["stop", "a1"]);
    let types = cli.sunk_types();
    for expected in [
        "created",
        "state-changed",
        "renamed",
        "tagged",
        "message-sent",
        "stopped",
    ] {
        assert!(types.iter().any(|type_| type_ == expected), "{types:?}");
    }
    assert_eq!(
        types.iter().filter(|type_| *type_ == "created").count(),
        2,
        "{types:?}"
    );
}

/// Acceptance 2: a sink gets only the types it lists.
#[test]
fn a_sink_gets_only_the_event_types_it_lists() {
    require_tmux!();
    require_python3!();
    let cli = with_the_stand_in();
    cli.log_events("[message-sent]", "", SINK);
    assert!(cli.create_fake("a").status.success());
    cli.ok(&["rename", "a", "a1"]);
    cli.ok(&["tag", "a1", "x"]);
    cli.ok(&["send", "a1", "--", "one more line"]);
    cli.ok(&["stop", "a1"]);
    assert_eq!(cli.sunk_types(), ["message-sent"]);
    assert_eq!(cli.sunk()[0]["name"], "a1");
}

/// Acceptance 3: one process's events reach the sink in the order it emitted them.
#[test]
fn twenty_tags_arrive_in_order() {
    let cli = Cli::new();
    cli.log_events("all", "", SINK);
    common::plant_record(&cli, PLANTED, "a", "claude");
    for index in 1..=20 {
        cli.ok(&["tag", "a", &format!("t{index}")]);
    }
    let sunk = cli.sunk();
    assert_eq!(sunk.len(), 20);
    for (index, line) in sunk.iter().enumerate() {
        assert_eq!(line["type"], "tagged");
        assert!(
            line["detail"]
                .as_str()
                .unwrap()
                .contains(&format!("t{}", index + 1)),
            "{line}"
        );
    }
    let stamps: Vec<f64> = sunk
        .iter()
        .map(|line| line["ts"].as_f64().unwrap())
        .collect();
    assert!(
        stamps.windows(2).all(|pair| pair[0] < pair[1]),
        "{stamps:?}"
    );
}

/// Acceptance 4: a sink past its timeout is killed and named; the operation stands.
#[test]
fn a_sink_past_its_timeout_is_killed_and_named_and_the_rename_stands() {
    let cli = Cli::new();
    cli.log_events("all", "  timeout: 1\n", "sleep 60");
    common::plant_record(&cli, PLANTED, "a", "claude");
    // The sandbox's first run builds the plugin index; it is not the sink's time.
    cli.ok(&["ls", "--ended"]);
    let started = Instant::now();
    let output = cli.run(&["rename", "a", "a2"], &[], "");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?} {}",
        started.elapsed(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "tx: plugin log-events: on-event timed out after 1s\n"
    );
    assert!(cli.ok(&["show", "a2"]).contains(PLANTED));
}

/// Acceptance 5, and a sink that cannot start: neither fails the command.
#[test]
fn a_failing_sink_or_one_that_cannot_start_fails_nothing() {
    let cli = Cli::new();
    cli.log_events("all", "", "exit 9");
    common::plant_record(&cli, PLANTED, "a", "claude");
    let output = cli.run(&["tag", "a", "z"], &[], "");
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");

    let program = cli.plugins_dir().join(LOG_EVENTS).join("bin/sink");
    std::fs::remove_file(&program).unwrap();
    let output = cli.run(&["tag", "a", "y"], &[], "");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with(&format!(
            "tx: plugin log-events: on-event cannot run {}: ",
            program.display()
        )),
        "{stderr}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(cli.ok(&["show", "a"]).contains("\"y\""));
}

/// Acceptance 7: the sink's line holds the whole detail; the log's line is cut at 512 bytes.
#[test]
fn the_sink_gets_the_whole_detail_and_the_log_a_cut_line() {
    let cli = Cli::new();
    cli.log_events("all", "", SINK);
    common::plant_record(&cli, PLANTED, "a", "claude");
    let tags: Vec<String> = (0..100).map(|index| format!("tag{index:03}")).collect();
    let mut args = vec!["tag", "a"];
    args.extend(tags.iter().map(String::as_str));
    cli.ok(&args);
    let sunk = cli.sunk();
    let detail = sunk.last().unwrap()["detail"].as_str().unwrap().to_owned();
    assert!(detail.len() > 600, "{detail}");
    for tag in &tags {
        assert!(detail.contains(tag.as_str()), "{tag} missing from {detail}");
    }
    let log = std::fs::read_to_string(cli.state().join("log.jsonl")).unwrap();
    let logged = log
        .lines()
        .rfind(|line| line.contains("\"tagged\""))
        .unwrap();
    assert_eq!(logged.len() + 1, 512, "{logged}");
}

/// Acceptance 8: without an `on-event` plugin no sink starts; with one, one per event.
#[test]
fn a_sink_starts_only_when_a_plugin_declares_one() {
    let cli = Cli::new();
    common::plant_record(&cli, PLANTED, "a", "claude");
    assert_eq!(sinks_started(&cli.traced(&["rename", "a", "a1"])), 0);
    cli.log_events("all", "", SINK);
    let lines = cli.traced(&["rename", "a1", "a2"]);
    assert_eq!(
        lines,
        [format!(
            "sink\t{LOG_EVENTS}\t{}",
            cli.plugins_dir()
                .join(LOG_EVENTS)
                .join("bin/sink")
                .display()
        )]
    );
    cli.write_yaml("plugins:\n  log-events: { enabled: false }\n");
    assert_eq!(sinks_started(&cli.traced(&["rename", "a2", "a3"])), 0);
}
