//! Adversarial / property tests for `TxCore` against a private tmux server: a model-based random
//! operation sequence, record integrity on every error path, terminal states, `resolve` edge
//! cases, odd tags / parents / env values, concurrent cores, archive-then-create, server death.
//!
//! Tests marked `#[ignore = "BUG: …"]` document real failures; they stay in the file so the
//! reproduction survives, and the suite stays green.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use common::{Harness, agent, cwd_of, typed, wait_for};
use proptest::prelude::*;
use tx_core::{
    CoreError, EngineKind, Lifecycle, ListFilter, RefreshResult, SendOptions, SessionId, TxCore,
};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// The variant name only: `prop_assert!` messages are format strings, so no `{}` from `Debug`.
fn kind(error: &CoreError) -> &'static str {
    match error {
        CoreError::NotFound(_) => "NotFound",
        CoreError::NameTaken(_) => "NameTaken",
        CoreError::InvalidName(_) => "InvalidName",
        CoreError::InvalidEnvKey(_) => "InvalidEnvKey",
        CoreError::NotADirectory(_) => "NotADirectory",
        CoreError::NotRunning(_) => "NotRunning",
        CoreError::EngineDisabled(_) => "EngineDisabled",
        CoreError::EngineTaken(_) => "EngineTaken",
        CoreError::SocketPathTooLong(_) => "SocketPathTooLong",
        CoreError::Profile(_) => "Profile",
        CoreError::Engine(_) => "Engine",
        CoreError::Store(_) => "Store",
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

fn sorted_live(h: &Harness) -> Vec<String> {
    let mut names = h.live_names();
    names.sort();
    names
}

fn log_types(h: &Harness) -> Vec<String> {
    h.log_lines()
        .iter()
        .map(|l| l["type"].as_str().unwrap().to_owned())
        .collect()
}

fn env_text(cwd: &std::path::Path) -> String {
    assert!(wait_for(Duration::from_secs(5), || cwd
        .join("env.txt")
        .exists()));
    std::fs::read_to_string(cwd.join("env.txt")).unwrap()
}

// ---------------------------------------------------------------------------------------------
// 1–3. Model-based random operation sequences
// ---------------------------------------------------------------------------------------------

const NAMES: [&str; 11] = [
    "alpha", "beta", "gamma", "delta", "eps", "zeta", "eta", "theta", "bad name", "", "x-long",
];
const VALID_NAMES: usize = 8;
const TAGS: [&str; 4] = ["t1", "t2", "", "ünï"];

fn name_at(i: usize) -> String {
    if i == NAMES.len() - 1 {
        "x".repeat(129)
    } else {
        NAMES[i].to_owned()
    }
}

#[derive(Clone, Debug)]
enum Op {
    Create {
        name: usize,
        tags: Vec<usize>,
        parent: Option<usize>,
    },
    Rename {
        who: usize,
        name: usize,
    },
    SetTags {
        who: usize,
        tags: Vec<usize>,
    },
    Stop(usize),
    Archive(usize),
    Refresh(usize),
    RefreshAll,
    /// `kill-session -t =<name>` behind core's back.
    Kill(usize),
    Send {
        to: usize,
        from: Option<usize>,
    },
}

fn arb_op() -> impl Strategy<Value = Op> {
    let who = 0..10usize;
    let tags = prop::collection::vec(0..TAGS.len(), 0..3);
    prop_oneof![
        5 => (0..NAMES.len(), tags.clone(), prop::option::of(0..10usize))
            .prop_map(|(name, tags, parent)| Op::Create { name, tags, parent }),
        2 => (who.clone(), 0..NAMES.len()).prop_map(|(who, name)| Op::Rename { who, name }),
        2 => (who.clone(), tags).prop_map(|(who, tags)| Op::SetTags { who, tags }),
        2 => who.clone().prop_map(Op::Stop),
        2 => who.clone().prop_map(Op::Archive),
        2 => who.clone().prop_map(Op::Refresh),
        1 => Just(Op::RefreshAll),
        2 => (0..VALID_NAMES).prop_map(Op::Kill),
        1 => (who.clone(), prop::option::of(who)).prop_map(|(to, from)| Op::Send { to, from }),
    ]
}

#[derive(Clone, Debug)]
struct Rec {
    name: String,
    lifecycle: Lifecycle,
    tags: Vec<String>,
    parent: Option<SessionId>,
    /// First observed `ended_at`; must never change afterwards.
    ended_at: Option<f64>,
    /// The tmux session was killed behind core's back; the record still says `Running`.
    killed: bool,
    /// Archived: hidden from the default listing, its name free; never deleted.
    archived: bool,
}

impl Rec {
    fn live(&self) -> bool {
        self.lifecycle == Lifecycle::Running && !self.killed
    }
}

#[derive(Default)]
struct Model {
    /// Every id ever minted, in creation order.
    all_ids: Vec<SessionId>,
    recs: BTreeMap<SessionId, Rec>,
    log_len: usize,
}

impl Model {
    fn id_for(&self, who: usize) -> SessionId {
        if who >= 8 || self.all_ids.is_empty() {
            format!("ghost-{who}").parse().unwrap()
        } else {
            self.all_ids[who % self.all_ids.len()].clone()
        }
    }

    /// Whether an unarchived record other than `except`, running or not, holds `name`.
    fn name_held(&self, name: &str, except: Option<&SessionId>) -> bool {
        self.recs
            .iter()
            .any(|(id, rec)| !rec.archived && rec.name == name && Some(id) != except)
    }

    /// The name a create asked `base` gets: the first of `base`, `base-2`, … no unarchived
    /// record holds.
    fn free_name(&self, base: &str) -> String {
        (1..)
            .map(|suffix| {
                if suffix == 1 {
                    base.to_owned()
                } else {
                    format!("{base}-{suffix}")
                }
            })
            .find(|candidate| !self.name_held(candidate, None))
            .unwrap()
    }

    /// The unarchived (`archived: false`) or archived records' ids passing `keep`, sorted.
    fn listed(&self, archived: bool, keep: impl Fn(&Rec) -> bool) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self
            .recs
            .iter()
            .filter(|(_, rec)| rec.archived == archived && keep(rec))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    fn expected_live(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .recs
            .values()
            .filter(|rec| rec.live())
            .map(|rec| rec.name.clone())
            .collect();
        names.sort();
        names
    }

    /// `resolve(name)`: the `Running` record, else the most recently created unarchived one,
    /// else the most recently created archived one.
    fn expected_resolve(&self, name: &str) -> Option<SessionId> {
        let named: Vec<&SessionId> = self
            .all_ids
            .iter()
            .filter(|id| self.recs.get(*id).is_some_and(|rec| rec.name == name))
            .collect();
        named
            .iter()
            .find(|id| self.recs[*id].lifecycle == Lifecycle::Running)
            .or(named.iter().rev().find(|id| !self.recs[*id].archived))
            .or(named.last())
            .map(|id| (*id).clone())
    }
}

fn snapshot(h: &Harness, model: &Model) -> BTreeMap<SessionId, Vec<u8>> {
    model
        .recs
        .keys()
        .map(|id| (id.clone(), h.record_bytes(id)))
        .collect()
}

/// Every stored fact equals the model: lists, filters, fields, live names, resolve, log length.
fn check_model(
    h: &Harness,
    core: &TxCore,
    model: &mut Model,
    step: usize,
) -> Result<(), TestCaseError> {
    let sorted = |mut ids: Vec<SessionId>| {
        ids.sort();
        ids
    };
    let listed = sorted(core.list_sessions(&ListFilter::default()).unwrap());
    prop_assert_eq!(listed, model.listed(false, |_| true), "step {}: ids", step);
    let archived = sorted(core.list_sessions(&ListFilter::new().archived()).unwrap());
    prop_assert_eq!(
        archived,
        model.listed(true, |_| true),
        "step {}: archived ids",
        step
    );

    let running = sorted(
        core.list_sessions(&ListFilter::new().lifecycle(Lifecycle::Running))
            .unwrap(),
    );
    prop_assert_eq!(
        running,
        model.listed(false, |rec| rec.lifecycle == Lifecycle::Running),
        "step {}: running filter",
        step
    );

    let tagged = sorted(core.list_sessions(&ListFilter::new().tag("t1")).unwrap());
    prop_assert_eq!(
        tagged,
        model.listed(false, |rec| rec.tags.iter().any(|t| t == "t1")),
        "step {}: tag filter",
        step
    );
    let tagged_archived = sorted(
        core.list_sessions(&ListFilter::new().tag("t1").archived())
            .unwrap(),
    );
    prop_assert_eq!(
        tagged_archived,
        model.listed(true, |rec| rec.tags.iter().any(|t| t == "t1")),
        "step {}: archived tag filter",
        step
    );

    if let Some(first) = model.all_ids.first() {
        let children = sorted(
            core.list_sessions(&ListFilter::new().parent(first.clone()))
                .unwrap(),
        );
        prop_assert_eq!(
            children,
            model.listed(false, |rec| rec.parent.as_ref() == Some(first)),
            "step {}: parent filter",
            step
        );
    }

    for (id, rec) in &mut model.recs {
        let session = core.get_session(id).unwrap();
        prop_assert_eq!(&session.name, &rec.name, "step {}: name of {}", step, id);
        prop_assert_eq!(
            session.lifecycle,
            rec.lifecycle,
            "step {}: lifecycle of {}",
            step,
            id
        );
        prop_assert_eq!(&session.tags, &rec.tags, "step {}: tags of {}", step, id);
        prop_assert_eq!(
            session.archived_at.is_some(),
            rec.archived,
            "step {}: archived_at presence of {}",
            step,
            id
        );
        prop_assert_eq!(
            &session.parent,
            &rec.parent,
            "step {}: parent of {}",
            step,
            id
        );
        prop_assert_eq!(
            session.ended_at.is_some(),
            rec.lifecycle.is_terminal(),
            "step {}: ended_at presence of {}",
            step,
            id
        );
        match rec.ended_at {
            None => rec.ended_at = session.ended_at,
            Some(at) => prop_assert_eq!(
                session.ended_at,
                Some(at),
                "step {}: ended_at changed",
                step
            ),
        }
    }

    prop_assert_eq!(
        sorted_live(h),
        model.expected_live(),
        "step {}: live names",
        step
    );

    let names: BTreeSet<String> = model.recs.values().map(|rec| rec.name.clone()).collect();
    for name in names {
        let resolved = core.resolve(&name).ok();
        prop_assert_eq!(
            resolved,
            model.expected_resolve(&name),
            "step {}: resolve {}",
            step,
            name
        );
    }
    prop_assert_eq!(
        h.log_lines().len(),
        model.log_len,
        "step {}: log length",
        step
    );
    Ok(())
}

fn run_ops(h: &Harness, core: &TxCore, ops: &[Op]) -> Result<(), TestCaseError> {
    let mut model = Model::default();
    let repository = h.repository("w");
    for (step, op) in ops.iter().enumerate() {
        let before = snapshot(h, &model);
        // Ids whose record may legitimately change in this step.
        let mut touched: BTreeSet<SessionId> = BTreeSet::new();
        match op {
            Op::Create { name, tags, parent } => {
                let name = name_at(*name);
                let mut opts = agent(&name, &repository);
                opts.tags = tags.iter().map(|t| TAGS[*t].to_owned()).collect();
                opts.parent = parent.map(|p| model.id_for(p));
                let valid = !name.is_empty() && name.len() <= 128 && !name.contains(' ');
                // A held name is never refused: the agent gets the next free one.
                let expected = if valid { "Ok" } else { "InvalidName" };
                let result = core.create_session(opts.clone());
                prop_assert_eq!(verdict(&result), expected, "step {}: create {}", step, name);
                if let Ok(id) = result {
                    let name = model.free_name(&name);
                    prop_assert_eq!(
                        &core.get_session(&id).unwrap().name,
                        &name,
                        "step {}: the name given",
                        step
                    );
                    model.all_ids.push(id.clone());
                    model.recs.insert(
                        id,
                        Rec {
                            name,
                            lifecycle: Lifecycle::Running,
                            tags: opts.tags,
                            parent: opts.parent,
                            ended_at: None,
                            killed: false,
                            archived: false,
                        },
                    );
                    model.log_len += 1;
                }
            }
            Op::Rename { who, name } => {
                let id = model.id_for(*who);
                let name = name_at(*name);
                let valid = !name.is_empty() && name.len() <= 128 && !name.contains(' ');
                let expected = if !valid {
                    "InvalidName"
                } else if let Some(rec) = model.recs.get(&id) {
                    if rec.name == name {
                        "Ok"
                    } else if model.name_held(&name, Some(&id)) {
                        "NameTaken"
                    } else {
                        "Ok"
                    }
                } else {
                    "NotFound"
                };
                let result = core.rename_session(&id, &name);
                prop_assert_eq!(
                    verdict(&result),
                    expected,
                    "step {}: rename {} -> {}",
                    step,
                    id,
                    name
                );
                if let Ok(session) = result {
                    let rec = model.recs.get_mut(&id).unwrap();
                    if rec.name != name {
                        rec.name = name;
                        model.log_len += 1;
                        touched.insert(id.clone());
                    }
                    prop_assert_eq!(&session.name, &rec.name);
                }
            }
            Op::SetTags { who, tags } => {
                let id = model.id_for(*who);
                let tags: Vec<String> = tags.iter().map(|t| TAGS[*t].to_owned()).collect();
                let expected = if model.recs.contains_key(&id) {
                    "Ok"
                } else {
                    "NotFound"
                };
                let result = core.set_session_tags(&id, tags.clone());
                prop_assert_eq!(verdict(&result), expected, "step {}: tags of {}", step, id);
                if let Ok(session) = result {
                    prop_assert_eq!(&session.tags, &tags);
                    model.recs.get_mut(&id).unwrap().tags = tags;
                    model.log_len += 1;
                    touched.insert(id);
                }
            }
            Op::Stop(who) => {
                let id = model.id_for(*who);
                let expected = match model.recs.get(&id) {
                    None => "NotFound",
                    Some(rec) if rec.lifecycle.is_terminal() => "NotRunning",
                    Some(_) => "Ok",
                };
                let result = core.stop_session(&id);
                prop_assert_eq!(verdict(&result), expected, "step {}: stop {}", step, id);
                if let Ok(session) = result {
                    let rec = model.recs.get_mut(&id).unwrap();
                    let wanted = if rec.killed {
                        Lifecycle::Exited
                    } else {
                        Lifecycle::Stopped
                    };
                    prop_assert_eq!(
                        session.lifecycle,
                        wanted,
                        "step {}: stop of a killed session",
                        step
                    );
                    prop_assert!(session.ended_at.is_some());
                    rec.lifecycle = wanted;
                    rec.killed = false;
                    model.log_len += 1;
                    touched.insert(id);
                }
            }
            Op::Archive(who) => {
                let id = model.id_for(*who);
                let expected = if model.recs.contains_key(&id) {
                    "Ok"
                } else {
                    "NotFound"
                };
                let result = core.archive_session(&id);
                prop_assert_eq!(verdict(&result), expected, "step {}: archive {}", step, id);
                if let Ok(session) = result {
                    let rec = model.recs.get_mut(&id).unwrap();
                    if !rec.archived {
                        // A running one is stopped first: `Stopped`, or `Exited` when its
                        // tmux session was already gone.
                        if !rec.lifecycle.is_terminal() {
                            rec.lifecycle = if rec.killed {
                                Lifecycle::Exited
                            } else {
                                Lifecycle::Stopped
                            };
                            rec.killed = false;
                            model.log_len += 1;
                        }
                        rec.archived = true;
                        model.log_len += 1;
                        touched.insert(id);
                    }
                    prop_assert_eq!(session.lifecycle, rec.lifecycle, "step {}: archive", step);
                    prop_assert!(session.archived_at.is_some());
                }
            }
            Op::Refresh(who) => {
                let id = model.id_for(*who);
                let result = core.refresh_session(&id);
                match model.recs.get_mut(&id) {
                    None => prop_assert_eq!(
                        verdict(&result),
                        "NotFound",
                        "step {}: refresh ghost",
                        step
                    ),
                    Some(rec) => {
                        let result = result.unwrap();
                        let exited = matches!(result, RefreshResult::Exited(_));
                        prop_assert_eq!(exited, rec.killed, "step {}: refresh {}", step, id);
                        prop_assert_eq!(
                            result.session().lifecycle,
                            if rec.killed {
                                Lifecycle::Exited
                            } else {
                                rec.lifecycle
                            }
                        );
                        if rec.killed {
                            rec.lifecycle = Lifecycle::Exited;
                            rec.killed = false;
                            model.log_len += 1;
                            touched.insert(id);
                        }
                    }
                }
            }
            Op::RefreshAll => {
                let changed = core.refresh_all().unwrap();
                let mut got: Vec<SessionId> = changed.iter().map(|s| s.id.clone()).collect();
                got.sort();
                let mut expected: Vec<SessionId> = model
                    .recs
                    .iter()
                    .filter(|(_, rec)| rec.killed)
                    .map(|(id, _)| id.clone())
                    .collect();
                expected.sort();
                prop_assert_eq!(
                    got,
                    expected.clone(),
                    "step {}: refresh_all changed set",
                    step
                );
                let all_exited = changed.iter().all(|s| s.lifecycle == Lifecycle::Exited);
                prop_assert!(
                    all_exited,
                    "step {}: refresh_all returned a non-Exited record",
                    step
                );
                for id in expected {
                    let rec = model.recs.get_mut(&id).unwrap();
                    rec.lifecycle = Lifecycle::Exited;
                    rec.killed = false;
                    model.log_len += 1;
                    touched.insert(id);
                }
            }
            Op::Kill(name) => {
                let name = NAMES[*name];
                let _ = h.tmux(&["kill-session", "-t", &format!("={name}")]);
                if let Some(rec) = model
                    .recs
                    .values_mut()
                    .find(|rec| rec.live() && rec.name == name)
                {
                    rec.killed = true;
                }
            }
            Op::Send { to, from } => {
                let to = model.id_for(*to);
                let from = from.map(|f| model.id_for(f));
                let expected = if from.as_ref().is_some_and(|f| !model.recs.contains_key(f)) {
                    "NotFound"
                } else {
                    match model.recs.get(&to) {
                        None => "NotFound",
                        Some(rec) if rec.live() => "Ok",
                        Some(_) => "NotRunning",
                    }
                };
                let result = core.send_message(
                    &to,
                    &format!("m{step}"),
                    match from {
                        Some(from) => SendOptions::from_agent(from),
                        None => SendOptions::default(),
                    },
                );
                prop_assert_eq!(verdict(&result), expected, "step {}: send to {}", step, to);
                if result.is_ok() {
                    model.log_len += 1;
                }
            }
        }
        // Record integrity: nothing but the touched records changed on disk.
        for (id, bytes) in &before {
            if touched.contains(id) {
                continue;
            }
            if let Some(now) = model.recs.contains_key(id).then(|| h.record_bytes(id)) {
                prop_assert_eq!(
                    &now,
                    bytes,
                    "step {}: untouched record {} was rewritten",
                    step,
                    id
                );
            }
        }
        check_model(h, core, &mut model, step)?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 12,
        max_shrink_iters: 16,
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn random_op_sequences_match_the_model(ops in prop::collection::vec(arb_op(), 20..40)) {
        if !common::tmux_available() {
            return Ok(());
        }
        let h = Harness::new();
        let core = h.open_with_engine();
        run_ops(&h, &core, &ops)?;
    }
}

/// A fixed sequence hitting the sharp corners the random walk may miss.
#[test]
fn scripted_corner_sequence_matches_the_model() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let alpha = || Op::Create {
        name: 0,
        tags: vec![],
        parent: None,
    };
    let ops = [
        Op::Create {
            name: 0,
            tags: vec![0],
            parent: None,
        },
        Op::Create {
            name: 1,
            tags: vec![],
            parent: Some(0),
        },
        Op::Kill(0),
        alpha(),                        // alpha-2: killed but still Running, alpha is held
        Op::Rename { who: 1, name: 0 }, // NameTaken: record of alpha is Running
        Op::Stop(0),                    // Exited, not Stopped
        alpha(),                        // alpha-3: an ended, unarchived record holds alpha
        Op::Archive(0),                 // already ended: archived only
        alpha(),                        // alpha: the first holder is archived
        Op::Archive(2),                 // a live one: stopped, then archived; no orphan
        alpha(),                        // alpha-2 again: its holder is archived
        Op::Rename { who: 1, name: 0 }, // NameTaken by the running alpha
        Op::Kill(1),
        Op::Rename { who: 1, name: 2 }, // killed record: record-only rename, Ok
        Op::RefreshAll,
        Op::Refresh(1),
        Op::Stop(1),                    // NotRunning
        Op::Send { to: 1, from: None }, // NotRunning
        Op::Send {
            to: 0,
            from: Some(9),
        }, // NotFound (sender)
        Op::Archive(0),                 // already archived: unchanged, no event
        Op::Archive(9),                 // NotFound
        Op::Kill(0),                    // the running alpha
        Op::Archive(4),                 // killed: Exited, then archived
        alpha(),                        // alpha: every record named alpha is archived
        Op::Rename { who: 0, name: 2 }, // NameTaken: an archived record obeys the name rule
        Op::Archive(1),
        Op::Rename { who: 0, name: 2 }, // Ok: gamma's holder is archived too
    ];
    run_ops(&h, &core, &ops).unwrap();
}

// ---------------------------------------------------------------------------------------------
// 4. resolve edge cases
// ---------------------------------------------------------------------------------------------

#[test]
fn resolve_prefers_an_id_over_a_name_equal_to_it() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("a");
    let a = core.create_session(agent("a", &repository)).unwrap();
    // A uuid is a valid name; a session *named* like another session's id.
    let b = core.create_session(agent(a.as_str(), &repository)).unwrap();
    assert_ne!(a, b);
    assert_eq!(
        core.resolve(a.as_str()).unwrap(),
        a,
        "id wins over an equal name"
    );
    assert_eq!(core.resolve(b.as_str()).unwrap(), b);
    assert_eq!(core.resolve("a").unwrap(), a);
    // Archived, the id record still exists, and the id still wins.
    core.archive_session(&a).unwrap();
    assert_eq!(
        core.resolve(a.as_str()).unwrap(),
        a,
        "an archived id still resolves as an id"
    );
    // Once the id record is gone (deleted by hand: core never deletes one), the name surfaces.
    std::fs::remove_file(h.data_dir().join(format!("sessions/{a}.json"))).unwrap();
    assert_eq!(
        core.resolve(a.as_str()).unwrap(),
        b,
        "a missing id falls through to the name"
    );
    // Even when the named record is stopped, the id-looking name still resolves to it.
    core.stop_session(&b).unwrap();
    assert_eq!(core.resolve(a.as_str()).unwrap(), b);
}

