//! Seam 2: in-process event sinks. Called after the record is saved and every lock is released;
//! cannot veto; a panicking sink never fails the operation.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Harness, agent, cwd_of, wait_for};
use tx_core::{AgentId, Event, EventSink, EventType, LlmEngineState, SendOptions, TxCore};

#[derive(Default)]
struct Recorder(Mutex<Vec<Event>>);

impl EventSink for Recorder {
    fn on_event(&self, event: &Event) {
        self.0.lock().unwrap().push(event.clone());
    }
}

fn types(events: &[Event]) -> Vec<EventType> {
    events.iter().map(|e| e.type_).collect()
}

#[test]
fn every_mutation_reaches_the_sink_in_order_with_its_session() {
    require_tmux!();
    let h = Harness::new();
    let recorder = Arc::new(Recorder::default());
    let mut core = h.open_with_engine();
    core.subscribe(Box::new(Arc::clone(&recorder)));
    let a = core
        .create_agent(agent("alice", &h.repository("a")))
        .unwrap();
    let cwd = cwd_of(&core, &a);
    let b = core.create_agent(agent("bob", &h.repository("b"))).unwrap();
    assert!(wait_for(Duration::from_secs(5), || cwd
        .join("env.txt")
        .exists()));
    core.rename_agent(&a, "alice2").unwrap();
    core.set_agent_tags(&a, vec!["t".into()]).unwrap();
    core.send_message(&a, "hi", SendOptions::from_agent(b.clone()))
        .unwrap();
    h.tmux_ok(&["kill-session", "-t", "=bob"]);
    core.refresh_all().unwrap();
    core.stop_agent(&a).unwrap();
    core.archive_agent(&b).unwrap();

    let events = recorder.0.lock().unwrap().clone();
    assert_eq!(
        types(&events),
        [
            EventType::Created,
            EventType::Created,
            EventType::Renamed,
            EventType::Tagged,
            EventType::MessageSent,
            EventType::Exited,
            EventType::Stopped,
            EventType::Archived,
        ]
    );
    assert_eq!(events[0].id, a);
    assert_eq!(events[0].name, "alice");
    assert_eq!(events[2].name, "alice2");
    assert_eq!(events[4].actor, Some(b.clone()));
    assert_eq!(
        events[4].id, a,
        "message-sent is attributed to the recipient"
    );
    assert_eq!(events[5].id, b);
    assert_eq!(events[7].id, b);
    assert!(
        events
            .iter()
            .all(|e| e.actor.is_none() || e.type_ == EventType::MessageSent)
    );
    assert!(events.windows(2).all(|w| w[0].ts <= w[1].ts));
    // The log carries the same facts, including the session id.
    let log = h.log_lines();
    assert_eq!(log.len(), events.len());
    for (line, event) in log.iter().zip(&events) {
        assert_eq!(line["id"], event.id.as_str());
        assert_eq!(line["type"], event.type_.as_str());
    }
}

/// A sink that re-enters the core through a second handle on the same state dir: it must see
/// the saved record and must not deadlock on the session lock.
struct Reentrant {
    core: TxCore,
    seen: Mutex<Vec<(EventType, LlmEngineState)>>,
}

impl EventSink for Reentrant {
    fn on_event(&self, event: &Event) {
        let session = self.core.get_agent(&event.id).ok();
        if session.is_some() && event.type_ == EventType::Created {
            self.core
                .set_agent_tags(&event.id, vec!["seen".into()])
                .unwrap();
        }
        if let Some(session) = session {
            self.seen.lock().unwrap().push((event.type_, session.state));
        }
    }
}

#[test]
fn sinks_run_after_the_record_is_saved_and_locks_are_released() {
    require_tmux!();
    let h = Harness::new();
    let sink = Arc::new(Reentrant {
        core: h.open(),
        seen: Mutex::new(vec![]),
    });
    let mut core = h.open_with_engine();
    core.subscribe(Box::new(Arc::clone(&sink)));
    let id = core.create_agent(agent("p", &h.repository("p"))).unwrap();
    core.stop_agent(&id).unwrap();
    let seen = sink.seen.lock().unwrap().clone();
    // The sink's own core has no subscribers, so its `tagged` goes to the log only.
    assert_eq!(
        seen,
        [
            (EventType::Created, LlmEngineState::Starting),
            (EventType::Stopped, LlmEngineState::Ended),
        ]
    );
    assert_eq!(core.get_agent(&id).unwrap().tags, ["seen"]);
    let types: Vec<String> = h
        .log_lines()
        .iter()
        .map(|l| l["type"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(types, ["created", "tagged", "stopped"]);
}

struct Panicky;

impl EventSink for Panicky {
    fn on_event(&self, _: &Event) {
        panic!("sink bug");
    }
}

#[test]
fn a_panicking_sink_never_fails_the_operation_and_later_sinks_still_run() {
    require_tmux!();
    let h = Harness::new();
    let recorder = Arc::new(Recorder::default());
    let mut core = h.open_with_engine();
    core.subscribe(Box::new(Panicky));
    core.subscribe(Box::new(Arc::clone(&recorder)));
    let id = core.create_agent(agent("p", &h.repository("p"))).unwrap();
    assert_ne!(core.get_agent(&id).unwrap().state, LlmEngineState::Ended);
    assert_eq!(types(&recorder.0.lock().unwrap()), [EventType::Created]);
    assert_eq!(h.log_lines().len(), 1);
}

#[test]
fn closures_are_sinks_and_cores_stay_send_sync() {
    require_tmux!();
    let h = Harness::new();
    let names = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut core = h.open_with_engine();
    let sink_names = Arc::clone(&names);
    core.subscribe(Box::new(move |event: &Event| {
        sink_names.lock().unwrap().push(event.name.clone());
    }));
    let core = Arc::new(core);
    let worker = {
        let core = Arc::clone(&core);
        let cwd = h.repository("t");
        std::thread::spawn(move || core.create_agent(agent("threaded", &cwd)).unwrap())
    };
    let id: AgentId = worker.join().unwrap();
    assert_eq!(core.get_agent(&id).unwrap().name, "threaded");
    assert_eq!(*names.lock().unwrap(), ["threaded"]);
}

#[test]
fn a_failed_operation_emits_nothing() {
    require_tmux!();
    let h = Harness::new();
    let recorder = Arc::new(Recorder::default());
    let mut core = h.open_with_engine();
    core.subscribe(Box::new(Arc::clone(&recorder)));
    let id = core.create_agent(agent("p", &h.repository("p"))).unwrap();
    assert!(
        core.create_agent(agent("bad name", &h.repository("q")))
            .is_err()
    );
    assert!(core.rename_agent(&id, "bad name").is_err());
    assert!(
        core.send_message(
            &"zz".parse::<AgentId>().unwrap(),
            "x",
            SendOptions::default()
        )
        .is_err()
    );
    assert_eq!(types(&recorder.0.lock().unwrap()), [EventType::Created]);
}
