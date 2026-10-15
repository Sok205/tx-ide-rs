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
pub use engine::{EngineAdapter, EngineError, EngineRegistry, LaunchPlan, LaunchRequest, WorkspaceAction};
pub use error::CoreError;
pub use events::{Event, EventLog, EventSink, EventType};
pub use locks::{LockGuard, Locks};
pub use tmux::{Live, LiveTable, Tmux, TmuxError};
pub use messaging::{DELIVERY_PAUSE, build_envelope};
pub use profile::{Profile, Profiles, ResolvedProfile, RoleError};
pub use store::{ReadError, SessionStore, StoreError};
pub use session::{Effort, Launch, Lifecycle, RecordError, Session, SessionId, validate_name};
