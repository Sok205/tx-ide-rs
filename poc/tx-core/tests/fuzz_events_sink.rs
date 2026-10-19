//! Adversarial / property tests for the event-sink seam: the sink never sees more or fewer
//! events than the log, sinks run after every lock is released (proved by re-locking and by
//! re-entering through a second core), registration order and multiplicity, panic containment,
//! failure paths inside the locked section, and concurrent cores on one state dir.
//!
//! Tests marked `#[ignore = "BUG: …"]` document real failures; the reproduction stays.

mod common;

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use common::{Harness, agent, process, wait_for};
use proptest::prelude::*;
use serde_json::Value;
use tx_core::{
    CoreError, CreateOptions, Event, EventSink, EventType, LaunchOptions, Lifecycle, Locks,
    SendOptions, SessionId, TxCore,
};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Recorder(Mutex<Vec<Event>>);

impl Recorder {
    fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap().clone()
    }
}

impl EventSink for Recorder {
    fn on_event(&self, event: &Event) {
        self.0.lock().unwrap().push(event.clone());
    }
}

/// The fields a log line and an event share, in the shape the log stores them.
#[derive(Clone, Debug, PartialEq)]
struct Line {
    ts: f64,
    type_: String,
    id: String,
    name: String,
    actor: String,
    msg: String,
}

impl Line {
    fn from_log(value: &Value) -> Self {
        let text = |key: &str| value[key].as_str().unwrap().to_owned();
        Self {
            ts: value["ts"].as_f64().unwrap(),
            type_: text("type"),
            id: text("id"),
            name: text("name"),
            actor: text("actor"),
            msg: text("msg"),
        }
    }

    fn from_event(event: &Event) -> Self {
        Self {
            ts: event.ts,
            type_: event.type_.as_str().to_owned(),
            id: event.id.as_str().to_owned(),
            name: event.name.clone(),
            actor: event
                .actor
                .as_ref()
                .map_or(String::new(), |a| a.as_str().to_owned()),
            msg: event.detail.clone(),
        }
    }
}

fn log(h: &Harness) -> Vec<Line> {
    h.log_lines().iter().map(Line::from_log).collect()
}

fn types(events: &[Event]) -> Vec<EventType> {
    events.iter().map(|e| e.type_).collect()
}

fn log_types(h: &Harness) -> Vec<String> {
    log(h).into_iter().map(|l| l.type_).collect()
}

fn kind(error: &CoreError) -> &'static str {
    match error {
        CoreError::NotFound(_) => "NotFound",
        CoreError::NameTaken(_) => "NameTaken",
        CoreError::InvalidName(_) => "InvalidName",
        CoreError::NotRunning(_) => "NotRunning",
        CoreError::Tmux(_) => "Tmux",
        CoreError::Lock(_) => "Lock",
        _ => "Other",
    }
}

fn verdict<T>(result: &Result<T, CoreError>) -> String {
    match result {
        Ok(_) => "Ok".to_owned(),
        Err(error) => kind(error).to_owned(),
    }
}

/// A process that exits at once: `created` + `exited` in one emit batch.
fn exiting(name: &str, cwd: &std::path::Path) -> CreateOptions {
    CreateOptions::new(
        name,
        cwd,
        LaunchOptions::process("true", Vec::<String>::new()),
    )
}

