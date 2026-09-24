use std::ffi::OsString;

pub mod argparse;
pub mod artifact;
pub mod artifact_store;
pub mod events;
pub mod palette;
pub mod pyjson;
pub mod read_only;
pub mod roles;
pub mod session;
pub mod shlex;
pub mod skills;
pub mod storage;
pub mod store;
pub mod tmux;
pub mod worktree;

pub fn run(_args: Vec<OsString>) -> i32 {
    eprintln!("tx: not implemented yet");
    2
}
