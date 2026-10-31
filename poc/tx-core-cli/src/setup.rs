//! What `txc` and `txi` share: the state dir, the core config, `plugins.yaml`, opening the core,
//! and how a failure maps to an exit code.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tx_core::{CoreConfig, CoreError, PluginConfig, TxCore};

use crate::args::{self, Global};
use crate::plugins;

pub const HOME_ENV: &str = "TX_CORE_HOME";
pub const DEFAULT_HOME: &str = ".tx-core";
/// Under the state dir: the roles plugins ship (written by `ui install`) and the user's own.
pub const AGENTS_DIR: &str = "agents";
pub const USER_AGENTS_DIR: &str = "user-agents";
pub const SKILLS_DIR: &str = "skills";
/// The internal binary the wired hooks run; installed next to `txc`.
pub const TXI: &str = "txi";

/// Why `run` stopped: usage is exit 2 with the usage text, everything else exit 1.
#[derive(Debug)]
pub enum Failure {
    Usage(args::UsageError),
    Core(CoreError),
    Plugin(tx_core::PluginError),
    Verb(tx_core::VerbError),
}

impl From<CoreError> for Failure {
    fn from(error: CoreError) -> Self {
        Failure::Core(error)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Usage(error) => error.fmt(f),
            Failure::Core(error) => error.fmt(f),
            Failure::Plugin(error) => error.fmt(f),
            Failure::Verb(error) => error.fmt(f),
        }
    }
}

/// `--state-dir`, else `$TX_CORE_HOME` (which every session carries), else `~/.tx-core`; made
/// absolute so the socket path exported into sessions does not depend on the caller's cwd.
pub fn state_dir(explicit: Option<PathBuf>) -> PathBuf {
    let dir = explicit
        .or_else(|| {
            std::env::var_os(HOME_ENV)
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(DEFAULT_HOME)
        });
    absolute(dir)
}

pub fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(&path))
        .unwrap_or(path)
}

/// Without `--profile-dir`: the roles `ui install` materialises, then the user's own.
fn default_profile_dirs(state: &Path) -> Vec<PathBuf> {
    vec![state.join(AGENTS_DIR), state.join(USER_AGENTS_DIR)]
}

/// Without `--skill-dir`: `skills/` under each default profile dir.
fn default_skill_dirs(state: &Path) -> Vec<PathBuf> {
    default_profile_dirs(state)
        .into_iter()
        .map(|dir| dir.join(SKILLS_DIR))
        .collect()
}

/// The core config from the globals; pure, touches no disk.
pub fn core_config(global: &Global) -> CoreConfig {
    let state = state_dir(global.state_dir.clone());
    let profile_dirs = if global.profile_dirs.is_empty() {
        default_profile_dirs(&state)
    } else {
        global.profile_dirs.clone()
    };
    let skill_dirs = if global.skill_dirs.is_empty() {
        default_skill_dirs(&state)
    } else {
        global.skill_dirs.clone()
    };
    let mut config = CoreConfig::new(state)
        .profile_dirs(profile_dirs)
        .skill_dirs(skill_dirs);
    // The hooks a session's engine wires run `txi hook <Event>`, the `txi` installed next to
    // this binary. Without a path to it the session still launches; it just stays `starting`.
    if let Ok(exe) = std::env::current_exe() {
        config = config.hook_command([
            exe.with_file_name(TXI).to_string_lossy().into_owned(),
            "hook".into(),
        ]);
    }
    for (key, value) in &global.env {
        config = config.env(key, value);
    }
    // An explicit socket wins; inside a session the exported one applies when no state dir was
    // named on the command line (the two were exported together).
    let socket = global.socket.clone().map(absolute).or_else(|| {
        if global.state_dir.is_some() {
            return None;
        }
        std::env::var_os(tx_core::core::TMUX_SOCKET_ENV)
            .filter(|socket| !socket.is_empty())
            .map(PathBuf::from)
    });
    if let Some(socket) = socket {
        config = config.tmux_socket(socket);
    }
    config
}

/// `--plugins`, else `<state_dir>/plugins.yaml`; reads the file, creates nothing.
pub fn plugin_config(global: &Global, config: &CoreConfig) -> Result<PluginConfig, Failure> {
    let yaml = global
        .plugins
        .clone()
        .map(absolute)
        .unwrap_or_else(|| config.state_dir.join(plugins::FILE_NAME));
    plugins::load(&yaml, &plugins::known_ids(), plugins::RESERVED).map_err(Failure::Usage)
}

/// The plugin verbs this invocation would see, without opening the core (so `help` and an
/// unknown verb never create the state dir).
pub fn preview_verbs(global: &Global) -> Result<Vec<Arc<dyn tx_core::Verb>>, Failure> {
    let config = core_config(global);
    let cfg = plugin_config(global, &config)?;
    tx_core::plugin::preview_verbs(&plugins::manifest(), &cfg, &config).map_err(Failure::Plugin)
}

pub fn open(global: &Global) -> Result<TxCore, Failure> {
    let config = core_config(global);
    let cfg = plugin_config(global, &config)?;
    let mut core = TxCore::open(config)?;
    core.plug(plugins::manifest(), &cfg)
        .map_err(Failure::Plugin)?;
    Ok(core)
}
