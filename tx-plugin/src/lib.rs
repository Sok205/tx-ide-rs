//! What every program that runs as tx shares, so that a plugin program resolves the directories
//! and reads `config.yaml` exactly as `tx` does: the global flags, the config file and opening the
//! core. For a plugin written in Rust it is the SDK: [`sdk::main`] is the whole `main` of its
//! program. It depends on `tx-core` alone, never on a plugin.

pub mod args;
pub mod bundle;
pub mod config;
pub mod engine;
pub mod sdk;
pub mod setup;
pub mod testing;

/// The version of this tx. The plugin index is trusted only by the version that wrote it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The built-in verbs of `tx`. A plugin's verb of one of these names is never run: an installed
/// plugin loses it with a problem, one hosted in a program is refused.
pub const RESERVED_VERBS: &[&str] = &[
    "help",
    "engines",
    "plugins",
    "create",
    "fork",
    "respawn",
    "ls",
    "show",
    "rename",
    "tag",
    "send",
    "where",
    "attach",
    "stop",
    "interrupt",
    "gc",
    "update",
    "check",
    "definitions",
];