#[test]
fn resolve_three_records_one_name() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("a");
    let first = core.create_session(agent("n", &repository)).unwrap();
    // An ended, unarchived record keeps its name: only archiving frees it.
    core.stop_session(&first).unwrap();
    let held = core.create_session(agent("n", &repository)).unwrap();
    assert_eq!(core.get_session(&held).unwrap().name, "n-2");
    core.archive_session(&held).unwrap();
    core.archive_session(&first).unwrap();
    let second = core.create_session(agent("n", &repository)).unwrap();
    assert_eq!(core.get_session(&second).unwrap().name, "n");
    core.archive_session(&second).unwrap();
    let third = core.create_session(agent("n", &repository)).unwrap();
    assert_eq!(core.resolve("n").unwrap(), third);
    // Kill behind the back: the record is still Running and still wins.
    h.tmux_ok(&["kill-session", "-t", "=n"]);
    assert_eq!(core.resolve("n").unwrap(), third);
    core.stop_session(&third).unwrap();
    assert_eq!(
        core.get_session(&third).unwrap().lifecycle,
        Lifecycle::Exited
    );
    assert_eq!(
        core.resolve("n").unwrap(),
        third,
        "the unarchived one, though ended, beats the archived ones"
    );
    core.archive_session(&third).unwrap();
    assert_eq!(
        core.resolve("n").unwrap(),
        third,
        "most recently created among the archived"
    );
    // Rename an older archived record away and back: still the most recent *created* wins.
    core.rename_session(&first, "m").unwrap();
    core.rename_session(&first, "n").unwrap();
    assert_eq!(core.resolve("n").unwrap(), third);
    // An older unarchived record beats a newer archived one.
    let older = core.create_session(agent("o", &repository)).unwrap();
    let newer = core.create_session(agent("p", &repository)).unwrap();
    core.archive_session(&newer).unwrap();
    core.rename_session(&older, "p").unwrap();
    assert_eq!(core.resolve("p").unwrap(), older);
}

