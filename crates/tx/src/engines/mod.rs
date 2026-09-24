//! Engine adapters (lib/tx/engines): the [`EngineAdapter`] trait, the [`EngineRegistry`] table and
//! the Claude / Codex adapters. Adapters are registered explicitly by the wiring (no import-time
//! self-registration).

pub mod adapter;
pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod codex_rollout;
pub mod codex_update;
pub mod registry;

pub use adapter::{
    CapturedChat, DEFAULT_EFFORT, Effort, EngineAdapter, EngineError, LaunchEnv, LaunchOptions,
    StateSource,
};
pub use antigravity::AntigravityEngine;
pub use claude::ClaudeEngine;
pub use codex::{CodexEngine, CodexHostEnv};
pub use registry::{EngineRegistry, RegistrationId};
