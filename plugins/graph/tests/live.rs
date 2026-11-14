mod world;

use std::io::Write;

use tx_graph::app::{App, PULSE_TRAVEL, Source};
use tx_graph::model::{Filter, LogTail, Message, TranscriptReader};
use tx_graph::run;
use world::HOUR;

const LEAD: &str = "a0000000-0000-4000-8000-00000000000a";
const WORKER: &str = "b0000000-0000-4000-8000-00000000000b";

fn line(at: f64, actor: &str, to: &str) -> String {
    format!(
        r#"{{"ts":{at},"actor":"{actor}","type":"message-sent","id":"{to}","name":"x","msg":""}}"#
    )
}

fn append(path: &std::path::Path, text: &str) {
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

#[test]
fn the_tail_follows_whole_lines_and_restarts_on_a_replaced_log() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log.jsonl");
    let mut tail = LogTail::new(&path);
    assert!(tail.backfill(0.0, 100.0).unwrap().is_empty(), "no log yet");
    append(&path, &format!("{}\n", line(10.0, "", LEAD)));
    let mut tail = LogTail::new(&path);
    assert_eq!(tail.backfill(0.0, 100.0).unwrap().len(), 1);
    assert!(tail.follow().unwrap().is_empty());
    let next = line(20.0, LEAD, WORKER);
    let (head, rest) = next.split_at(30);
    append(&path, head);
    assert!(tail.follow().unwrap().is_empty(), "half a line waits");
    append(&path, &format!("{rest}\n"));
    let messages = tail.follow().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].at, 20.0);
    assert_eq!(messages[0].from.as_ref().unwrap().as_str(), LEAD);
    std::fs::write(&path, format!("{}\n", line(30.0, "", WORKER))).unwrap();
    let messages = tail.follow().unwrap();
    assert_eq!(messages.len(), 1, "a shorter log is read from its start");
    assert_eq!(messages[0].at, 30.0);
}

#[test]
fn transcripts_are_read_incrementally() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("t.jsonl");
    let usage = |tokens: u64| {
        format!(
            r#"{{"type":"assistant","message":{{"model":"m","usage":{{"input_tokens":{tokens}}}}}}}"#
        )
    };
    let mut reader = TranscriptReader::default();
    assert_eq!(reader.context(&path), None, "missing");
    append(&path, &format!("{}\n", usage(100)));
    assert_eq!(reader.context(&path).unwrap().tokens, 100);
    append(&path, &format!("{}\n", usage(250)));
    assert_eq!(reader.context(&path).unwrap().tokens, 250);
    std::fs::write(&path, format!("{}\n", usage(7))).unwrap();
    assert_eq!(
        reader.context(&path).unwrap().tokens,
        7,
        "a replaced transcript"
    );
}

fn fixture_app() -> App {
    let now = world::now();
    let messages =
        tx_graph::model::read_messages(&world::dir().join("log.jsonl"), now - HOUR, now).unwrap();
    App::new(
        Source {
            agents: world::agents(),
            messages,
            ..Source::default()
        },
        Filter::default(),
        now,
        HOUR,
    )
}

#[test]
fn new_messages_pulse_then_settle() {
    let mut app = fixture_app();
    run::snapshot(&mut app, 140, 40);
    assert!(!app.animating(), "the first fold does not pulse");
    let orchestrator = app.graph.nodes[app
        .graph
        .nodes
        .iter()
        .position(|node| node.name == "pv-orchestrator")
        .unwrap()]
    .id
    .clone()
    .unwrap();
    let pr4 = app.graph.nodes[app
        .graph
        .nodes
        .iter()
        .position(|node| node.name == "pv-pr4")
        .unwrap()]
    .id
    .clone()
    .unwrap();
    let before = app.graph.traffic.len();
    let message = |from: &tx_core::AgentId, to: &tx_core::AgentId| Message {
        at: app.now,
        from: Some(from.clone()),
        to: to.clone(),
    };
    let messages = vec![message(&pr4, &orchestrator), message(&pr4, &orchestrator)];
    app.receive(messages);
    app.rebuild();
    assert_eq!(app.graph.traffic.len(), before + 2);
    assert_eq!(app.pulses.len(), 2);
    assert_eq!(
        app.pulses[1].started - app.pulses[0].started,
        0.3,
        "staggered"
    );
    app.clock = 0.5;
    assert!(app.animating());
    // Without the sidebar, so the edge is long enough to show the pulse beside its badge.
    let frame = run::snapshot(&mut app, 120, 40);
    let pulses = std::mem::take(&mut app.pulses);
    let still = run::snapshot(&mut app, 120, 40);
    app.pulses = pulses;
    assert_ne!(frame, still, "a pulse is drawn");
    app.clock = 0.3 + PULSE_TRAVEL + 0.01;
    assert!(!app.animating());
    assert!(app.pulses.is_empty());
}

#[test]
fn messages_leave_with_the_window() {
    let mut app = fixture_app();
    let before = app.source.messages.len();
    app.now += HOUR;
    app.receive(Vec::new());
    assert!(app.source.messages.len() < before);
    assert!(
        app.source
            .messages
            .iter()
            .all(|message| message.at >= app.now - HOUR)
    );
}
