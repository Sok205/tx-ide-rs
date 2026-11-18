//! The tx client: the tmux and nvim library the IDE's plugins share: the socket-bound tmux
//! helper, the picker renderer and palette, the modal editor, the fzf prompt and the `views`
//! store. Views are the non-LLM tmux sessions on the core's private server (`views`). It
//! registers no plugin: the `ide` plugin, which composes the server's config and owns
//! `ui attach` and `ui view`, is the package `tx-ide` (`plugins/ide`).

pub mod editor;
pub mod palette;
pub mod prompt;
pub mod render;
pub mod tmux;
pub mod views;
