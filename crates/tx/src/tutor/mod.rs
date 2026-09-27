//! `tx tutor`: lesson packs, progress and checks for the vimtutor-style walkthrough.

pub mod pack;
pub mod progress;

use std::path::PathBuf;

use crate::storage::Home;

/// The tutor view's nvim `--listen` socket.
pub fn socket_path(home: &Home, lang: &str) -> PathBuf {
    home.root().join("tutor").join(format!("{lang}.sock"))
}
