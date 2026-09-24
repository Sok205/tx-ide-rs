//! The engine table (registry.py) — a plain value, not a process-wide singleton.
//!
//! Registration hands back a [`RegistrationId`] that removes exactly that row, so a Cordis
//! component can register its adapter as an effect whose undo is `unregister(id)` (a commutative
//! table with unique row ids, `ARCHITECTURE.md`). Several rows for one engine may coexist: the
//! most recent registration wins (the reference's "last registration wins"), and unregistering it
//! uncovers the previous one.

use std::rc::Rc;

use crate::session::Engine;

use super::adapter::EngineAdapter;

/// The handle of one registered row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegistrationId(u64);

struct Row {
    id: RegistrationId,
    engine: Engine,
    adapter: Rc<dyn EngineAdapter>,
}

#[derive(Default)]
pub struct EngineRegistry {
    rows: Vec<Row>,
    next_id: u64,
}

impl EngineRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, engine: Engine, adapter: Rc<dyn EngineAdapter>) -> RegistrationId {
        let id = RegistrationId(self.next_id);
        self.next_id += 1;
        self.rows.push(Row {
            id,
            engine,
            adapter,
        });
        id
    }

    /// Remove exactly the row `id` registered; `None` if it is already gone.
    pub fn unregister(&mut self, id: RegistrationId) -> Option<Rc<dyn EngineAdapter>> {
        let index = self.rows.iter().position(|row| row.id == id)?;
        Some(self.rows.remove(index).adapter)
    }

    /// The adapter for `engine`, or `None` when none is registered (no fallback, by design).
    pub fn get(&self, engine: Engine) -> Option<Rc<dyn EngineAdapter>> {
        self.effective()
            .find(|row| row.engine == engine)
            .map(|row| Rc::clone(&row.adapter))
    }

    /// The engines with an adapter, in first-registration order.
    pub fn registered(&self) -> Vec<Engine> {
        let mut engines: Vec<Engine> = Vec::new();
        for row in &self.rows {
            if !engines.contains(&row.engine) {
                engines.push(row.engine);
            }
        }
        engines
    }

    /// The registered engine whose binary `command` invokes, or `None`.
    pub fn engine_for_command(&self, command: &str) -> Option<Engine> {
        self.registered().into_iter().find(|&engine| {
            self.get(engine)
                .is_some_and(|adapter| adapter.matches_binary(command))
        })
    }

    /// The winning row per engine: newest first.
    fn effective(&self) -> impl Iterator<Item = &Row> {
        self.rows.iter().rev()
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::*;
    use crate::engines::adapter::{
        CapturedChat, EngineError, LaunchEnv, LaunchOptions, StateSource,
    };
    use crate::session::State;

    struct Fake(&'static str);

    impl EngineAdapter for Fake {
        fn binary(&self) -> &str {
            self.0
        }
        fn capture_session_id(&self, _: &Value) -> Option<CapturedChat> {
            None
        }
        fn build_launch_command(
            &self,
            _: &LaunchOptions<'_>,
            _: &mut LaunchEnv,
        ) -> Result<Vec<String>, EngineError> {
            Ok(vec![self.0.to_owned()])
        }
        fn resume_command(
            &self,
            _: &str,
            _: Option<&str>,
            _: bool,
        ) -> Result<Vec<String>, EngineError> {
            Ok(vec![])
        }
        fn fork_command(&self, _: &str, _: &str, _: bool) -> Result<Vec<String>, EngineError> {
            Ok(vec![])
        }
        fn seed_command(&self, _: &str, _: &str, _: bool) -> Result<Vec<String>, EngineError> {
            Ok(vec![])
        }
        fn distiller_command(&self, _: &str) -> Vec<String> {
            vec![]
        }
        fn prepare_chat_for_cwd(&self, _: &str, _: &str, _: &str) -> Result<(), EngineError> {
            Ok(())
        }
        fn prepare_workspace(
            &self,
            command: &str,
            _: &str,
            _: &[(String, String)],
        ) -> Result<String, EngineError> {
            Ok(command.to_owned())
        }
        fn is_read_only_command(&self, _: &str) -> bool {
            false
        }
        fn resolve_transcript(&self, _: &str, _: &str) -> PathBuf {
            PathBuf::new()
        }
        fn iter_messages(&self, _: &Path) -> Result<Vec<Value>, EngineError> {
            Ok(vec![])
        }
        fn bundle_sidecars(&self, _: &Path, _: &str) -> Vec<PathBuf> {
            vec![]
        }
        fn event_to_state(&self) -> &'static [(&'static str, State)] {
            &[]
        }
        fn state_source(&self) -> StateSource {
            StateSource::HookEvents
        }
    }

    fn binary_of(registry: &EngineRegistry, engine: Engine) -> Option<String> {
        registry.get(engine).map(|a| a.binary().to_owned())
    }

    #[test]
    fn get_and_registered() {
        let mut registry = EngineRegistry::new();
        assert!(registry.get(Engine::Claude).is_none());
        registry.register(Engine::Codex, Rc::new(Fake("codex")));
        registry.register(Engine::Claude, Rc::new(Fake("claude")));
        assert_eq!(registry.registered(), [Engine::Codex, Engine::Claude]);
        assert_eq!(
            binary_of(&registry, Engine::Claude).as_deref(),
            Some("claude")
        );
    }

    #[test]
    fn last_registration_wins_and_unregister_removes_exactly_that_row() {
        let mut registry = EngineRegistry::new();
        let first = registry.register(Engine::Claude, Rc::new(Fake("claude")));
        let second = registry.register(Engine::Claude, Rc::new(Fake("other")));
        assert_eq!(
            binary_of(&registry, Engine::Claude).as_deref(),
            Some("other")
        );
        assert!(registry.unregister(second).is_some());
        assert!(registry.unregister(second).is_none());
        assert_eq!(
            binary_of(&registry, Engine::Claude).as_deref(),
            Some("claude")
        );
        registry.unregister(first);
        assert!(registry.get(Engine::Claude).is_none());
        assert!(registry.registered().is_empty());
    }

    #[test]
    fn engine_for_command_matches_the_first_token_basename() {
        let mut registry = EngineRegistry::new();
        registry.register(Engine::Claude, Rc::new(Fake("claude")));
        registry.register(Engine::Codex, Rc::new(Fake("codex")));
        let cases = [
            ("codex -m x", Some(Engine::Codex)),
            ("/usr/local/bin/claude --model x", Some(Engine::Claude)),
            ("  claude", Some(Engine::Claude)),
            ("env X=1 claude", None),
            ("bash", None),
            ("", None),
            ("claudex", None),
        ];
        for (command, expected) in cases {
            assert_eq!(registry.engine_for_command(command), expected, "{command}");
        }
    }
}
