use std::cell::RefCell;
use std::collections::BTreeSet;

use proptest::prelude::*;

use super::*;

/// The world outside the runtime: a commutative table of `(owner, entry)` rows.
type World = Rc<RefCell<BTreeSet<(String, String)>>>;

const DB: Key<String> = Key::new("db");

/// A test component: injects/provides the given keys, adds one world row per activation, and
/// logs every apply and undo (reading its dependencies during teardown, as a pool drain would).
struct Probe {
    name: &'static str,
    inject: &'static [&'static str],
    provide: &'static [&'static str],
    world: World,
    log: Rc<RefCell<Vec<String>>>,
    fail_after_effect: bool,
}

impl Probe {
    fn new(name: &'static str, world: &World, log: &Rc<RefCell<Vec<String>>>) -> Self {
        Self {
            name,
            inject: &[],
            provide: &[],
            world: Rc::clone(world),
            log: Rc::clone(log),
            fail_after_effect: false,
        }
    }
    fn inject(mut self, keys: &'static [&'static str]) -> Self {
        self.inject = keys;
        self
    }
    fn provide(mut self, keys: &'static [&'static str]) -> Self {
        self.provide = keys;
        self
    }
}

impl Component for Probe {
    fn name(&self) -> &str {
        self.name
    }
    fn inject(&self) -> &[&'static str] {
        self.inject
    }
    fn provide(&self) -> &[&'static str] {
        self.provide
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        let db = if self.inject.contains(&"db") {
            Some(ctx.get(DB)?)
        } else {
            None
        };
        let row = (
            self.name.to_owned(),
            db.as_deref().cloned().unwrap_or_default(),
        );
        self.log
            .borrow_mut()
            .push(format!("apply {} {}", self.name, row.1));
        self.world.borrow_mut().insert(row.clone());
        let (world, log, name) = (Rc::clone(&self.world), Rc::clone(&self.log), self.name);
        ctx.effect(move || {
            let seen = db.map(|db| format!(" saw {db}")).unwrap_or_default();
            log.borrow_mut().push(format!("undo {name}{seen}"));
            world.borrow_mut().remove(&row);
        });
        if self.fail_after_effect {
            return Err("boom".into());
        }
        if self.provide.contains(&"db") {
            ctx.provide(DB, format!("{}-conn", self.name))?;
        }
        Ok(())
    }
}

fn fixture() -> (Runtime, World, Rc<RefCell<Vec<String>>>) {
    (Runtime::new(), World::default(), Rc::default())
}

#[test]
fn consumer_waits_for_its_provider_whatever_the_plug_order() {
    let (mut rt, world, log) = fixture();
    let consumer = rt
        .plug(Probe::new("app", &world, &log).inject(&["db"]))
        .unwrap();
    assert_eq!(rt.state(consumer), Some(FiberState::Inactive));
    rt.plug(Probe::new("pg", &world, &log).provide(&["db"]))
        .unwrap();
    assert_eq!(rt.state(consumer), Some(FiberState::Active));
    assert_eq!(*log.borrow(), ["apply pg ", "apply app pg-conn"]);
}

#[test]
fn provider_outlives_consumer_teardown() {
    let (mut rt, world, log) = fixture();
    let pg = rt
        .plug(Probe::new("pg", &world, &log).provide(&["db"]))
        .unwrap();
    rt.plug(Probe::new("app", &world, &log).inject(&["db"]))
        .unwrap();
    rt.unplug(pg);
    let log = log.borrow();
    assert_eq!(log[2..], ["undo app saw pg-conn", "undo pg"]);
    assert!(world.borrow().is_empty());
}

#[test]
fn replacing_a_provider_reloads_only_its_consumers() {
    let (mut rt, world, log) = fixture();
    let pg = rt
        .plug(Probe::new("pg", &world, &log).provide(&["db"]))
        .unwrap();
    let app = rt
        .plug(Probe::new("app", &world, &log).inject(&["db"]))
        .unwrap();
    rt.plug(Probe::new("other", &world, &log)).unwrap();
    rt.unplug(pg);
    assert_eq!(rt.state(app), Some(FiberState::Inactive));
    rt.plug(Probe::new("sqlite", &world, &log).provide(&["db"]))
        .unwrap();
    assert_eq!(rt.state(app), Some(FiberState::Active));
    assert_eq!(log.borrow().last().unwrap(), "apply app sqlite-conn");
    assert_eq!(
        log.borrow().iter().filter(|l| l.contains("other")).count(),
        1
    );
}

#[test]
fn failed_apply_is_unwound_reported_and_not_retried() {
    let (mut rt, world, log) = fixture();
    let mut bad = Probe::new("bad", &world, &log);
    bad.fail_after_effect = true;
    rt.plug(bad).unwrap();
    assert!(world.borrow().is_empty());
    let failures = rt.take_failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].0, "bad");
    rt.plug(Probe::new("x", &world, &log)).unwrap();
    assert_eq!(
        log.borrow()
            .iter()
            .filter(|l| l.starts_with("apply bad"))
            .count(),
        1
    );
}

#[test]
fn undo_stack_runs_last_in_first_out() {
    struct Steps(Rc<RefCell<Vec<String>>>);
    impl Component for Steps {
        fn name(&self) -> &str {
            "steps"
        }
        fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
            for step in ["open", "listen", "register"] {
                let log = Rc::clone(&self.0);
                ctx.effect(move || log.borrow_mut().push(format!("undo {step}")));
            }
            Ok(())
        }
    }
    let log = Rc::<RefCell<Vec<String>>>::default();
    let mut rt = Runtime::new();
    let id = rt.plug(Steps(Rc::clone(&log))).unwrap();
    rt.unplug(id);
    assert_eq!(*log.borrow(), ["undo register", "undo listen", "undo open"]);
}

