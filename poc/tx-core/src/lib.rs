//! tx-core: run and manage a swarm of interactive agents, each a process in a detached session
//! on tx's own tmux server. See `docs/superpowers/specs/2026-10-15-tx-core-design.md`.

pub mod config;
pub mod core;
pub mod session;
/// Internal; no stability promise for plugin authors.
#[doc(hidden)]
pub mod store;
/// Internal; no stability promise for plugin authors.
#[doc(hidden)]
pub mod tmux;
pub mod engine;
pub mod error;
pub mod events;
/// Internal; no stability promise for plugin authors.
#[doc(hidden)]
pub mod locks;
/// Internal; no stability promise for plugin authors.
#[doc(hidden)]
pub mod messaging;
pub mod profile;

pub use core::{CreateOptions, DeliveryReceipt, ExecutionLocation, LaunchOptions, ListFilter, RefreshResult, SendOptions, TxCore};
pub use config::{CoreConfig, MAX_SOCKET_PATH_BYTES};
pub use engine::{EngineAdapter, EngineError, LaunchPlan, LaunchRequest, WorkspaceAction};
#[doc(hidden)]
pub use engine::EngineRegistry;
pub use error::CoreError;
pub use events::{Event, EventSink, EventType};
#[doc(hidden)]
pub use events::EventLog;
#[doc(hidden)]
pub use locks::{LockGuard, Locks};
#[doc(hidden)]
pub use tmux::{Live, LiveTable, Tmux};
/// Stable: it is the payload of `CoreError::Tmux`.
pub use tmux::TmuxError;
/// The default envelope; an `EngineAdapter::envelope` override may wrap it.
pub use messaging::build_envelope;
#[doc(hidden)]
pub use messaging::DELIVERY_PAUSE;
pub use profile::{Profile, Profiles, ResolvedProfile, RoleError};
#[doc(hidden)]
pub use store::SessionStore;
/// Stable: they are the payload of `CoreError::Store`.
pub use store::{ReadError, StoreError};
pub use session::{Effort, Launch, Lifecycle, RecordError, Session, SessionId, validate_name};