// ---------------------------------------------------------------------------------------------
// 5. parent / tags are stored verbatim, never validated
// ---------------------------------------------------------------------------------------------

#[test]
fn parent_is_never_validated_and_tags_are_verbatim() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let ghost: SessionId = "no-such-parent".parse().unwrap();
    let mut opts = agent("p", &h.repository("a"));
    opts.parent = Some(ghost.clone());
    let big = "T".repeat(10 * 1024);
    opts.tags = vec![
        String::new(),
        "dup".into(),
        "dup".into(),
        "ünïcödé 😀".into(),
        "with\nnewline".into(),
        "\"quoted\"".into(),
        big.clone(),
    ];
    let id = core.create_session(opts.clone()).unwrap();
    let session = core.get_session(&id).unwrap();
    assert_eq!(
        session.parent,
        Some(ghost.clone()),
        "dangling parent accepted verbatim"
    );
    assert_eq!(
        session.tags, opts.tags,
        "tags kept verbatim: empty, duplicate, unicode, 10 kB"
    );
    let by_parent = core
        .list_sessions(&ListFilter::new().parent(ghost))
        .unwrap();
    assert_eq!(by_parent, std::slice::from_ref(&id));
    for tag in ["", "dup", "ünïcödé 😀", "with\nnewline", big.as_str()] {
        let found = core.list_sessions(&ListFilter::new().tag(tag)).unwrap();
        assert_eq!(found, std::slice::from_ref(&id), "tag {tag:?}");
    }
    // A session may even claim itself as parent via set_tags? No API; but rename/tags keep parent.
    let session = core
        .set_session_tags(&id, vec![big.clone(), big.clone()])
        .unwrap();
    assert_eq!(session.tags, [big.clone(), big]);
    // The log line is bounded even though the tag text is not.
    let last = h.log_lines().pop().unwrap();
    assert_eq!(last["type"], "tagged");
    assert!(
        last["msg"].as_str().unwrap().len() <= 600,
        "log msg is trimmed"
    );
    // Re-reading after the reopen path works with the huge record.
    let again = h.open();
    assert_eq!(again.get_session(&id).unwrap().tags.len(), 2);
}

