//! The component loader (paper §5.2): a declarative list of entries reconciled into fibers.
//!
//! Each [`Entry`] names a component, says whether it is disabled, and carries its configuration.
//! [`Loader::reconcile`] turns the difference between the entries and what is loaded into the
//! least disruptive operations: plug what is new, unplug what was removed or disabled, rebuild
//! (unplug + plug) what changed configuration, and leave everything else untouched. This is sound
//! because of confluence: the quiescent state depends only on the final entries, never on the
//! order of past reconciles.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::{Component, Error, FiberId, Runtime};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry<C> {
    pub id: String,
    pub disabled: bool,
    pub config: C,
}

impl<C> Entry<C> {
    pub fn new(id: impl Into<String>, config: C) -> Self {
        Self {
            id: id.into(),
            disabled: false,
            config,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("unknown component `{0}`")]
    Unknown(String),
    #[error("component `{0}` is listed twice")]
    Duplicate(String),
    #[error("component `{id}`: {source}")]
    Plug {
        id: String,
        #[source]
        source: Error,
    },
}

/// Where an entry stands after a reconcile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Loaded and running.
    Active,
    /// Loaded, waiting for a dependency (or its parent) to be provided.
    Waiting,
    /// Disabled by its entry.
    Disabled,
    /// Loaded, but its `apply` failed and it was unwound (see [`Runtime::take_failures`]).
    Failed,
}

#[derive(Default)]
pub struct Loader<C> {
    loaded: BTreeMap<String, (FiberId, C)>,
    disabled: Vec<String>,
}

impl<C: Clone + PartialEq> Loader<C> {
    pub fn new() -> Self {
        Self {
            loaded: BTreeMap::new(),
            disabled: Vec::new(),
        }
    }

    /// Bring the runtime to `entries`. `build` makes the component for an id and its config, or
    /// `None` when the id is unknown. Errors are per entry: the rest of the list still loads.
    pub fn reconcile(
        &mut self,
        runtime: &mut Runtime,
        entries: &[Entry<C>],
        build: impl Fn(&str, &C) -> Option<Rc<dyn Component>>,
    ) -> Vec<LoadError> {
        let mut errors = Vec::new();
        let mut wanted: BTreeMap<&str, &Entry<C>> = BTreeMap::new();
        for entry in entries {
            if wanted.insert(&entry.id, entry).is_some() {
                errors.push(LoadError::Duplicate(entry.id.clone()));
            }
        }

        let stale: Vec<String> = self
            .loaded
            .iter()
            .filter(|(id, (_, config))| {
                wanted
                    .get(id.as_str())
                    .is_none_or(|entry| entry.disabled || entry.config != *config)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            if let Some((fiber, _)) = self.loaded.remove(&id) {
                runtime.unplug(fiber);
            }
        }

        self.disabled.clear();
        for entry in entries {
            if !wanted
                .get(entry.id.as_str())
                .is_some_and(|kept| std::ptr::eq(*kept, entry))
            {
                continue; // an earlier duplicate: the last occurrence wins
            }
            if entry.disabled {
                self.disabled.push(entry.id.clone());
                continue;
            }
            if self.loaded.contains_key(&entry.id) {
                continue;
            }
            let Some(component) = build(&entry.id, &entry.config) else {
                errors.push(LoadError::Unknown(entry.id.clone()));
                continue;
            };
            match runtime.plug_shared(component) {
                Ok(fiber) => {
                    self.loaded
                        .insert(entry.id.clone(), (fiber, entry.config.clone()));
                }
                Err(source) => errors.push(LoadError::Plug {
                    id: entry.id.clone(),
                    source,
                }),
            }
        }
        errors
    }

    /// The status of `id`, or `None` when the last reconcile did not list it (or it failed to plug).
    pub fn status(&self, runtime: &Runtime, id: &str) -> Option<Status> {
        if self.disabled.iter().any(|disabled| disabled == id) {
            return Some(Status::Disabled);
        }
        let (fiber, _) = self.loaded.get(id)?;
        Some(match runtime.state(*fiber) {
            Some(crate::FiberState::Active) => Status::Active,
            Some(crate::FiberState::Inactive) => Status::Waiting,
            // Unloading only lasts inside a settle; a failed fiber is unwound and collected.
            Some(crate::FiberState::Unloading) | None => Status::Failed,
        })
    }

    /// Unload everything this loader plugged.
    pub fn clear(&mut self, runtime: &mut Runtime) {
        for (_, (fiber, _)) in std::mem::take(&mut self.loaded) {
            runtime.unplug(fiber);
        }
        self.disabled.clear();
    }
}

#[cfg(test)]
mod tests;
