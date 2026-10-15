//! Adversarial / property tests for `TxCore` against a private tmux server: a model-based random
//! operation sequence, record integrity on every error path, terminal states, `resolve` edge
//! cases, odd tags / parents / env values, concurrent cores, forget-then-create, server death.
//!
//! Tests marked `#[ignore = "BUG: …"]` document real failures; they stay in the file so the
//! reproduction survives, and the suite stays green.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use common::{Harness, agent, process, typed, wait_for};
use proptest::prelude::*;
use tx_core::{
    CoreError, Lifecycle, ListFilter, RefreshResult, SendOptions, SessionId, TxCore,
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
        CoreError::UnknownEngine(_) => "UnknownEngine",
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
    assert!(wait_for(Duration::from_secs(5), || cwd.join("env.txt").exists()));
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
    Forget(usize),
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
        2 => who.clone().prop_map(Op::Forget),
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
    /// tmux sessions no record manages any more (forgotten while live).
    orphans: BTreeSet<String>,
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

    fn name_running(&self, name: &str, except: Option<&SessionId>) -> bool {
        self.recs.iter().any(|(id, rec)| {
            rec.lifecycle == Lifecycle::Running && rec.name == name && Some(id) != except
        })
    }

    fn expected_live(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .recs
            .values()
            .filter(|rec| rec.live())
            .map(|rec| rec.name.clone())
            .chain(self.orphans.iter().cloned())
            .collect();
        names.sort();
        names
    }

    /// `resolve(name)`: the `Running` record, else the most recently created.
    fn expected_resolve(&self, name: &str) -> Option<SessionId> {
        let named: Vec<&SessionId> = self
            .all_ids
            .iter()
            .filter(|id| self.recs.get(*id).is_some_and(|rec| rec.name == name))
            .collect();
        named
            .iter()
            .find(|id| self.recs[*id].lifecycle == Lifecycle::Running)
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
fn check_model(h: &Harness, core: &TxCore, model: &mut Model, step: usize) -> Result<(), TestCaseError> {
    let mut listed = core.list_sessions(&ListFilter::default()).unwrap();
    listed.sort();
    let mut expected: Vec<SessionId> = model.recs.keys().cloned().collect();
    expected.sort();
    prop_assert_eq!(listed, expected, "step {}: ids", step);

    let mut running = core
        .list_sessions(&ListFilter::new().lifecycle(Lifecycle::Running))
        .unwrap();
    running.sort();
    let mut expected_running: Vec<SessionId> = model
        .recs
        .iter()
        .filter(|(_, rec)| rec.lifecycle == Lifecycle::Running)
        .map(|(id, _)| id.clone())
        .collect();
    expected_running.sort();
    prop_assert_eq!(running, expected_running, "step {}: running filter", step);

    let mut tagged = core
        .list_sessions(&ListFilter::new().tag("t1"))
        .unwrap();
    tagged.sort();
    let mut expected_tagged: Vec<SessionId> = model
        .recs
        .iter()
        .filter(|(_, rec)| rec.tags.iter().any(|t| t == "t1"))
        .map(|(id, _)| id.clone())
        .collect();
    expected_tagged.sort();
    prop_assert_eq!(tagged, expected_tagged, "step {}: tag filter", step);

    if let Some(first) = model.all_ids.first() {
        let mut children = core
            .list_sessions(&ListFilter::new().parent(first.clone()))
            .unwrap();
        children.sort();
        let mut expected_children: Vec<SessionId> = model
            .recs
            .iter()
            .filter(|(_, rec)| rec.parent.as_ref() == Some(first))
            .map(|(id, _)| id.clone())
            .collect();
        expected_children.sort();
        prop_assert_eq!(children, expected_children, "step {}: parent filter", step);
    }

    for (id, rec) in &mut model.recs {
        let session = core.get_session(id).unwrap();
        prop_assert_eq!(&session.name, &rec.name, "step {}: name of {}", step, id);
        prop_assert_eq!(session.lifecycle, rec.lifecycle, "step {}: lifecycle of {}", step, id);
        prop_assert_eq!(&session.tags, &rec.tags, "step {}: tags of {}", step, id);
        prop_assert_eq!(&session.parent, &rec.parent, "step {}: parent of {}", step, id);
        prop_assert_eq!(
            session.ended_at.is_some(),
            rec.lifecycle.is_terminal(),
            "step {}: ended_at presence of {}",
            step,
            id
        );
        match rec.ended_at {
            None => rec.ended_at = session.ended_at,
            Some(at) => prop_assert_eq!(session.ended_at, Some(at), "step {}: ended_at changed", step),
        }
    }

    prop_assert_eq!(sorted_live(h), model.expected_live(), "step {}: live names", step);

    let names: BTreeSet<String> = model.recs.values().map(|rec| rec.name.clone()).collect();
    for name in names {
        let resolved = core.resolve(&name).ok();
        prop_assert_eq!(resolved, model.expected_resolve(&name), "step {}: resolve {}", step, name);
    }
    prop_assert_eq!(h.log_lines().len(), model.log_len, "step {}: log length", step);
    Ok(())
}

fn run_ops(h: &Harness, core: &TxCore, ops: &[Op]) -> Result<(), TestCaseError> {
    let mut model = Model::default();
    let cwd = h.workdir("w");
    for (step, op) in ops.iter().enumerate() {
        let before = snapshot(h, &model);
        // Ids whose record may legitimately change in this step.
        let mut touched: BTreeSet<SessionId> = BTreeSet::new();
        match op {
            Op::Create { name, tags, parent } => {
                let name = name_at(*name);
                let mut opts = process(&name, &cwd);
                opts.tags = tags.iter().map(|t| TAGS[*t].to_owned()).collect();
                opts.parent = parent.map(|p| model.id_for(p));
                let valid = !name.is_empty() && name.len() <= 128 && !name.contains(' ');
                let expected = if !valid {
                    "InvalidName"
                } else if model.name_running(&name, None) || model.orphans.contains(&name) {
                    "NameTaken"
                } else {
                    "Ok"
                };
                let result = core.create_session(opts.clone());
                prop_assert_eq!(verdict(&result), expected, "step {}: create {}", step, name);
                if let Ok(id) = result {
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
                    } else if model.name_running(&name, Some(&id))
                        || (rec.live() && model.orphans.contains(&name))
                    {
                        "NameTaken"
                    } else {
                        "Ok"
                    }
                } else {
                    "NotFound"
                };
                let result = core.rename_session(&id, &name);
                prop_assert_eq!(verdict(&result), expected, "step {}: rename {} -> {}", step, id, name);
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
                let expected = if model.recs.contains_key(&id) { "Ok" } else { "NotFound" };
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
                    let wanted = if rec.killed { Lifecycle::Exited } else { Lifecycle::Stopped };
                    prop_assert_eq!(session.lifecycle, wanted, "step {}: stop of a killed session", step);
                    prop_assert!(session.ended_at.is_some());
                    rec.lifecycle = wanted;
                    rec.killed = false;
                    model.log_len += 1;
                    touched.insert(id);
                }
            }
            Op::Forget(who) => {
                let id = model.id_for(*who);
                let existed = model.recs.contains_key(&id);
                let result = core.forget_session(&id);
                prop_assert_eq!(verdict(&result), "Ok", "step {}: forget {}", step, id);
                prop_assert_eq!(result.unwrap(), existed, "step {}: forget existed", step);
                if let Some(rec) = model.recs.remove(&id) {
                    if rec.live() {
                        model.orphans.insert(rec.name);
                    }
                    model.log_len += 1;
                    touched.insert(id);
                }
            }
            Op::Refresh(who) => {
                let id = model.id_for(*who);
                let result = core.refresh_session(&id);
                match model.recs.get_mut(&id) {
                    None => prop_assert_eq!(verdict(&result), "NotFound", "step {}: refresh ghost", step),
                    Some(rec) => {
                        let result = result.unwrap();
                        let exited = matches!(result, RefreshResult::Exited(_));
                        prop_assert_eq!(exited, rec.killed, "step {}: refresh {}", step, id);
                        prop_assert_eq!(result.session().lifecycle, if rec.killed { Lifecycle::Exited } else { rec.lifecycle });
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
                prop_assert_eq!(got, expected.clone(), "step {}: refresh_all changed set", step);
                let all_exited = changed.iter().all(|s| s.lifecycle == Lifecycle::Exited);
                prop_assert!(all_exited, "step {}: refresh_all returned a non-Exited record", step);
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
                if let Some(rec) = model.recs.values_mut().find(|rec| rec.live() && rec.name == name) {
                    rec.killed = true;
                } else {
                    model.orphans.remove(name);
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
                let result = core.send_message(&to, &format!("m{step}"), match from { Some(from) => SendOptions::from_agent(from), None => SendOptions::default() });
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
                prop_assert_eq!(&now, bytes, "step {}: untouched record {} was rewritten", step, id);
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
        let core = h.open();
        run_ops(&h, &core, &ops)?;
    }
}

/// A fixed sequence hitting the sharp corners the random walk may miss.
#[test]
fn scripted_corner_sequence_matches_the_model() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let ops = [
        Op::Create { name: 0, tags: vec![0], parent: None },
        Op::Create { name: 1, tags: vec![], parent: Some(0) },
        Op::Kill(0),
        Op::Create { name: 0, tags: vec![], parent: None }, // NameTaken: killed but still Running
        Op::Rename { who: 1, name: 0 },                     // NameTaken: record of alpha is Running
        Op::Stop(0),                                        // Exited, not Stopped
        Op::Create { name: 0, tags: vec![], parent: None }, // name free again
        Op::Forget(2),                                      // forget a live one: orphan in tmux
        Op::Create { name: 0, tags: vec![], parent: None }, // NameTaken by tmux
        Op::Rename { who: 1, name: 0 },                     // live rename into an orphan: NameTaken
        Op::Kill(1),
        Op::Rename { who: 1, name: 0 },                     // killed record: record-only rename, Ok
        Op::RefreshAll,
        Op::Refresh(1),
        Op::Stop(1),                                        // NotRunning
        Op::Send { to: 1, from: None },                     // NotRunning
        Op::Send { to: 0, from: Some(9) },                  // NotFound (sender)
        Op::Kill(0),                                        // kills the orphan
        Op::Create { name: 0, tags: vec![], parent: None }, // free: every "alpha" record is terminal
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
    let core = h.open();
    let cwd = h.workdir("a");
    let a = core.create_session(process("a", &cwd)).unwrap();
    // A uuid is a valid name; a session *named* like another session's id.
    let b = core.create_session(process(a.as_str(), &cwd)).unwrap();
    assert_ne!(a, b);
    assert_eq!(core.resolve(a.as_str()).unwrap(), a, "id wins over an equal name");
    assert_eq!(core.resolve(b.as_str()).unwrap(), b);
    assert_eq!(core.resolve("a").unwrap(), a);
    // Once the id record is gone, the name match surfaces.
    core.forget_session(&a).unwrap();
    assert_eq!(core.resolve(a.as_str()).unwrap(), b, "forgotten id falls through to the name");
    // Even when the named record is stopped, the id-looking name still resolves to it.
    core.stop_session(&b).unwrap();
    assert_eq!(core.resolve(a.as_str()).unwrap(), b);
}

#[test]
fn resolve_three_records_one_name() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    let first = core.create_session(process("n", &cwd)).unwrap();
    core.stop_session(&first).unwrap();
    let second = core.create_session(process("n", &cwd)).unwrap();
    core.stop_session(&second).unwrap();
    let third = core.create_session(process("n", &cwd)).unwrap();
    assert_eq!(core.resolve("n").unwrap(), third);
    // Kill behind the back: the record is still Running and still wins.
    h.tmux_ok(&["kill-session", "-t", "=n"]);
    assert_eq!(core.resolve("n").unwrap(), third);
    core.stop_session(&third).unwrap();
    assert_eq!(core.get_session(&third).unwrap().lifecycle, Lifecycle::Exited);
    assert_eq!(core.resolve("n").unwrap(), third, "most recently created among terminals");
    // Rename an older terminal record to the name: still the most recent *created* wins.
    core.rename_session(&first, "m").unwrap();
    core.rename_session(&first, "n").unwrap();
    assert_eq!(core.resolve("n").unwrap(), third);
    core.forget_session(&third).unwrap();
    assert_eq!(core.resolve("n").unwrap(), second);
}

// ---------------------------------------------------------------------------------------------
// 5. parent / tags are stored verbatim, never validated
// ---------------------------------------------------------------------------------------------

#[test]
fn parent_is_never_validated_and_tags_are_verbatim() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    let ghost: SessionId = "no-such-parent".parse().unwrap();
    let mut opts = process("p", &cwd);
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
    assert_eq!(session.parent, Some(ghost.clone()), "dangling parent accepted verbatim");
    assert_eq!(session.tags, opts.tags, "tags kept verbatim: empty, duplicate, unicode, 10 kB");
    let by_parent = core.list_sessions(&ListFilter::new().parent(ghost)).unwrap();
    assert_eq!(by_parent, std::slice::from_ref(&id));
    for tag in ["", "dup", "ünïcödé 😀", "with\nnewline", big.as_str()] {
        let found = core.list_sessions(&ListFilter::new().tag(tag)).unwrap();
        assert_eq!(found, std::slice::from_ref(&id), "tag {tag:?}");
    }
    // A session may even claim itself as parent via set_tags? No API; but rename/tags keep parent.
    let session = core.set_session_tags(&id, vec![big.clone(), big.clone()]).unwrap();
    assert_eq!(session.tags, [big.clone(), big]);
    // The log line is bounded even though the tag text is not.
    let last = h.log_lines().pop().unwrap();
    assert_eq!(last["type"], "tagged");
    assert!(last["msg"].as_str().unwrap().len() <= 600, "log msg is trimmed");
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
    config.env = vec![("FROM_PLAN".into(), "config".into()), ("CFG".into(), "config".into())];
    let mut core = TxCore::open(config).unwrap();
    core.register_engine(Box::new(common::ShellStub)).unwrap();
    let cwd = h.workdir("a");
    let big = "v".repeat(8 * 1024);
    let mut opts = agent("e", &cwd);
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
    let text = env_text(&cwd);
    assert!(text.contains("\nEQ=a=b=c\n"), "{text}");
    assert!(text.contains("\nSP=  two  words  \n"), "{text}");
    assert!(text.contains("\nNL=line1\nline2\n"), "newline value: {text}");
    assert!(text.contains(&format!("\nBIG={big}\n")), "8 kB value");
    assert!(text.contains("\nEMPTY=\n"), "{text}");
    assert!(text.contains("\nUNI=żółć 😀\n"), "{text}");
    assert!(text.contains("\nTX_SESSION_ID=override\n"), "opts.env overrides TX_SESSION_ID");
    assert!(!text.contains(&format!("TX_SESSION_ID={id}")), "the real id is not in the env any more");
    assert!(text.contains("\nFROM_PLAN=plan\n"), "plan env beats config env");
    assert!(text.contains("\nCFG=opts\n"), "option env beats config env");
    // Core's bookkeeping does not depend on the env: @tx-id still links the session.
    assert!(core.get_execution_location(&id).is_ok());
    assert!(matches!(core.refresh_session(&id).unwrap(), RefreshResult::Unchanged(_)));
}

#[test]
fn env_keys_rejected_before_any_state() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    for key in ["", "A=B", "A B", "A\nB", "A\0", "ä", "A$", "A/B"] {
        let mut opts = process("k", &cwd);
        opts.env = vec![(key.into(), "1".into())];
        let error = core.create_session(opts).unwrap_err();
        assert_eq!(kind(&error), "InvalidEnvKey", "{key:?}");
    }
    let mut opts = process("k", &cwd);
    opts.env = vec![("A".into(), "x\0y".into())];
    assert_eq!(kind(&core.create_session(opts).unwrap_err()), "InvalidEnvKey");
    assert!(h.live_names().is_empty());
    assert!(h.log_lines().is_empty());
    assert!(core.list_sessions(&ListFilter::default()).unwrap().is_empty());
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
            let cwd = h.workdir(&format!("w{i}"));
            std::thread::spawn(move || {
                let core = TxCore::open(config).unwrap();
                barrier.wait();
                core.create_session(process(&format!("par-{i}"), &cwd))
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
    let leftovers: Vec<String> = std::fs::read_dir(h.state_dir().join("sessions"))
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
// 8. forget then create / refresh
// ---------------------------------------------------------------------------------------------

#[test]
fn forget_of_a_live_session_keeps_its_name_taken_in_tmux() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let cwd = h.workdir("a");
    let id = core.create_session(agent("f", &cwd)).unwrap();
    assert!(core.forget_session(&id).unwrap());
    let error = core.create_session(process("f", &h.workdir("b"))).unwrap_err();
    assert_eq!(kind(&error), "NameTaken", "tmux still holds the name");
    assert!(core.list_sessions(&ListFilter::default()).unwrap().is_empty());
    assert!(core.refresh_all().unwrap().is_empty(), "no record, nothing to refresh");
    assert_eq!(kind(&core.refresh_session(&id).unwrap_err()), "NotFound");
    assert_eq!(kind(&core.get_execution_location(&id).unwrap_err()), "NotFound");
    assert_eq!(kind(&core.send_message(&id, "x", SendOptions::default()).unwrap_err()), "NotFound");
    assert_eq!(kind(&core.stop_session(&id).unwrap_err()), "NotFound");
    assert_eq!(kind(&core.rename_session(&id, "g").unwrap_err()), "NotFound");
    assert_eq!(h.live_names(), ["f"], "the orphan keeps running");
    assert_eq!(log_types(&h), ["created", "forgotten"]);
    // A failed create against the orphan name left nothing behind.
    assert!(!h.state_dir().join("profiles").read_dir().unwrap().any(|_| true));
    // Once the orphan is gone, the name is free again.
    h.tmux_ok(&["kill-session", "-t", "=f"]);
    let again = core.create_session(process("f", &h.workdir("b"))).unwrap();
    assert_ne!(again, id);
    assert_eq!(h.live_names(), ["f"]);
}

// ---------------------------------------------------------------------------------------------
// 9. Rename behind the back; server death
// ---------------------------------------------------------------------------------------------

#[test]
fn location_survives_a_rename_behind_the_back_and_server_death_is_not_running() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let cwd = h.workdir("a");
    let a = core.create_session(process("a", &cwd)).unwrap();
    let b = core.create_session(process("b", &h.workdir("b"))).unwrap();
    assert!(wait_for(Duration::from_secs(5), || cwd.join("env.txt").exists()));
    let before = core.get_execution_location(&a).unwrap();
    h.tmux_ok(&["rename-session", "-t", "=a", "sneaky"]);
    let after = core.get_execution_location(&a).unwrap();
    assert_eq!(before, after, "targets are server-local ids, not names");
    // The record still says "a"; tmux says "sneaky". A new create named "a" succeeds in tmux
    // but the record of `a` is Running with that name: NameTaken by the record.
    assert_eq!(kind(&core.create_session(process("a", &cwd)).unwrap_err()), "NameTaken");
    // And "sneaky" is free by records but taken by tmux: NameTaken from tmux.
    assert_eq!(kind(&core.create_session(process("sneaky", &cwd)).unwrap_err()), "NameTaken");
    core.send_message(&a, "hi", SendOptions::default()).unwrap();
    assert!(wait_for(Duration::from_secs(5), || typed(&cwd).contains("hi")));

    let bytes_a = h.record_bytes(&a);
    let bytes_b = h.record_bytes(&b);
    let log_before = h.log_lines().len();
    h.tmux(&["kill-server"]);
    assert_eq!(kind(&core.get_execution_location(&a).unwrap_err()), "NotRunning");
    assert_eq!(kind(&core.send_message(&a, "x", SendOptions::default()).unwrap_err()), "NotRunning");
    assert_eq!(kind(&core.send_message(&b, "x", SendOptions::from_agent(a.clone())).unwrap_err()), "NotRunning");
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
    assert!(changed.iter().all(|s| s.lifecycle == Lifecycle::Exited && s.ended_at.is_some()));
    assert!(h.live_names().is_empty(), "refresh did not restart the server");
    assert_eq!(kind(&core.stop_session(&a).unwrap_err()), "NotRunning");
    assert_eq!(kind(&core.send_message(&b, "x", SendOptions::default()).unwrap_err()), "NotRunning");
    assert!(core.refresh_all().unwrap().is_empty());
    // Creating again starts a fresh server; old records stay Exited.
    let c = core.create_session(process("renamed", &cwd)).unwrap();
    assert_eq!(h.live_names(), ["renamed"]);
    assert_eq!(core.resolve("renamed").unwrap(), c);
    assert_eq!(core.get_session(&a).unwrap().lifecycle, Lifecycle::Exited);
}

#[test]
fn stop_after_server_death_reports_exited() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open();
    let id = core.create_session(process("s", &h.workdir("a"))).unwrap();
    h.tmux(&["kill-server"]);
    let session = core.stop_session(&id).unwrap();
    assert_eq!(session.lifecycle, Lifecycle::Exited, "what happened, not what was asked");
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
    let core = h.open();
    let cwd = h.workdir("a");
    let stopped = core.create_session(process("s", &cwd)).unwrap();
    let exited = core.create_session(process("e", &cwd)).unwrap();
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
    for (id, at) in [&stopped, &exited].into_iter().zip(ended) {
        assert!(matches!(core.refresh_session(id).unwrap(), RefreshResult::Unchanged(_)));
        core.rename_session(id, "zz").unwrap();
        core.rename_session(id, "back").unwrap();
        core.set_session_tags(id, vec!["t".into()]).unwrap();
        let session = core.get_session(id).unwrap();
        assert!(session.lifecycle.is_terminal());
        assert_eq!(session.ended_at, Some(at), "ended_at is never cleared or rewritten");
        assert_eq!(kind(&core.stop_session(id).unwrap_err()), "NotRunning");
        assert_eq!(kind(&core.send_message(id, "x", SendOptions::default()).unwrap_err()), "NotRunning");
    }
    // A terminal record has no execution location, even when a foreign session carries its id.
    assert_eq!(kind(&core.get_execution_location(&stopped).unwrap_err()), "NotRunning");
}