// ---------------------------------------------------------------------------------------------
// 6. Env edge cases reach the process
// ---------------------------------------------------------------------------------------------

#[test]
fn env_values_with_equals_spaces_newline_and_8kb_reach_the_process() {
    require_tmux!();
    let h = Harness::new();
    let mut config = h.config();
    config.env = vec![
        ("FROM_PLAN".into(), "config".into()),
        ("CFG".into(), "config".into()),
    ];
    let mut core = TxCore::open(config).unwrap();
    core.register_engine(Box::new(common::ShellStub)).unwrap();
    let big = "v".repeat(8 * 1024);
    let mut opts = agent("e", &h.repository("a"));
    opts.env = vec![
        ("EQ".into(), "a=b=c".into()),
        ("SP".into(), "  two  words  ".into()),
        ("NL".into(), "line1\nline2".into()),
        ("BIG".into(), big.clone()),
        ("EMPTY".into(), String::new()),
        ("UNI".into(), "żółć 😀".into()),
        ("TX_SESSION_ID".into(), "override".into()),
        ("CFG".into(), "opts".into()),
    ];
    let id = core.create_session(opts).unwrap();
    let text = env_text(&cwd_of(&core, &id));
    assert!(text.contains("\nEQ=a=b=c\n"), "{text}");
    assert!(text.contains("\nSP=  two  words  \n"), "{text}");
    assert!(
        text.contains("\nNL=line1\nline2\n"),
        "newline value: {text}"
    );
    assert!(text.contains(&format!("\nBIG={big}\n")), "8 kB value");
    assert!(text.contains("\nEMPTY=\n"), "{text}");
    assert!(text.contains("\nUNI=żółć 😀\n"), "{text}");
    assert!(
        text.contains("\nTX_SESSION_ID=override\n"),
        "opts.env overrides TX_SESSION_ID"
    );
    assert!(
        !text.contains(&format!("TX_SESSION_ID={id}")),
        "the real id is not in the env any more"
    );
    assert!(
        text.contains("\nFROM_PLAN=plan\n"),
        "plan env beats config env"
    );
    assert!(text.contains("\nCFG=opts\n"), "option env beats config env");
    // Core's bookkeeping does not depend on the env: @tx-id still links the session.
    assert!(core.get_execution_location(&id).is_ok());
    assert!(matches!(
        core.refresh_session(&id).unwrap(),
        RefreshResult::Unchanged(_)
    ));
}

