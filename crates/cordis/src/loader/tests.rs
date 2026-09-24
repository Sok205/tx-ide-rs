use std::cell::RefCell;
use std::collections::BTreeSet;

use proptest::prelude::*;

use super::*;
use crate::{BoxError, Ctx, FiberState, Key};

type World = Rc<RefCell<BTreeSet<String>>>;
type Applies = Rc<RefCell<Vec<String>>>;

const DB: Key<String> = Key::new("db");

/// A configurable component: adds `<id>:<config>` to the world while loaded. `db` provides the
/// `db` key; `app` injects it.
struct Unit {
    id: String,
    config: u8,
    world: World,
    applies: Applies,
}

impl Component for Unit {
    fn name(&self) -> &str {
        &self.id
    }
    fn inject(&self) -> &[&'static str] {
        if self.id == "app" { &["db"] } else { &[] }
    }
    fn provide(&self) -> &[&'static str] {
        if self.id == "db" { &["db"] } else { &[] }
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError> {
        if self.id == "broken" {
            return Err("broken on purpose".into());
        }
        let row = format!("{}:{}", self.id, self.config);
        self.applies.borrow_mut().push(row.clone());
        self.world.borrow_mut().insert(row.clone());
        let world = Rc::clone(&self.world);
        ctx.effect(move || {
            world.borrow_mut().remove(&row);
        });
        if self.id == "db" {
            ctx.provide(DB, "conn".to_owned())?;
        }
        Ok(())
    }
}

const KNOWN: [&str; 5] = ["db", "app", "cache", "cli", "broken"];

struct Fixture {
    runtime: Runtime,
    loader: Loader<u8>,
    world: World,
    applies: Applies,
}

impl Fixture {
    fn new() -> Self {
        Self {
            runtime: Runtime::new(),
            loader: Loader::new(),
            world: World::default(),
            applies: Applies::default(),
        }
    }

    fn reconcile(&mut self, entries: &[Entry<u8>]) -> Vec<LoadError> {
        let (world, applies) = (Rc::clone(&self.world), Rc::clone(&self.applies));
        self.loader
            .reconcile(&mut self.runtime, entries, move |id, config| {
                KNOWN.contains(&id).then(|| {
                    Rc::new(Unit {
                        id: id.to_owned(),
                        config: *config,
                        world: Rc::clone(&world),
                        applies: Rc::clone(&applies),
                    }) as Rc<dyn Component>
                })
            })
    }

    fn world(&self) -> Vec<String> {
        self.world.borrow().iter().cloned().collect()
    }

    fn status(&self, id: &str) -> Option<Status> {
        self.loader.status(&self.runtime, id)
    }
}

#[test]
fn loads_enabled_entries_and_reports_disabled_ones() {
    let mut fx = Fixture::new();
    let errors = fx.reconcile(&[Entry::new("cli", 0), Entry::new("cache", 0).disabled(true)]);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(fx.world(), ["cli:0"]);
    assert_eq!(fx.status("cli"), Some(Status::Active));
    assert_eq!(fx.status("cache"), Some(Status::Disabled));
    assert_eq!(fx.status("nope"), None);
}

#[test]
fn disabling_a_loaded_entry_reverts_exactly_its_effects() {
    let mut fx = Fixture::new();
    fx.reconcile(&[Entry::new("cli", 0), Entry::new("cache", 0)]);
    fx.reconcile(&[Entry::new("cli", 0), Entry::new("cache", 0).disabled(true)]);
    assert_eq!(fx.world(), ["cli:0"]);
    assert_eq!(
        fx.applies
            .borrow()
            .iter()
            .filter(|row| row.starts_with("cli"))
            .count(),
        1,
        "cli untouched"
    );
}

#[test]
fn a_config_change_rebuilds_only_that_entry() {
    let mut fx = Fixture::new();
    fx.reconcile(&[Entry::new("cli", 0), Entry::new("cache", 0)]);
    fx.reconcile(&[Entry::new("cli", 0), Entry::new("cache", 7)]);
    assert_eq!(fx.world(), ["cache:7", "cli:0"]);
    assert_eq!(*fx.applies.borrow(), ["cli:0", "cache:0", "cache:7"]);
}

#[test]
fn a_removed_entry_is_unloaded() {
    let mut fx = Fixture::new();
    fx.reconcile(&[Entry::new("cli", 0), Entry::new("cache", 0)]);
    fx.reconcile(&[Entry::new("cli", 0)]);
    assert_eq!(fx.world(), ["cli:0"]);
    assert_eq!(fx.status("cache"), None);
}

#[test]
fn unknown_and_duplicate_entries_are_reported_and_the_rest_loads() {
    let mut fx = Fixture::new();
    let errors = fx.reconcile(&[
        Entry::new("ghost", 0),
        Entry::new("cli", 1),
        Entry::new("cli", 2),
    ]);
    let messages: Vec<String> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages,
        [
            "component `cli` is listed twice",
            "unknown component `ghost`"
        ]
    );
    assert_eq!(fx.world(), ["cli:2"], "the last occurrence wins");
}

#[test]
fn disabling_a_provider_parks_its_consumer_until_re_enabled() {
    let mut fx = Fixture::new();
    fx.reconcile(&[Entry::new("db", 0), Entry::new("app", 0)]);
    assert_eq!(fx.status("app"), Some(Status::Active));
    fx.reconcile(&[Entry::new("db", 0).disabled(true), Entry::new("app", 0)]);
    assert_eq!(fx.status("app"), Some(Status::Waiting));
    assert!(fx.world().is_empty());
    fx.reconcile(&[Entry::new("db", 0), Entry::new("app", 0)]);
    assert_eq!(fx.status("app"), Some(Status::Active));
    assert_eq!(fx.world(), ["app:0", "db:0"]);
}

#[test]
fn a_failing_component_reports_failed_without_blocking_others() {
    let mut fx = Fixture::new();
    fx.reconcile(&[Entry::new("broken", 0), Entry::new("cli", 0)]);
    assert_eq!(fx.status("broken"), Some(Status::Failed));
    assert_eq!(fx.status("cli"), Some(Status::Active));
    assert_eq!(fx.runtime.take_failures().len(), 1);
    assert_eq!(
        fx.runtime.state(fx.loader.loaded["cli"].0),
        Some(FiberState::Active)
    );
}

#[test]
fn clear_unloads_everything() {
    let mut fx = Fixture::new();
    fx.reconcile(&[
        Entry::new("db", 0),
        Entry::new("app", 0),
        Entry::new("cli", 0),
    ]);
    fx.loader.clear(&mut fx.runtime);
    assert!(fx.world().is_empty());
}

fn entries() -> impl Strategy<Value = Vec<Entry<u8>>> {
    prop::collection::vec((0..4usize, any::<bool>(), 0..3u8), 0..5).prop_map(|rows| {
        rows.into_iter()
            .map(|(index, disabled, config)| Entry::new(KNOWN[index], config).disabled(disabled))
            .collect()
    })
}

proptest! {
    /// Paper §4.3 confluence, lifted to the loader: after any history of reconciles, the world
    /// equals a fresh reconcile of the last entry list; clearing empties it.
    #[test]
    fn reconcile_is_confluent(history in prop::collection::vec(entries(), 1..6)) {
        let mut fx = Fixture::new();
        for step in &history {
            fx.reconcile(step);
        }
        let mut fresh = Fixture::new();
        fresh.reconcile(history.last().expect("non-empty"));
        prop_assert_eq!(fx.world(), fresh.world());
        fx.loader.clear(&mut fx.runtime);
        prop_assert!(fx.world().is_empty());
    }
}
