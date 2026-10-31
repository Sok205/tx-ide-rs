//! `txc` (the CLI people type) and `txi` (the internal entry point machines run) share this
//! library: argument parsing, the plugin switchboard and opening the core.

pub mod args;
pub mod plugins;
pub mod setup;