#[test]
fn env_keys_rejected_before_any_state() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("a");
    for key in ["", "A=B", "A B", "A\nB", "A\0", "ä", "A$", "A/B"] {
        let mut opts = agent("k", &repository);
        opts.env = vec![(key.into(), "1".into())];
        let error = core.create_session(opts).unwrap_err();
        assert_eq!(kind(&error), "InvalidEnvKey", "{key:?}");
    }
    let mut opts = agent("k", &repository);
    opts.env = vec![("A".into(), "x\0y".into())];
    assert_eq!(
        kind(&core.create_session(opts).unwrap_err()),
        "InvalidEnvKey"
    );
    assert!(h.live_names().is_empty());
    assert!(h.log_lines().is_empty());
    assert!(
        core.list_sessions(&ListFilter::default())
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------------------------
// 7. Two cores on one state dir, concurrent creates
// ---------------------------------------------------------------------------------------------

#[test]
fn eight_cores_create_distinct_names_in_parallel() {
    require_tmux!();
    let h = Harness::new();
    h.open();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let barrier = Arc::clone(&barrier);
            let config = h.config();
            let repository = h.repository(&format!("w{i}"));
            std::thread::spawn(move || {
                let mut core = TxCore::open(config).unwrap();
                core.register_engine(Box::new(common::ShellStub)).unwrap();
                barrier.wait();
                core.create_session(agent(&format!("par-{i}"), &repository))
            })
        })
        .collect();
    let mut ids = Vec::new();
    for handle in handles {
        ids.push(handle.join().unwrap().unwrap());
    }
    let unique: BTreeSet<&SessionId> = ids.iter().collect();
    assert_eq!(unique.len(), 8);
    let live = sorted_live(&h);
    let expected: Vec<String> = (0..8).map(|i| format!("par-{i}")).collect();
    assert_eq!(live, expected);
    let core = h.open();
    let mut listed = core.list_sessions(&ListFilter::default()).unwrap();
    listed.sort();
    ids.sort();
    assert_eq!(listed, ids);
    assert_eq!(log_types(&h), vec!["created"; 8]);
    let leftovers: Vec<String> = std::fs::read_dir(h.data_dir().join("sessions"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".json") || n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert!(h.state_dir().join("locks/create.lock").exists());
    // Every record is readable by every core and all eight are live by @tx-id.
    for id in &ids {
        assert!(core.get_execution_location(id).is_ok());
    }
    assert!(core.refresh_all().unwrap().is_empty());
}

