//! The one error type every public operation returns.

use std::io;
use std::path::PathBuf;

use crate::engine::EngineError;
use crate::profile::RoleError;
use crate::session::SessionId;
use crate::store::StoreError;
use crate::tmux::TmuxError;
use crate::worktree::WorktreeError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CoreError {
    #[error("no session {0:?}")]
    NotFound(String),
    #[error("a running session is already named {0:?}")]
    NameTaken(String),
    #[error("invalid session name {0:?} (allowed: [A-Za-z0-9_-]+, at most {max} bytes)", max = crate::session::MAX_NAME_BYTES)]
    InvalidName(String),
    #[error("invalid environment key {0:?} (a name without '=' or NUL)")]
    InvalidEnvKey(String),
    #[error("{} is not a directory", .0.display())]
    NotADirectory(PathBuf),
    #[error("session {0} is not running")]
    NotRunning(SessionId),
    #[error("no engine registered as {0:?}")]
    UnknownEngine(String),
    #[error("an agent needs an initial prompt")]
    PromptRequired,
    #[error("an engine with id {0:?} is already registered")]
    EngineTaken(String),
    #[error("an engine owning the ext key {0:?} is already registered")]
    ExtKeyTaken(String),
    #[error("invalid ext key {0:?}: use [a-z0-9-]+")]
    InvalidExtKey(String),
    #[error("tmux socket path is {len} bytes, longer than the {max} allowed: {path}", len = .0.as_os_str().len(), max = crate::config::MAX_SOCKET_PATH_BYTES, path = .0.display())]
    SocketPathTooLong(PathBuf),
    #[error("tmux socket path must be absolute: {}", .0.display())]
    SocketPathNotAbsolute(PathBuf),
    #[error(transparent)]
    Profile(#[from] RoleError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Tmux(#[from] TmuxError),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error("lock: {0}")]
    Lock(io::Error),
}
