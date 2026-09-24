//! A synchronous runtime for spatiotemporal composability, after Shi, Zhang, Cui (Cordis).
//! See `docs/cordis-paradigm.md` for the paradigm and `docs/ARCHITECTURE.md` for how `tx` uses it.
//!
//! A [`Component`] declares the keys it injects and provides and an `apply` that performs its
//! effects through a [`Ctx`]. Every effect records its own undo, so unloading a fiber reverts
//! exactly what it did (temporal composability). The service container is derived from the
//! tables of `Active` fibers and every fiber re-resolves its dependencies whenever it changes
//! (spatial composability). `apply` runs to completion, so the paper's `Reloading` state is
//! transient and not represented; `L-Divert` becomes `L-Leave` right after a finished apply.

use std::any::Any;
use std::collections::BTreeMap;
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

pub type BoxError = Box<dyn std::error::Error>;

/// A typed service key. Linking is by `name`; `T` is the value type every provider must bind.
pub struct Key<T: 'static> {
    pub name: &'static str,
    _value: PhantomData<fn() -> T>,
}

impl<T: 'static> Key<T> {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            _value: PhantomData,
        }
    }
}

impl<T: 'static> Clone for Key<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: 'static> Copy for Key<T> {}

impl<T: 'static> fmt::Debug for Key<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Key({})", self.name)
    }
}