// ---------------------------------------------------------------------------------------------
// 8. archive then create / refresh
// ---------------------------------------------------------------------------------------------

#[test]
fn archive_of_a_live_session_stops_it_and_frees_its_name() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(agent("f", &h.repository("a"))).unwrap();
    let archived = core.archive_session(&id).unwrap();
    assert_eq!(archived.lifecycle, Lifecycle::Stopped);
    assert!(
        h.live_names().is_empty(),
        "stopped first: no orphan in tmux"
    );
    let again = core.create_session(agent("f", &h.repository("b"))).unwrap();
    assert_ne!(again, id);
    assert_eq!(
        core.list_sessions(&ListFilter::default()).unwrap(),
        std::slice::from_ref(&again)
    );
    assert_eq!(
        core.list_sessions(&ListFilter::new().archived()).unwrap(),
        std::slice::from_ref(&id)
    );
    assert!(
        core.refresh_all().unwrap().is_empty(),
        "an archived record has nothing to refresh"
    );
    assert!(matches!(
        core.refresh_session(&id).unwrap(),
        RefreshResult::Unchanged(_)
    ));
    assert_eq!(
        kind(&core.get_execution_location(&id).unwrap_err()),
        "NotRunning"
    );
    assert_eq!(
        kind(
            &core
                .send_message(&id, "x", SendOptions::default())
                .unwrap_err()
        ),
        "NotRunning"
    );
    assert_eq!(kind(&core.stop_session(&id).unwrap_err()), "NotRunning");
    // An archived record still obeys the name rule, and may take a free name.
    assert_eq!(
        core.rename_session(&id, "f").unwrap(),
        archived,
        "renaming to its own name is a no-op"
    );
    assert_eq!(core.rename_session(&id, "g").unwrap().name, "g");
    assert_eq!(
        kind(&core.rename_session(&id, "f").unwrap_err()),
        "NameTaken"
    );
    assert!(
        h.state_dir()
            .join("profiles")
            .join(format!("{id}.md"))
            .is_file(),
        "nothing is deleted"
    );
    assert_eq!(h.live_names(), ["f"]);
    assert_eq!(
        log_types(&h),
        ["created", "stopped", "archived", "created", "renamed"]
    );
}

