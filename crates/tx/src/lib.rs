use std::ffi::OsString;

pub mod app;
pub mod argparse;
pub mod artifact;
pub mod artifact_store;
pub mod engines;
pub mod events;
pub mod grouping;
pub mod palette;
pub mod pyjson;
pub mod read_only;
pub mod render;
pub mod roles;
pub mod session;
pub mod session_editor;
pub mod shlex;
pub mod skills;
pub mod storage;
pub mod store;
pub mod sync;
pub mod tmux;
pub mod worktree;

pub mod plugins;
pub mod verbs;

pub fn run(args: Vec<OsString>) -> i32 {
    app::run(args)
}
