use std::ffi::OsString;

pub mod argparse;
pub mod pyjson;

pub fn run(_args: Vec<OsString>) -> i32 {
    eprintln!("tx: not implemented yet");
    2
}