// ---------------------------------------------------------------------------------------------
// 9. Rename behind the back; server death
// ---------------------------------------------------------------------------------------------

#[test]
fn location_survives_a_rename_behind_the_back_and_server_death_is_not_running() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("a");
    let a = core.create_session(agent("a", &repository)).unwrap();
    let cwd = cwd_of(&core, &a);
    let b = core.create_session(agent("b", &h.repository("b"))).unwrap();
    assert!(wait_for(Duration::from_secs(5), || cwd
        .join("env.txt")
        .exists()));
    let before = core.get_execution_location(&a).unwrap();
    h.tmux_ok(&["rename-session", "-t", "=a", "sneaky"]);
    let after = core.get_execution_location(&a).unwrap();
    assert_eq!(before, after, "targets are server-local ids, not names");
    // The record still says "a"; tmux says "sneaky". "sneaky" is free by records but taken by
    // tmux: NameTaken from tmux.
    assert_eq!(
        kind(
            &core
                .create_session(agent("sneaky", &repository))
                .unwrap_err()
        ),
        "NameTaken"
    );
    core.send_message(&a, "hi", SendOptions::default()).unwrap();
    assert!(wait_for(Duration::from_secs(5), || typed(&cwd).contains("hi")));

    let bytes_a = h.record_bytes(&a);
    let bytes_b = h.record_bytes(&b);
    let log_before = h.log_lines().len();
    h.tmux(&["kill-server"]);
    assert_eq!(
        kind(&core.get_execution_location(&a).unwrap_err()),
        "NotRunning"
    );
    assert_eq!(
        kind(
            &core
                .send_message(&a, "x", SendOptions::default())
                .unwrap_err()
        ),
        "NotRunning"
    );
    assert_eq!(
        kind(
            &core
                .send_message(&b, "x", SendOptions::from_agent(a.clone()))
                .unwrap_err()
        ),
        "NotRunning"
    );
    assert!(h.live_names().is_empty(), "no op restarted the server");
    assert_eq!(h.record_bytes(&a), bytes_a);
    assert_eq!(h.record_bytes(&b), bytes_b);
    assert_eq!(h.log_lines().len(), log_before);
    assert_eq!(core.get_session(&a).unwrap().lifecycle, Lifecycle::Running);

    // A rename on a Running record with no live session is record-only.
    assert_eq!(core.rename_session(&a, "renamed").unwrap().name, "renamed");
    assert!(h.live_names().is_empty());
    let changed = core.refresh_all().unwrap();
    let mut ids: Vec<SessionId> = changed.iter().map(|s| s.id.clone()).collect();
    ids.sort();
    let mut expected = vec![a.clone(), b.clone()];
    expected.sort();
    assert_eq!(ids, expected);
    assert!(
        changed
            .iter()
            .all(|s| s.lifecycle == Lifecycle::Exited && s.ended_at.is_some())
    );
    assert!(
        h.live_names().is_empty(),
        "refresh did not restart the server"
    );
    assert_eq!(kind(&core.stop_session(&a).unwrap_err()), "NotRunning");
    assert_eq!(
        kind(
            &core
                .send_message(&b, "x", SendOptions::default())
                .unwrap_err()
        ),
        "NotRunning"
    );
    assert!(core.refresh_all().unwrap().is_empty());
    core.archive_session(&a).unwrap();
    // Creating again starts a fresh server; old records stay Exited.
    let c = core.create_session(agent("renamed", &repository)).unwrap();
    assert_eq!(h.live_names(), ["renamed"]);
    assert_eq!(core.resolve("renamed").unwrap(), c);
    assert_eq!(core.get_session(&a).unwrap().lifecycle, Lifecycle::Exited);
}

