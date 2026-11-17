//! What every program that runs as tx shares, so that a plugin program resolves the directories
//! and reads `config.yaml` exactly as `tx` does: the global flags, the config file and opening the
//! core. It depends on `tx-core` alone, never on a plugin.

pub mod args;
pub mod config;
pub mod setup;