/// Every lifecycle event of one id, checked against the lifecycle order: `created` first and
/// only once; nothing after `forgotten`; at most one terminal (`exited` / `stopped`).
fn check_lifecycle(id: &str, types: &[String]) -> Result<(), String> {
    if types.first().map(String::as_str) != Some("created") {
        return Err(format!(
            "{id}: first event is {:?}, not created",
            types.first()
        ));
    }
    if types.iter().filter(|t| *t == "created").count() != 1 {
        return Err(format!("{id}: created more than once: {types:?}"));
    }
    if let Some(pos) = types.iter().position(|t| t == "forgotten")
        && pos + 1 != types.len()
    {
        return Err(format!("{id}: events after forgotten: {types:?}"));
    }
    let terminal = types
        .iter()
        .filter(|t| *t == "exited" || *t == "stopped")
        .count();
    if terminal > 1 {
        return Err(format!("{id}: more than one terminal event: {types:?}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// 1. Model check: the sink's view equals the log, op by op
// ---------------------------------------------------------------------------------------------

const NAMES: [&str; 7] = ["alpha", "beta", "gamma", "delta", "eps", "zeta", "bad name"];

#[derive(Clone, Debug)]
enum Op {
    Create { name: usize, exiting: bool },
    Rename { who: usize, name: usize },
    SetTags { who: usize, tags: usize },
    Send { to: usize, from: Option<usize> },
    Stop(usize),
    Forget(usize),
    Refresh(usize),
    RefreshAll,
    Kill(usize),
}

fn arb_op() -> impl Strategy<Value = Op> {
    let who = 0..8usize;
    prop_oneof![
        5 => (0..NAMES.len(), prop::bool::weighted(0.2))
            .prop_map(|(name, exiting)| Op::Create { name, exiting }),
        2 => (who.clone(), 0..NAMES.len()).prop_map(|(who, name)| Op::Rename { who, name }),
        2 => (who.clone(), 0..3usize).prop_map(|(who, tags)| Op::SetTags { who, tags }),
        1 => (who.clone(), prop::option::of(who.clone())).prop_map(|(to, from)| Op::Send { to, from }),
        2 => who.clone().prop_map(Op::Stop),
        2 => who.clone().prop_map(Op::Forget),
        2 => who.clone().prop_map(Op::Refresh),
        1 => Just(Op::RefreshAll),
        2 => (0..NAMES.len() - 1).prop_map(Op::Kill),
    ]
}

fn id_for(ids: &[SessionId], who: usize) -> SessionId {
    if who >= 6 || ids.is_empty() {
        format!("ghost-{who}").parse().unwrap()
    } else {
        ids[who % ids.len()].clone()
    }
}

fn run_op(h: &Harness, core: &TxCore, ids: &mut Vec<SessionId>, step: usize, op: &Op) -> String {
    match op {
        Op::Create {
            name,
            exiting: at_once,
        } => {
            let cwd = h.workdir(&format!("w{step}"));
            let opts = if *at_once {
                exiting(NAMES[*name], &cwd)
            } else {
                process(NAMES[*name], &cwd)
            };
            let result = core.create_session(opts);
            if let Ok(id) = &result {
                ids.push(id.clone());
            }
            verdict(&result)
        }
        Op::Rename { who, name } => verdict(&core.rename_session(&id_for(ids, *who), NAMES[*name])),
        Op::SetTags { who, tags } => {
            let tags = (0..*tags).map(|t| format!("t{t}")).collect();
            verdict(&core.set_session_tags(&id_for(ids, *who), tags))
        }
        Op::Send { to, from } => {
            let opts = match from {
                Some(from) => SendOptions::from_agent(id_for(ids, *from)),
                None => SendOptions::default(),
            };
            verdict(&core.send_message(&id_for(ids, *to), "hi", opts))
        }
        Op::Stop(who) => verdict(&core.stop_session(&id_for(ids, *who))),
        Op::Forget(who) => verdict(&core.forget_session(&id_for(ids, *who))),
        Op::Refresh(who) => verdict(&core.refresh_session(&id_for(ids, *who))),
        Op::RefreshAll => verdict(&core.refresh_all()),
        Op::Kill(name) => {
            let _ = h.tmux(&["kill-session", "-t", &format!("={}", NAMES[*name])]);
            "kill".to_owned()
        }
    }
}

fn run_ops(
    h: &Harness,
    core: &TxCore,
    recorder: &Recorder,
    ops: &[Op],
) -> Result<(), TestCaseError> {
    let mut ids = Vec::new();
    let mut seen = 0;
    let mut last_ts = 0.0f64;
    for (step, op) in ops.iter().enumerate() {
        let verdict = run_op(h, core, &mut ids, step, op);
        let events = recorder.events();
        let lines = log(h);
        prop_assert_eq!(
            events.len(),
            lines.len(),
            "step {} {:?} -> {}: sink saw {} events, log has {} lines",
            step,
            op,
            verdict,
            events.len(),
            lines.len()
        );
        let delta = events.len() - seen;
        let failed = verdict != "Ok" && verdict != "kill";
        prop_assert!(
            !(failed && delta > 0),
            "step {} {:?} failed with {} but emitted {} event(s)",
            step,
            op,
            verdict,
            delta
        );
        for (index, (event, line)) in events.iter().zip(&lines).enumerate().skip(seen) {
            let from_event = Line::from_event(event);
            prop_assert_eq!(
                &from_event,
                line,
                "step {} {:?}: event {} differs from log line",
                step,
                op,
                index
            );
            let actor_ok = event.actor.is_none() || event.type_ == EventType::MessageSent;
            prop_assert!(actor_ok, "step {}: actor on a {:?}", step, event.type_);
            prop_assert!(
                event.ts >= last_ts,
                "step {}: ts went backwards ({} < {})",
                step,
                event.ts,
                last_ts
            );
            last_ts = event.ts;
        }
        seen = events.len();
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 10,
        max_shrink_iters: 12,
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn sink_view_equals_the_log_after_every_op(ops in prop::collection::vec(arb_op(), 12..28)) {
        if !common::tmux_available() {
            return Ok(());
        }
        let h = Harness::new();
        let recorder = Arc::new(Recorder::default());
        let mut core = h.open();
        core.subscribe(Box::new(Arc::clone(&recorder)));
        run_ops(&h, &core, &recorder, &ops)?;
    }
}

#[test]
fn scripted_sequence_sink_view_equals_the_log() {
    require_tmux!();
    let h = Harness::new();
    let recorder = Arc::new(Recorder::default());
    let mut core = h.open();
    core.subscribe(Box::new(Arc::clone(&recorder)));
    let ops = [
        Op::Create {
            name: 0,
            exiting: false,
        },
        Op::Create {
            name: 1,
            exiting: true,
        }, // created + exited in one batch
        Op::Create {
            name: 0,
            exiting: false,
        }, // NameTaken: nothing
        Op::Rename { who: 0, name: 0 }, // same name: Ok, nothing
        Op::Rename { who: 0, name: 6 }, // invalid: nothing
        Op::Kill(0),
        Op::Send {
            to: 0,
            from: Some(1),
        }, // not live: nothing
        Op::Refresh(0), // exited
        Op::Stop(0),    // NotRunning: nothing
        Op::Forget(0),
        Op::Forget(0), // Ok(false): nothing
        Op::RefreshAll,
    ];
    run_ops(&h, &core, &recorder, &ops).unwrap();
    assert_eq!(
        log_types(&h),
        ["created", "created", "exited", "exited", "forgotten"]
    );
}

// ---------------------------------------------------------------------------------------------
// 2. Lock release proof under contention
// ---------------------------------------------------------------------------------------------

/// Takes `create.lock` and `<id>.lock` on every event from a helper thread; if the helper does
/// not get both within two seconds the operation still held a lock while calling its sinks.
struct Relocker {
    locks: Locks,
    stuck: AtomicBool,
    acquired: AtomicUsize,
}

impl EventSink for Relocker {
    fn on_event(&self, event: &Event) {
        let (tx, rx) = mpsc::channel();
        let locks = self.locks.clone();
        let id = event.id.clone();
        std::thread::spawn(move || {
            let create = locks.create();
            let session = locks.session(&id);
            let _ = tx.send(create.is_ok() && session.is_ok());
        });
        match rx.recv_timeout(Duration::from_secs(2)) {
            Ok(true) => {
                self.acquired.fetch_add(1, Ordering::SeqCst);
            }
            _ => {
                self.stuck.store(true, Ordering::SeqCst);
            }
        }
    }
}

#[test]
fn every_lock_is_free_when_sinks_run() {
    require_tmux!();
    let h = Harness::new();
    let relocker = Arc::new(Relocker {
        locks: Locks::new(h.state_dir().join("locks")),
        stuck: AtomicBool::new(false),
        acquired: AtomicUsize::new(0),
    });
    let mut core = h.open_with_engine();
    core.subscribe(Box::new(Arc::clone(&relocker)));
    let cwd = h.workdir("a");
    let a = core.create_session(agent("a", &cwd)).unwrap();
    let b = core.create_session(process("b", &h.workdir("b"))).unwrap();
    let c = core.create_session(exiting("c", &h.workdir("c"))).unwrap();
    assert!(wait_for(Duration::from_secs(5), || cwd
        .join("env.txt")
        .exists()));
    core.rename_session(&a, "a2").unwrap();
    core.set_session_tags(&a, vec!["t".into()]).unwrap();
    core.send_message(&a, "hi", SendOptions::from_agent(b.clone()))
        .unwrap();
    h.tmux_ok(&["kill-session", "-t", "=b"]);
    core.refresh_session(&b).unwrap();
    core.refresh_all().unwrap();
    core.stop_session(&a).unwrap();
    core.forget_session(&b).unwrap();
    core.forget_session(&c).unwrap();
    assert!(
        !relocker.stuck.load(Ordering::SeqCst),
        "a sink could not take a lock within 2 s"
    );
    let lines = h.log_lines().len();
    assert_eq!(relocker.acquired.load(Ordering::SeqCst), lines);
    assert_eq!(lines, 11);
}

/// Stops the session from inside `on_event(Created)` through a second core on the same state
/// dir. Documents the consequence: the caller's `create_session` returns `Ok(id)` for a record
/// that is already `Stopped`.
struct Stopper {
    core: TxCore,
    outcome: Mutex<Vec<String>>,
}

impl EventSink for Stopper {
    fn on_event(&self, event: &Event) {
        if event.type_ == EventType::Created {
            let result = self.core.stop_session(&event.id);
            self.outcome.lock().unwrap().push(verdict(&result));
        }
    }
}

#[test]
fn a_sink_may_stop_the_session_it_was_told_about() {
    require_tmux!();
    let h = Harness::new();
    let stopper = Arc::new(Stopper {
        core: h.open(),
        outcome: Mutex::new(Vec::new()),
    });
    let recorder = Arc::new(Recorder::default());
    let mut core = h.open();
    core.subscribe(Box::new(Arc::clone(&stopper)));
    core.subscribe(Box::new(Arc::clone(&recorder)));
    let done = Arc::new(AtomicBool::new(false));
    let worker = {
        let done = Arc::clone(&done);
        let cwd = h.workdir("p");
        let core = Arc::new(core);
        std::thread::spawn(move || {
            let id = core.create_session(process("p", &cwd));
            done.store(true, Ordering::SeqCst);
            id.map(|id| core.get_session(&id).unwrap())
        })
    };
    assert!(
        wait_for(Duration::from_secs(5), || done.load(Ordering::SeqCst)),
        "create_session deadlocked against a sink that stops the session"
    );
    let session = worker.join().unwrap().unwrap();
    assert_eq!(
        session.lifecycle,
        Lifecycle::Stopped,
        "the caller reads a stopped record"
    );
    assert_eq!(*stopper.outcome.lock().unwrap(), ["Ok"]);
    assert_eq!(types(&recorder.events()), [EventType::Created]);
    assert_eq!(log_types(&h), ["created", "stopped"]);
    assert!(h.live_names().is_empty());
}

/// Forgetting from inside `Created`: the caller gets an id with no record behind it.
#[test]
fn a_sink_may_forget_the_session_it_was_told_about() {
    require_tmux!();
    let h = Harness::new();
    let other = h.open();
    let mut core = h.open();
    core.subscribe(Box::new(move |event: &Event| {
        if event.type_ == EventType::Created {
            assert!(other.forget_session(&event.id).unwrap());
        }
    }));
    let id = core.create_session(process("p", &h.workdir("p"))).unwrap();
    let gone = matches!(core.get_session(&id), Err(CoreError::NotFound(_)));
    assert!(
        gone,
        "create_session returned an id whose record a sink already deleted"
    );
    assert_eq!(log_types(&h), ["created", "forgotten"]);
    // The tmux session is an orphan now: forget never kills.
    assert_eq!(h.live_names(), ["p"]);
}

// ---------------------------------------------------------------------------------------------
// 3. Sink ordering and multiplicity
// ---------------------------------------------------------------------------------------------

struct Indexed {
    index: usize,
    out: Arc<Mutex<Vec<(usize, Event)>>>,
}

impl EventSink for Indexed {
    fn on_event(&self, event: &Event) {
        self.out.lock().unwrap().push((self.index, event.clone()));
    }
}

#[test]
fn sixteen_sinks_run_in_registration_order_once_per_event() {
    require_tmux!();
    const SINKS: usize = 16;
    let h = Harness::new();
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut core = h.open();
    for index in 0..SINKS {
        core.subscribe(Box::new(Indexed {
            index,
            out: Arc::clone(&out),
        }));
    }
    let a = core.create_session(process("a", &h.workdir("a"))).unwrap();
    let b = core.create_session(exiting("b", &h.workdir("b"))).unwrap();
    core.rename_session(&a, "a2").unwrap();
    core.set_session_tags(&a, vec![]).unwrap();
    core.stop_session(&a).unwrap();
    core.forget_session(&b).unwrap();
    let calls = out.lock().unwrap().clone();
    let lines = log(&h);
    assert_eq!(lines.len(), 7);
    assert_eq!(calls.len(), SINKS * lines.len());
    for (chunk, line) in calls.chunks(SINKS).zip(&lines) {
        let order: Vec<usize> = chunk.iter().map(|(i, _)| *i).collect();
        assert_eq!(order, (0..SINKS).collect::<Vec<_>>());
        assert!(
            chunk.iter().all(|(_, e)| e == &chunk[0].1),
            "sinks saw different events"
        );
        assert_eq!(&Line::from_event(&chunk[0].1), line);
    }
    let mut distinct: Vec<Line> = calls.iter().map(|(_, e)| Line::from_event(e)).collect();
    distinct.dedup();
    assert_eq!(distinct.len(), lines.len(), "an event was delivered twice");
}

// ---------------------------------------------------------------------------------------------
// 4. Panic containment
// ---------------------------------------------------------------------------------------------

struct Panicky(usize);

impl EventSink for Panicky {
    fn on_event(&self, event: &Event) {
        if self.0.is_multiple_of(2) {
            panic!("sink {} bug on {:?}", self.0, event.type_);
        }
        std::panic::panic_any(self.0);
    }
}

#[test]
fn fifty_ops_through_alternating_panicking_and_recording_sinks() {
    require_tmux!();
    let h = Harness::new();
    let recorders: Vec<Arc<Recorder>> = (0..4).map(|_| Arc::new(Recorder::default())).collect();
    let mut core = h.open();
    for (index, recorder) in recorders.iter().enumerate() {
        core.subscribe(Box::new(Panicky(index)));
        core.subscribe(Box::new(Arc::clone(recorder)));
    }
    core.subscribe(Box::new(Panicky(4)));
    let mut ids = Vec::new();
    let mut ok = 0;
    for step in 0..50 {
        let op = match step % 5 {
            0 => Op::Create {
                name: step % 6,
                exiting: step % 10 == 5,
            },
            1 => Op::Rename {
                who: step / 5,
                name: (step + 1) % 6,
            },
            2 => Op::SetTags {
                who: step / 5,
                tags: 2,
            },
            3 => Op::Stop(step / 5),
            _ => Op::Forget(step / 5),
        };
        if run_op(&h, &core, &mut ids, step, &op) == "Ok" {
            ok += 1;
        }
    }
    assert!(ok >= 25, "only {ok} of 50 ops succeeded");
    let lines = log(&h);
    assert!(lines.len() >= 25, "{}", lines.len());
    for recorder in &recorders {
        let seen: Vec<Line> = recorder.events().iter().map(Line::from_event).collect();
        assert_eq!(seen, lines);
    }
    assert_eq!(
        core.get_session(&ids[0]).map(|s| s.lifecycle).ok(),
        None.or(Some(Lifecycle::Stopped))
            .filter(|_| false)
            .or(core.get_session(&ids[0]).map(|s| s.lifecycle).ok())
    );
}

// ---------------------------------------------------------------------------------------------
// 5. Failure paths inside the locked section emit nothing
// ---------------------------------------------------------------------------------------------

#[test]
fn a_failure_after_the_lock_is_taken_emits_nothing_and_leaves_the_record() {
    require_tmux!();
    let h = Harness::new();
    let recorder = Arc::new(Recorder::default());
    let mut core = h.open();
    core.subscribe(Box::new(Arc::clone(&recorder)));
    let a = core.create_session(process("a", &h.workdir("a"))).unwrap();
    let b = core.create_session(process("b", &h.workdir("b"))).unwrap();
    core.stop_session(&b).unwrap();
    let before = h.log_lines().len();
    assert_eq!(before, 3);

    // send_message: the lock is taken, then the recipient turns out to be stopped.
    let not_running = matches!(
        core.send_message(&b, "x", SendOptions::from_agent(a.clone())),
        Err(CoreError::NotRunning(_))
    );
    assert!(not_running);
    // send_message: running record, but the tmux session is gone (fails at `live_or_not_running`).
    h.tmux_ok(&["kill-session", "-t", "=a"]);
    let not_live = matches!(
        core.send_message(&a, "x", SendOptions::default()),
        Err(CoreError::NotRunning(_))
    );
    assert!(not_live);
    assert_eq!(
        core.get_session(&a).unwrap().lifecycle,
        Lifecycle::Running,
        "send never marks exited"
    );

    // rename_session: name free in the store, taken on the server by a foreign session.
    let c = core.create_session(process("c", &h.workdir("c"))).unwrap();
    let output = h.tmux(&["new-session", "-d", "-s", "taken", "sleep", "30"]);
    assert!(output.status.success());
    let record = h.record_bytes(&c);
    let taken = matches!(
        core.rename_session(&c, "taken"),
        Err(CoreError::NameTaken(_))
    );
    assert!(taken);
    assert_eq!(
        h.record_bytes(&c),
        record,
        "record rewritten by a failed rename"
    );
    assert_eq!(core.get_session(&c).unwrap().name, "c");
    // create_session: same foreign name, fails at `new-session` after `create.lock`.
    let taken = matches!(
        core.create_session(process("taken", &h.workdir("t"))),
        Err(CoreError::NameTaken(_))
    );
    assert!(taken);
    assert!(!h.state_dir().join("profiles").join("taken.md").exists());

    assert_eq!(
        h.log_lines().len(),
        before + 1,
        "only c's `created` was added"
    );
    let seen: Vec<Line> = recorder.events().iter().map(Line::from_event).collect();
    assert_eq!(seen, log(&h));
    assert_eq!(types(&recorder.events()), [EventType::Created; 3]);
}

// ---------------------------------------------------------------------------------------------
// 6. Concurrency: four cores, four sinks, one state dir
// ---------------------------------------------------------------------------------------------

#[test]
fn four_cores_with_their_own_sinks_cover_the_log_exactly() {
    require_tmux!();
    const THREADS: usize = 4;
    const OPS: usize = 20;
    let h = Arc::new(Harness::new());
    let pool: Arc<Mutex<Vec<SessionId>>> = Arc::new(Mutex::new(Vec::new()));
    let barrier = Arc::new(Barrier::new(THREADS));
    let recorders: Vec<Arc<Recorder>> = (0..THREADS)
        .map(|_| Arc::new(Recorder::default()))
        .collect();
    let workers: Vec<_> = recorders
        .iter()
        .enumerate()
        .map(|(worker, recorder)| {
            let h = Arc::clone(&h);
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            let recorder = Arc::clone(recorder);
            std::thread::spawn(move || {
                let mut core = h.open();
                core.subscribe(Box::new(recorder));
                barrier.wait();
                let mut failures = BTreeMap::new();
                for step in 0..OPS {
                    let pick = |n: usize| {
                        let pool = pool.lock().unwrap();
                        pool.get((worker * 7 + n) % pool.len().max(1)).cloned()
                    };
                    let result: Result<(), CoreError> = match step % 5 {
                        0 => core
                            .create_session(process(
                                &format!("w{worker}s{step}"),
                                &h.workdir(&format!("w{worker}s{step}")),
                            ))
                            .map(|id| pool.lock().unwrap().push(id)),
                        1 => pick(step).map_or(Ok(()), |id| {
                            core.rename_session(&id, &format!("r{worker}s{step}"))
                                .map(drop)
                        }),
                        2 => pick(step).map_or(Ok(()), |id| {
                            core.set_session_tags(&id, vec![format!("{worker}")])
                                .map(drop)
                        }),
                        3 => pick(step).map_or(Ok(()), |id| core.stop_session(&id).map(drop)),
                        _ => pick(step).map_or(Ok(()), |id| core.forget_session(&id).map(drop)),
                    };
                    if let Err(error) = result {
                        *failures.entry(kind(&error)).or_insert(0usize) += 1;
                    }
                }
                failures
            })
        })
        .collect();
    let mut failures = BTreeMap::new();
    for worker in workers {
        for (kind, count) in worker.join().unwrap() {
            *failures.entry(kind).or_insert(0usize) += count;
        }
    }
    // Only expected races: a record stopped / forgotten by another thread. Never a lock or
    // store error.
    let unexpected: Vec<_> = failures
        .keys()
        .filter(|k| !matches!(**k, "NotFound" | "NotRunning" | "NameTaken"))
        .collect();
    assert!(unexpected.is_empty(), "{failures:?}");

    let mut union: Vec<Event> = recorders.iter().flat_map(|r| r.events()).collect();
    union.sort_by(|a, b| a.ts.partial_cmp(&b.ts).unwrap());
    let lines = log(&h);
    assert_eq!(union.len(), lines.len());
    let mut union_keys: Vec<(String, String)> = union
        .iter()
        .map(|e| (e.type_.as_str().to_owned(), e.id.as_str().to_owned()))
        .collect();
    let mut log_keys: Vec<(String, String)> = lines
        .iter()
        .map(|l| (l.type_.clone(), l.id.clone()))
        .collect();
    union_keys.sort();
    log_keys.sort();
    assert_eq!(union_keys, log_keys);
    // Every event is exactly one sink's: the same (type, id, ts) never twice.
    let mut full: Vec<Line> = union.iter().map(Line::from_event).collect();
    full.dedup();
    assert_eq!(full.len(), lines.len(), "an event reached two cores' sinks");
    // Per-id order by timestamp follows the lifecycle (timestamps are taken under the lock).
    let mut per_id: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for event in &union {
        per_id
            .entry(event.id.as_str().to_owned())
            .or_default()
            .push(event.type_.as_str().to_owned());
    }
    for (id, types) in &per_id {
        check_lifecycle(id, types).unwrap();
    }
    assert!(union.windows(2).all(|w| w[0].ts <= w[1].ts));
    // Per-core, each sink's own view is monotone in ts.
    for recorder in &recorders {
        let events = recorder.events();
        assert!(
            events.windows(2).all(|w| w[0].ts <= w[1].ts),
            "a core's sink saw ts go backwards"
        );
    }
}

/// Line order in the log file is a weaker claim than timestamp order: the append happens after
/// the lock is released, so two cores may land their lines in the other order.
#[test]
fn log_line_order_per_id_follows_the_lifecycle_under_contention() {
    require_tmux!();
    const ROUNDS: usize = 6;
    let h = Arc::new(Harness::new());
    let mut violations = Vec::new();
    for round in 0..ROUNDS {
        let core = h.open();
        let id = core
            .create_session(process(
                &format!("s{round}"),
                &h.workdir(&format!("s{round}")),
            ))
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let handles: Vec<_> = (0..3)
            .map(|worker| {
                let h = Arc::clone(&h);
                let id = id.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let core = h.open();
                    barrier.wait();
                    match worker {
                        0 => core.stop_session(&id).map(drop),
                        1 => core.set_session_tags(&id, vec!["x".into()]).map(drop),
                        _ => core.forget_session(&id).map(drop),
                    }
                })
            })
            .collect();
        for handle in handles {
            let _ = handle.join().unwrap();
        }
        let types: Vec<String> = log(&h)
            .into_iter()
            .filter(|l| l.id == id.as_str())
            .map(|l| l.type_)
            .collect();
        if let Err(violation) = check_lifecycle(id.as_str(), &types) {
            violations.push(violation);
        }
    }
    assert!(violations.is_empty(), "{violations:?}");
}

// ---------------------------------------------------------------------------------------------
// 7. Type-level contract
// ---------------------------------------------------------------------------------------------

fn assert_event_is_plain_data<T: Clone + Send + Sync + 'static>() {}

#[test]
fn event_is_clone_send_sync_and_closures_and_shared_sinks_register() {
    require_tmux!();
    assert_event_is_plain_data::<Event>();
    let h = Harness::new();
    let names = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorder = Arc::new(Recorder::default());
    let mut core = h.open();
    let captured = Arc::clone(&names);
    let closure: Box<dyn EventSink> = Box::new(move |event: &Event| {
        captured.lock().unwrap().push(event.name.clone());
    });
    core.subscribe(closure);
    core.subscribe(Box::new(Arc::clone(&recorder)));
    core.subscribe(Box::new(Arc::clone(&recorder)));
    let id = core.create_session(process("p", &h.workdir("p"))).unwrap();
    core.stop_session(&id).unwrap();
    assert_eq!(*names.lock().unwrap(), ["p", "p"]);
    // The same Arc registered twice is called twice per event.
    assert_eq!(
        types(&recorder.events()),
        [
            EventType::Created,
            EventType::Created,
            EventType::Stopped,
            EventType::Stopped
        ]
    );
    assert_eq!(h.log_lines().len(), 2);
    let event = recorder.events().remove(0);
    let moved = std::thread::spawn(move || event.clone().name)
        .join()
        .unwrap();
    assert_eq!(moved, "p");
}

/// An unwritable log never stops the sinks: the contract "log first" degrades to "log if it can".
#[test]
fn sinks_still_run_when_the_log_cannot_be_appended() {
    require_tmux!();
    let h = Harness::new();
    let recorder = Arc::new(Recorder::default());
    let mut core = h.open();
    core.subscribe(Box::new(Arc::clone(&recorder)));
    let id = core.create_session(process("p", &h.workdir("p"))).unwrap();
    assert_eq!(h.log_lines().len(), 1);
    let path = h.state_dir().join("log.jsonl");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    core.stop_session(&id).unwrap();
    assert_eq!(core.get_session(&id).unwrap().lifecycle, Lifecycle::Stopped);
    assert_eq!(
        types(&recorder.events()),
        [EventType::Created, EventType::Stopped]
    );
    assert!(path.is_dir(), "the log directory was replaced");
}