pub trait Component: 'static {
    fn name(&self) -> &str;
    /// Keys this component needs: it activates only when all are provided.
    fn inject(&self) -> &[&'static str] {
        &[]
    }
    /// Keys this component may bind. At most one fiber may provide a key.
    fn provide(&self) -> &[&'static str] {
        &[]
    }
    fn apply(&self, ctx: &mut Ctx<'_>) -> Result<(), BoxError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FiberId(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FiberState {
    Inactive,
    Active,
    Unloading,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{fiber}: key `{key}` was not declared in inject/provide")]
    UndeclaredAccess { fiber: String, key: &'static str },
    #[error("{fiber}: key `{key}` has no active provider")]
    InactiveAccess { fiber: String, key: &'static str },
    #[error("key `{key}` holds a value of another type")]
    TypeMismatch { key: &'static str },
    #[error("key `{key}` is already provided by `{by}`")]
    ProvisionConflict { key: &'static str, by: String },
    #[error("{fiber}: key `{key}` provided twice")]
    DoubleProvision { fiber: String, key: &'static str },
}

enum Undo {
    Custom(Box<dyn FnOnce()>),
    Retire(FiberId),
}

struct Fiber {
    component: Rc<dyn Component>,
    parent: Option<FiberId>,
    retired: bool,
    failed: bool,
    state: FiberState,
    table: BTreeMap<&'static str, Rc<dyn Any>>,
    committed: BTreeMap<&'static str, FiberId>,
    dispose: Vec<Undo>,
}

/// The fiber registry: every loaded component instance and its lifecycle state.
#[derive(Default)]
pub struct Runtime {
    fibers: BTreeMap<FiberId, Fiber>,
    next_id: u64,
    failures: Vec<(String, BoxError)>,
}

impl Runtime {
    pub fn new() -> Self {
        Self::default()
    }

    /// `O-Insert` + settle: load a root component. Rejects a provision overlap up front.
    pub fn plug(&mut self, component: impl Component) -> Result<FiberId, Error> {
        let id = self.insert(Rc::new(component), None)?;
        self.settle();
        Ok(id)
    }

    /// `O-Retire` + settle: unload a fiber (and, through its undo stack, its children).
    pub fn unplug(&mut self, id: FiberId) {
        if let Some(fiber) = self.fibers.get_mut(&id) {
            fiber.retired = true;
        }
        self.settle();
    }

    /// Retire every root and settle: the whole world is reverted, consumers before providers.
    pub fn shutdown(&mut self) {
        let roots: Vec<_> = self
            .fibers
            .iter()
            .filter(|(_, f)| f.parent.is_none())
            .map(|(id, _)| *id)
            .collect();
        for id in roots {
            if let Some(fiber) = self.fibers.get_mut(&id) {
                fiber.retired = true;
            }
        }
        self.settle();
    }

    pub fn state(&self, id: FiberId) -> Option<FiberState> {
        self.fibers.get(&id).map(|f| f.state)
    }

    /// The current value at `key`, from the `Active` fiber that provides it.
    pub fn get<T: 'static>(&self, key: Key<T>) -> Option<Rc<T>> {
        let provider = self.provider(key.name)?;
        self.fibers[&provider]
            .table
            .get(key.name)?
            .clone()
            .downcast()
            .ok()
    }

    /// Errors raised by `apply` since the last call. A failed fiber is unwound and not retried.
    pub fn take_failures(&mut self) -> Vec<(String, BoxError)> {
        std::mem::take(&mut self.failures)
    }

    fn insert(
        &mut self,
        component: Rc<dyn Component>,
        parent: Option<FiberId>,
    ) -> Result<FiberId, Error> {
        for key in component.provide() {
            if let Some(other) = self
                .fibers
                .values()
                .find(|f| !f.retired && f.component.provide().contains(key))
            {
                return Err(Error::ProvisionConflict {
                    key,
                    by: other.component.name().to_owned(),
                });
            }
        }
        let id = FiberId(self.next_id);
        self.next_id += 1;
        self.fibers.insert(
            id,
            Fiber {
                component,
                parent,
                retired: false,
                failed: false,
                state: FiberState::Inactive,
                table: BTreeMap::new(),
                committed: BTreeMap::new(),
                dispose: Vec::new(),
            },
        );
        Ok(id)
    }

    fn provider(&self, key: &str) -> Option<FiberId> {
        self.fibers
            .iter()
            .find(|(_, f)| f.state == FiberState::Active && f.table.contains_key(key))
            .map(|(id, _)| *id)
    }

    /// `target(n)`: the provider of each injected key right now, or `None` (⊥) when retired,
    /// failed, orphaned, or unsatisfiable.
    fn target(&self, id: FiberId) -> Option<BTreeMap<&'static str, FiberId>> {
        let fiber = &self.fibers[&id];
        if fiber.retired || fiber.failed {
            return None;
        }
        if let Some(parent) = fiber.parent
            && self
                .fibers
                .get(&parent)
                .is_none_or(|p| p.state != FiberState::Active)
        {
            return None;
        }
        fiber
            .component
            .inject()
            .iter()
            .map(|key| Some((*key, self.provider(key)?)))
            .collect()
    }

    fn relied(&self, id: FiberId) -> bool {
        self.fibers.iter().any(|(other, f)| {
            *other != id
                && f.state != FiberState::Inactive
                && f.committed.values().any(|p| *p == id)
        })
    }

    /// Apply lifecycle rules until quiescent (`L-Begin`/`L-Finish`, `L-Leave`, `L-Unload`,
    /// then garbage-collect vestigial fibers with `O-Remove`).
    fn settle(&mut self) {
        loop {
            let mut changed = false;
            let ids: Vec<_> = self.fibers.keys().copied().collect();
            for id in ids {
                let Some(fiber) = self.fibers.get(&id) else {
                    continue;
                };
                match fiber.state {
                    FiberState::Inactive => {
                        if let Some(target) = self.target(id) {
                            self.load(id, target);
                            changed = true;
                        }
                    }
                    FiberState::Active => {
                        if self.target(id).as_ref() != Some(&fiber.committed) {
                            self.fibers.get_mut(&id).expect("present").state =
                                FiberState::Unloading;
                            changed = true;
                        }
                    }
                    FiberState::Unloading => {
                        if !self.relied(id) {
                            self.unload(id);
                            changed = true;
                        }
                    }
                }
            }
            let vestigial: Vec<_> = self
                .fibers
                .iter()
                .filter(|(id, f)| {
                    f.retired
                        && f.state == FiberState::Inactive
                        && f.table.is_empty()
                        && !self.fibers.values().any(|c| c.parent == Some(**id))
                })
                .map(|(id, _)| *id)
                .collect();
            changed |= !vestigial.is_empty();
            for id in vestigial {
                self.fibers.remove(&id);
            }
            if !changed {
                return;
            }
        }
    }

    fn load(&mut self, id: FiberId, target: BTreeMap<&'static str, FiberId>) {
        let fiber = self.fibers.get_mut(&id).expect("present");
        fiber.committed = target;
        fiber.state = FiberState::Active;
        let component = Rc::clone(&fiber.component);
        let mut ctx = Ctx {
            runtime: self,
            fiber: id,
        };
        if let Err(error) = component.apply(&mut ctx) {
            let fiber = self.fibers.get_mut(&id).expect("present");
            fiber.failed = true;
            // Retired too: the orchestrator is told through `take_failures`, and the key is freed
            // for a replacement once the unwound fiber is garbage-collected.
            fiber.retired = true;
            fiber.state = FiberState::Unloading;
            self.failures.push((component.name().to_owned(), error));
        }
    }

    /// `L-Unload`: run the undo stack LIFO, drop the table and the committed view.
    fn unload(&mut self, id: FiberId) {
        let fiber = self.fibers.get_mut(&id).expect("present");
        let undos = std::mem::take(&mut fiber.dispose);
        fiber.table.clear();
        fiber.committed.clear();
        fiber.state = FiberState::Inactive;
        for undo in undos.into_iter().rev() {
            match undo {
                Undo::Custom(undo) => undo(),
                Undo::Retire(child) => {
                    if let Some(child) = self.fibers.get_mut(&child) {
                        child.retired = true;
                    }
                }
            }
        }
    }
}

/// The unified context handed to `apply`: the only way a component touches the world.
pub struct Ctx<'r> {
    runtime: &'r mut Runtime,
    fiber: FiberId,
}

impl Ctx<'_> {
    fn this(&self) -> &Fiber {
        &self.runtime.fibers[&self.fiber]
    }

    /// Read a dependency through the committed view (valid during this fiber's whole episode).
    pub fn get<T: 'static>(&self, key: Key<T>) -> Result<Rc<T>, Error> {
        let fiber = self.this();
        let name = fiber.component.name().to_owned();
        let value = if let Some(provider) = fiber.committed.get(key.name) {
            self.runtime.fibers[provider].table.get(key.name)
        } else if fiber.component.provide().contains(&key.name) {
            fiber.table.get(key.name)
        } else {
            return Err(Error::UndeclaredAccess {
                fiber: name,
                key: key.name,
            });
        };
        let value = value.ok_or(Error::InactiveAccess {
            fiber: name,
            key: key.name,
        })?;
        Rc::clone(value)
            .downcast()
            .map_err(|_| Error::TypeMismatch { key: key.name })
    }

    /// Bind a declared key. The undo (unbinding) is implied: unloading clears the table.
    pub fn provide<T: 'static>(&mut self, key: Key<T>, value: T) -> Result<(), Error> {
        let fiber = self.runtime.fibers.get_mut(&self.fiber).expect("present");
        let name = fiber.component.name().to_owned();
        if !fiber.component.provide().contains(&key.name) {
            return Err(Error::UndeclaredAccess {
                fiber: name,
                key: key.name,
            });
        }
        if fiber.table.insert(key.name, Rc::new(value)).is_some() {
            return Err(Error::DoubleProvision {
                fiber: name,
                key: key.name,
            });
        }
        Ok(())
    }

    /// Record a revertible effect: `undo` runs when this fiber unloads, in LIFO order.
    pub fn effect(&mut self, undo: impl FnOnce() + 'static) {
        self.runtime
            .fibers
            .get_mut(&self.fiber)
            .expect("present")
            .dispose
            .push(Undo::Custom(Box::new(undo)));
    }

    /// Instantiate a child component. Its undo retires the child, so unloading cascades.
    pub fn plug(&mut self, component: impl Component) -> Result<FiberId, Error> {
        let child = self.runtime.insert(Rc::new(component), Some(self.fiber))?;
        self.runtime
            .fibers
            .get_mut(&self.fiber)
            .expect("present")
            .dispose
            .push(Undo::Retire(child));
        Ok(child)
    }
}

#[cfg(test)]
mod tests;