#[test]
fn stop_after_server_death_reports_exited() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core.create_session(agent("s", &h.repository("a"))).unwrap();
    h.tmux(&["kill-server"]);
    let session = core.stop_session(&id).unwrap();
    assert_eq!(
        session.lifecycle,
        Lifecycle::Exited,
        "what happened, not what was asked"
    );
    assert!(session.ended_at.is_some());
    assert!(h.live_names().is_empty());
    assert_eq!(log_types(&h), ["created", "exited"]);
    assert_eq!(kind(&core.stop_session(&id).unwrap_err()), "NotRunning");
}

// ---------------------------------------------------------------------------------------------
// Terminal states never revive (targeted)
// ---------------------------------------------------------------------------------------------

#[test]
fn terminal_records_never_revive() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let repository = h.repository("a");
    let stopped = core.create_session(agent("s", &repository)).unwrap();
    let exited = core.create_session(agent("e", &repository)).unwrap();
    core.stop_session(&stopped).unwrap();
    h.tmux_ok(&["kill-session", "-t", "=e"]);
    core.refresh_session(&exited).unwrap();
    let ended: Vec<f64> = [&stopped, &exited]
        .iter()
        .map(|id| core.get_session(id).unwrap().ended_at.unwrap())
        .collect();
    // A foreign tmux session with the record's name and even its @tx-id must not revive it.
    h.tmux_ok(&["new-session", "-d", "-s", "s", "sleep", "30"]);
    h.tmux_ok(&["set-option", "-t", "=s:", "@tx-id", stopped.as_str()]);
    h.tmux_ok(&["new-session", "-d", "-s", "e", "sleep", "30"]);
    h.tmux_ok(&["set-option", "-t", "=e:", "@tx-id", exited.as_str()]);
    assert!(core.refresh_all().unwrap().is_empty());
    for (index, (id, at)) in [&stopped, &exited].into_iter().zip(ended).enumerate() {
        assert!(matches!(
            core.refresh_session(id).unwrap(),
            RefreshResult::Unchanged(_)
        ));
        // Distinct names: an ended, unarchived record keeps holding the one it takes.
        core.rename_session(id, &format!("zz{index}")).unwrap();
        core.rename_session(id, &format!("back{index}")).unwrap();
        core.set_session_tags(id, vec!["t".into()]).unwrap();
        let session = core.get_session(id).unwrap();
        assert!(session.lifecycle.is_terminal());
        assert_eq!(
            session.ended_at,
            Some(at),
            "ended_at is never cleared or rewritten"
        );
        assert_eq!(kind(&core.stop_session(id).unwrap_err()), "NotRunning");
        assert_eq!(
            kind(
                &core
                    .send_message(id, "x", SendOptions::default())
                    .unwrap_err()
            ),
            "NotRunning"
        );
    }
    // A terminal record has no execution location, even when a foreign session carries its id.
    assert_eq!(
        kind(&core.get_execution_location(&stopped).unwrap_err()),
        "NotRunning"
    );
}