#[test]
fn overlapping_provision_is_rejected() {
    let (mut rt, world, log) = fixture();
    rt.plug(Probe::new("pg", &world, &log).provide(&["db"]))
        .unwrap();
    let err = rt
        .plug(Probe::new("sqlite", &world, &log).provide(&["db"]))
        .unwrap_err();
    assert!(
        matches!(err, Error::ProvisionConflict { key: "db", .. }),
        "{err}"
    );
}

#[test]
fn undeclared_access_is_refused() {
    struct Sneaky;
    impl Component for Sneaky {
        fn name(&self) -> &str {
            "sneaky"
        }
        fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
            ctx.get(DB)?;
            Ok(())
        }
    }
    let (mut rt, world, log) = fixture();
    rt.plug(Probe::new("pg", &world, &log).provide(&["db"]))
        .unwrap();
    rt.plug(Sneaky).unwrap();
    let failures = rt.take_failures();
    let error = failures[0].1.downcast_ref::<Error>().unwrap();
    assert!(
        matches!(error, Error::UndeclaredAccess { key: "db", .. }),
        "{error}"
    );
}

#[test]
fn unplugging_a_host_cascades_to_its_children() {
    struct Host(World, Rc<RefCell<Vec<String>>>);
    impl Component for Host {
        fn name(&self) -> &str {
            "host"
        }
        fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
            ctx.plug(Probe::new("child", &self.0, &self.1))?;
            Ok(())
        }
    }
    let (mut rt, world, log) = fixture();
    let host = rt.plug(Host(Rc::clone(&world), Rc::clone(&log))).unwrap();
    assert_eq!(world.borrow().len(), 1);
    rt.unplug(host);
    assert!(world.borrow().is_empty());
    assert_eq!(rt.state(host), None, "vestigial host is garbage-collected");
}

// Confluence + recovery exactness: whatever interleaving of plugs and unplugs happened, the
// quiescent world equals a from-scratch load of the final configuration, and shutdown empties it.
#[derive(Clone, Copy, Debug)]
enum Op {
    Plug(usize),
    Unplug(usize),
}

const CAST: [(&str, &[&str], &[&str]); 4] = [
    ("pg", &[], &["db"]),
    ("app", &["db"], &[]),
    ("cache", &["db"], &[]),
    ("cli", &[], &[]),
];

fn probe(i: usize, world: &World, log: &Rc<RefCell<Vec<String>>>) -> Probe {
    let (name, inject, provide) = CAST[i];
    Probe::new(name, world, log).inject(inject).provide(provide)
}

proptest! {
    #[test]
    fn confluence_and_recovery(ops in prop::collection::vec(
        prop_oneof![(0..CAST.len()).prop_map(Op::Plug), (0..CAST.len()).prop_map(Op::Unplug)], 0..24)
    ) {
        let (mut rt, world, log) = fixture();
        let mut live: [Option<FiberId>; CAST.len()] = [None; CAST.len()];
        for op in ops {
            match op {
                Op::Plug(i) if live[i].is_none() => live[i] = Some(rt.plug(probe(i, &world, &log)).unwrap()),
                Op::Unplug(i) => if let Some(id) = live[i].take() { rt.unplug(id) },
                Op::Plug(_) => {}
            }
        }
        let (mut fresh, fresh_world, fresh_log) = fixture();
        for (i, id) in live.iter().enumerate() {
            if id.is_some() {
                fresh.plug(probe(i, &fresh_world, &fresh_log)).unwrap();
            }
        }
        prop_assert_eq!(&*world.borrow(), &*fresh_world.borrow());
        rt.shutdown();
        prop_assert!(world.borrow().is_empty());
    }
}

#[test]
fn a_failed_provider_frees_its_key_for_a_replacement() {
    let (mut rt, world, log) = fixture();
    let mut bad = Probe::new("pg", &world, &log).provide(&["db"]);
    bad.fail_after_effect = true;
    rt.plug(bad).unwrap();
    let app = rt.plug(Probe::new("app", &world, &log).inject(&["db"])).unwrap();
    rt.plug(Probe::new("sqlite", &world, &log).provide(&["db"])).unwrap();
    assert_eq!(rt.state(app), Some(FiberState::Active));
}

#[test]
fn unplugging_a_deep_tree_removes_every_fiber() {
    struct Nest(u8, World, Rc<RefCell<Vec<String>>>);
    impl Component for Nest {
        fn name(&self) -> &str {
            "nest"
        }
        fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
            if self.0 > 0 {
                ctx.plug(Nest(self.0 - 1, Rc::clone(&self.1), Rc::clone(&self.2)))?;
            } else {
                ctx.plug(Probe::new("leaf", &self.1, &self.2))?;
            }
            Ok(())
        }
    }
    let (mut rt, world, log) = fixture();
    let root = rt.plug(Nest(3, Rc::clone(&world), Rc::clone(&log))).unwrap();
    assert_eq!(world.borrow().len(), 1);
    rt.unplug(root);
    assert!(world.borrow().is_empty());
    assert!(rt.fibers.is_empty(), "{} fibers left", rt.fibers.len());
}
