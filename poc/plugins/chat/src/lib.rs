//! tx-core plugin `chat`: prefix+f (`_fork-pane`), `chat ls` (a session's conversation, asked
//! from its engine) and `doctor --fix-hooks` (removes the global hooks older versions installed).
//! Fork and respawn are core's built-in verbs; conversations live in the engines' own data.

pub mod fork_pane;
pub mod hooks_cleanup;
pub mod messages;
pub mod verbs;

use std::path::Path;

use tx_core::{Plugin, PluginError, Registry};

pub const CHAT_ID: &str = "chat";

/// `create_dir_all`, then mode 0755 on the leaf.
pub fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ChatPlugin;

impl Plugin for ChatPlugin {
    fn id(&self) -> &'static str {
        CHAT_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        for verb in verbs::verbs().into_iter().chain(fork_pane::verbs()) {
            r.verb(verb);
        }
        r.resource("keys/chat.tmux", include_bytes!("../tmux/keys.tmux"))?;
        r.resource(
            "bin/tmux-fork-tx-session",
            include_bytes!("../bin/tmux-fork-tx-session"),
        )
    }
}
