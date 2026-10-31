//! tx-core: run and manage a swarm of interactive agents, each a process in a detached session
//! on tx's own tmux server. See `docs/superpowers/specs/2026-10-15-tx-core-design.md`.

pub mod config;
pub mod core;
pub mod engine;
pub mod error;
pub mod events;
/// Internal; no stability promise for plugin authors.
#[doc(hidden)]
pub mod locks;
/// Internal; no stability promise for plugin authors.
#[doc(hidden)]
pub mod messaging;
pub mod migrate;
pub mod plugin;
pub mod profile;
pub mod session;
pub mod state;
/// Internal; no stability promise for plugin authors.
#[doc(hidden)]
pub mod store;
/// Internal; no stability promise for plugin authors.
#[doc(hidden)]
pub mod tmux;
pub mod worktree;

pub use config::{CoreConfig, Directories, MAX_SOCKET_PATH_BYTES};
pub use core::{
    CreateOptions, DeliveryReceipt, ExecutionLocation, LaunchOptions, ListFilter, RefreshResult,
    SendOptions, TxCore,
};
#[doc(hidden)]
pub use engine::EngineRegistry;
pub use engine::{
    EngineError, LaunchMode, LaunchPlan, LaunchRequest, LlmEngine, WorkspaceAction, engine_data,
    hook_shell_command,
};
pub use error::CoreError;
#[doc(hidden)]
pub use events::EventLog;
pub use events::{Event, EventSink, EventType};
#[doc(hidden)]
pub use locks::{LockGuard, Locks};
#[doc(hidden)]
pub use messaging::DELIVERY_PAUSE;
/// The default envelope; an `LlmEngine::envelope` override may wrap it.
pub use messaging::build_envelope;
pub use plugin::{
    Plugin, PluginConfig, PluginError, PluginStatus, Registry, Resource, Verb, VerbError, VerbIo,
};
pub use profile::{Profile, Profiles, ResolvedProfile, RoleError};
pub use session::{Launch, Lifecycle, RecordError, Session, SessionId, validate_name};
pub use state::{LlmEngineChange, LlmEngineHook, LlmEngineState};
#[doc(hidden)]
pub use store::SessionStore;
/// Stable: they are the payload of `CoreError::Store`.
pub use store::{ReadError, StoreError};
/// Stable: it is the payload of `CoreError::Tmux`.
pub use tmux::TmuxError;
#[doc(hidden)]
pub use tmux::{Live, LiveTable, Tmux};
/// Stable: `WorktreeError` is the payload of `CoreError::Worktree`.
pub use worktree::{WorktreeError, WorktreeManager};
