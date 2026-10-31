//! tx-core plugins `history` (transcript mirror, `history` listing) and `chat` (chat-id
//! capture, chat ops, prefix+f). Neither uses a core slot beyond verbs and resources; each
//! keeps bulk state under `<data>/plugins/<id>/` (`CoreConfig::plugin_data_dir`).

pub mod capture;
pub mod detach;
pub mod engines;
pub mod ext;
pub mod fork_pane;
pub mod history_verbs;
pub mod install;
pub mod messages;
pub mod mirror;
pub mod ops;

use std::io::Write;
use std::path::{Path, PathBuf};

use tx_client::tmux::UiError;
use tx_core::{CoreError, EngineError, Plugin, PluginError, Registry, VerbError};

pub const HISTORY_ID: &str = "history";
pub const CHAT_ID: &str = "chat";

/// `create_dir_all`, then mode 0755 on the leaf.
pub fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
}

/// Temp file in the destination directory, fsync, rename: a reader never sees half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[derive(Debug, thiserror::Error)]
pub enum ChatError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{op}: session '{token}' not found")]
    NotFound { op: String, token: String },
    #[error("{op}: '{name}' has no {missing}")]
    NoChat {
        op: String,
        name: String,
        missing: String,
    },
    #[error("session '{0}' is not an agent session")]
    NotAnAgent(String),
    #[error("no engine adapter for '{0}'")]
    EngineUnknown(String),
    #[error("ext.{plugin} of '{name}' is malformed: {reason}")]
    Malformed {
        plugin: String,
        name: String,
        reason: String,
    },
    #[error("resume: a live session named '{0}' already exists — pass --as <new-name>")]
    LiveName(String),
    #[error("resume: cwd '{cwd}' does not exist — {remedy} (C8)")]
    CwdMissing { cwd: String, remedy: String },
    #[error(transparent)]
    Ui(#[from] UiError),
}

impl From<ChatError> for VerbError {
    fn from(error: ChatError) -> Self {
        match error {
            ChatError::Core(error) => VerbError::Core(error),
            other => VerbError::Failed(other.to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HistoryPlugin;

impl Plugin for HistoryPlugin {
    fn id(&self) -> &'static str {
        HISTORY_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        for verb in history_verbs::verbs(r.config().plugin_data_dir(HISTORY_ID)) {
            r.verb(verb);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ChatPlugin;

impl Plugin for ChatPlugin {
    fn id(&self) -> &'static str {
        CHAT_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        for verb in capture::verbs()
            .into_iter()
            .chain(ops::verbs())
            .chain(fork_pane::verbs())
        {
            r.verb(verb);
        }
        r.resource("keys/chat.tmux", include_bytes!("../tmux/keys.tmux"))?;
        r.resource(
            "bin/tmux-fork-tx-session",
            include_bytes!("../bin/tmux-fork-tx-session"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("plugins/history");
        ensure_dir(&data).unwrap();
        let file = data.join("x.json");
        write_atomic(&file, b"one").unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        assert_eq!(std::fs::read_dir(&data).unwrap().count(), 1, "no temp left");
    }

    #[test]
    fn chat_error_maps_to_verb_error() {
        let failed: VerbError = ChatError::LiveName("x".into()).into();
        assert!(matches!(failed, VerbError::Failed(_)));
        let core: VerbError = ChatError::Core(CoreError::NotFound("x".into())).into();
        assert!(matches!(core, VerbError::Core(CoreError::NotFound(_))));
    }
}
