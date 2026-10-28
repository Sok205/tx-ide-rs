//! tx-core plugins `history` (transcript mirror, `history` listing) and `chat` (chat-id
//! capture, chat ops, prefix+f). Neither uses a core slot beyond verbs and resources; each
//! keeps bulk state under `<state_dir>/plugins/<id>/`.

pub mod capture;
pub mod detach;
pub mod engines;
pub mod ext;
pub mod fork_pane;
pub mod history_verbs;
pub mod messages;
pub mod mirror;
pub mod ops;

use std::path::{Path, PathBuf};

use tx_core::{CoreError, EngineError, Plugin, PluginError, Registry, VerbError};
use tx_ui::tmux::UiError;

pub const HISTORY_ID: &str = "history";
pub const CHAT_ID: &str = "chat";

/// `create_dir_all`, then mode 0755 on the leaf.
pub fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
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
    #[error("op {op_id}: cannot read {}: {source}", path.display())]
    SpecUnreadable {
        op_id: String,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("op {op_id}: invalid spec: {reason}")]
    SpecInvalid { op_id: String, reason: String },
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
    #[error("no pane for {0}")]
    NoPane(String),
    #[error("not inside tmux (TMUX_PANE is unset)")]
    NotInsideTmux,
    #[error(transparent)]
    Ui(#[from] UiError),
}

/// `.map_err(io_err(path))`: an io failure at `path`.
pub(crate) fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> ChatError + '_ {
    move |source| ChatError::Io {
        path: path.to_owned(),
        source,
    }
}

impl From<ChatError> for VerbError {
    fn from(error: ChatError) -> Self {
        match error {
            ChatError::Core(error) => VerbError::Core(error),
            other => VerbError::other(other),
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
        for verb in history_verbs::verbs(r.config().plugin_dir(HISTORY_ID)) {
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
    fn plugin_dir_is_pure() {
        let dir = tempfile::tempdir().unwrap();
        let data = tx_core::CoreConfig::new(dir.path()).plugin_dir(HISTORY_ID);
        assert_eq!(data, dir.path().join("plugins/history"));
        assert!(!data.exists());
        ensure_dir(&data).unwrap();
        assert!(data.is_dir());
    }

    #[test]
    fn chat_error_maps_to_verb_error() {
        let failed: VerbError = ChatError::NotInsideTmux.into();
        assert!(matches!(failed, VerbError::Other(_)));
        let core: VerbError = ChatError::Core(CoreError::NotFound("x".into())).into();
        assert!(matches!(core, VerbError::Core(CoreError::NotFound(_))));
    }
}
