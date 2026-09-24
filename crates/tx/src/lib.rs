use std::ffi::OsString;

pub mod app;
pub mod argparse;
pub mod artifact;
pub mod artifact_store;
pub mod engines;
pub mod events;
pub mod grouping;
pub mod history;
pub mod messages;
pub mod migrations;
pub mod palette;
pub mod plugins;
pub mod pyjson;
pub mod read_only;
pub mod reconcile;
pub mod render;
pub mod roles;
pub mod service;
pub mod session;
pub mod session_editor;
pub mod shlex;
pub mod skills;
pub mod spawn;
pub mod storage;
pub mod store;
pub mod sync;
pub mod tmux;
pub mod verbs;
pub mod worktree;

pub fn run(args: Vec<OsString>) -> i32 {
    app::run(args)
}
