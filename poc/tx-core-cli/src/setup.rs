//! What `txc` and `txi` share: the five directories, the core config, `plugins.yaml`, opening the core,
//! and how a failure maps to an exit code.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use tx_core::{CoreConfig, CoreError, Directories, PluginConfig, TxCore};

use crate::args::{self, Global};
use crate::plugins;

/// Renames all five directories at once, as `NVIM_APPNAME` does for nvim.
pub const APPNAME_ENV: &str = "TX_APPNAME";
pub const DEFAULT_APPNAME: &str = "tx-ide";
/// The version-1 home `txc migrate` moves by default: `$TX_CORE_HOME`, else `~/.tx-core`.
pub const VERSION_ONE_HOME_ENV: &str = "TX_CORE_HOME";
pub const VERSION_ONE_HOME: &str = ".tx-core";
/// Under the data dir the roles plugins ship (written by `ui install`); under the config dir
/// the user's own.
pub const ROLES_DIR: &str = "roles";
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
    /// The environment names an invalid application or leaves a directory unresolvable.
    Environment(String),
    /// `--state-dir` alone, on a directory laid out the version-1 way.
    VersionOneHome(PathBuf),
    Migration(tx_core::migrate::MigrationError),
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
            Failure::Environment(text) => f.write_str(text),
            Failure::VersionOneHome(dir) => write!(
                f,
                "{} is a version-1 home; run `txc migrate --from {}`",
                dir.display(),
                dir.display()
            ),
            Failure::Migration(error) => error.fmt(f),
        }
    }
}

/// The process environment, as the resolution reads it: a variable set to the empty string
/// counts as unset.
pub fn process_environment(key: &str) -> Option<OsString> {
    std::env::var_os(key).filter(|value| !value.is_empty())
}

/// `$TX_APPNAME` when set, else `tx-ide`; validated, since it becomes a path component.
pub fn application_name(environment: &dyn Fn(&str) -> Option<OsString>) -> Result<String, Failure> {
    let Some(name) = environment(APPNAME_ENV) else {
        return Ok(DEFAULT_APPNAME.to_owned());
    };
    name.to_str()
        .filter(|name| {
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
                && *name != "."
                && *name != ".."
        })
        .map(str::to_owned)
        .ok_or_else(|| {
            Failure::Environment(format!(
                "{APPNAME_ENV} must match [A-Za-z0-9_.-]+, got {name:?}"
            ))
        })
}

/// Each directory: its flag, else what core exported into the session (`TX_*_DIR`), else the XDG
/// variable joined with the application name, else the XDG default; made absolute so paths
/// exported into sessions do not depend on the caller's cwd.
pub fn directories(
    global: &Global,
    application: &str,
    environment: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Directories, Failure> {
    let home = || {
        environment("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let resolve = |flag: &Option<PathBuf>, exported: &str, xdg: &str, default: &[&str]| {
        flag.clone()
            .or_else(|| environment(exported).map(PathBuf::from))
            .or_else(|| environment(xdg).map(|base| PathBuf::from(base).join(application)))
            .unwrap_or_else(|| {
                default
                    .iter()
                    .fold(home(), |path, part| path.join(part))
                    .join(application)
            })
    };
    let run = match global
        .run_dir
        .clone()
        .or_else(|| environment("TX_RUN_DIR").map(PathBuf::from))
        .or_else(|| {
            environment("XDG_RUNTIME_DIR").map(|base| PathBuf::from(base).join(application))
        }) {
        Some(run) => run,
        None => {
            let user = environment("USER")
                .or_else(|| environment("LOGNAME"))
                .ok_or_else(|| {
                    Failure::Environment(
                        "cannot name the run directory: set $USER, $XDG_RUNTIME_DIR or --run-dir"
                            .to_owned(),
                    )
                })?;
            let temporary = environment("TMPDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"));
            temporary.join(format!("{application}.{}", user.to_string_lossy()))
        }
    };
    Ok(Directories {
        config: absolute(resolve(
            &global.config_dir,
            "TX_CONFIG_DIR",
            "XDG_CONFIG_HOME",
            &[".config"],
        )),
        data: absolute(resolve(
            &global.data_dir,
            "TX_DATA_DIR",
            "XDG_DATA_HOME",
            &[".local", "share"],
        )),
        state: absolute(resolve(
            &global.state_dir,
            "TX_STATE_DIR",
            "XDG_STATE_HOME",
            &[".local", "state"],
        )),
        cache: absolute(resolve(
            &global.cache_dir,
            "TX_CACHE_DIR",
            "XDG_CACHE_HOME",
            &[".cache"],
        )),
        run: absolute(run),
    })
}

/// Whether any directory was named on the command line.
fn names_a_directory(global: &Global) -> bool {
    [
        &global.config_dir,
        &global.data_dir,
        &global.state_dir,
        &global.cache_dir,
        &global.run_dir,
    ]
    .iter()
    .any(|flag| flag.is_some())
}

/// An old script's `--state-dir ~/.tx-core` meant "everything"; now it names only the state
/// directory. Caught when it is the only directory flag and points at a version-1 home.
fn refuse_a_version_one_home(global: &Global) -> Result<(), Failure> {
    let Some(state) = &global.state_dir else {
        return Ok(());
    };
    let only_flag = global.config_dir.is_none()
        && global.data_dir.is_none()
        && global.cache_dir.is_none()
        && global.run_dir.is_none();
    if only_flag && state.join(tx_core::core::SESSIONS_DIR).is_dir() {
        return Err(Failure::VersionOneHome(absolute(state.clone())));
    }
    Ok(())
}

pub fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(&path))
        .unwrap_or(path)
}

/// Without `--profile-dir`: the roles `ui install` materialises, then the user's own, which
/// override them.
fn default_profile_dirs(directories: &Directories) -> Vec<PathBuf> {
    vec![
        directories.data.join(ROLES_DIR),
        directories.config.join(ROLES_DIR),
    ]
}

/// Without `--skill-dir`: `skills/` under each default profile dir.
fn default_skill_dirs(directories: &Directories) -> Vec<PathBuf> {
    default_profile_dirs(directories)
        .into_iter()
        .map(|dir| dir.join(SKILLS_DIR))
        .collect()
}

/// The core config from the globals and the process environment; touches no disk beyond the
/// version-1 check.
pub fn core_config(global: &Global) -> Result<CoreConfig, Failure> {
    core_config_in(global, &process_environment)
}

pub fn core_config_in(
    global: &Global,
    environment: &dyn Fn(&str) -> Option<OsString>,
) -> Result<CoreConfig, Failure> {
    refuse_a_version_one_home(global)?;
    let application = application_name(environment)?;
    let directories = directories(global, &application, environment)?;
    let profile_dirs = if global.profile_dirs.is_empty() {
        default_profile_dirs(&directories)
    } else {
        global.profile_dirs.clone()
    };
    let skill_dirs = if global.skill_dirs.is_empty() {
        default_skill_dirs(&directories)
    } else {
        global.skill_dirs.clone()
    };
    let mut config = CoreConfig::new(directories)
        .profile_dirs(profile_dirs)
        .skill_dirs(skill_dirs)
        // Every session carries the application name next to the `TX_*_DIR` core exports.
        .env(APPNAME_ENV, application);
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
    // An explicit socket wins; inside a session the exported one applies when no directory was
    // named on the command line (the two were exported together).
    let socket = global.socket.clone().map(absolute).or_else(|| {
        if names_a_directory(global) {
            return None;
        }
        environment(tx_core::core::TMUX_SOCKET_ENV).map(PathBuf::from)
    });
    if let Some(socket) = socket {
        config = config.tmux_socket(socket);
    }
    Ok(config)
}

/// `--plugins`, else `<config>/plugins.yaml`; reads the file, creates nothing.
pub fn plugin_config(global: &Global, config: &CoreConfig) -> Result<PluginConfig, Failure> {
    let yaml = global
        .plugins
        .clone()
        .map(absolute)
        .unwrap_or_else(|| config.directories.config.join(plugins::FILE_NAME));
    plugins::load(&yaml, &plugins::known_ids(), plugins::RESERVED).map_err(Failure::Usage)
}

/// The plugin verbs this invocation would see, without opening the core (so `help` and an
/// unknown verb never create a directory).
pub fn preview_verbs(global: &Global) -> Result<Vec<Arc<dyn tx_core::Verb>>, Failure> {
    let config = core_config(global)?;
    let cfg = plugin_config(global, &config)?;
    tx_core::plugin::preview_verbs(&plugins::manifest(), &cfg, &config).map_err(Failure::Plugin)
}

pub fn open(global: &Global) -> Result<TxCore, Failure> {
    open_with(global, core_config(global)?)
}

/// Opens the core from an already resolved config. Process records left on disk by a build
/// before PR 11 are skipped by every read; this names them on stderr, once per invocation,
/// until Storage PR 14's migration moves them.
pub fn open_with(global: &Global, config: CoreConfig) -> Result<TxCore, Failure> {
    let cfg = plugin_config(global, &config)?;
    let mut core = TxCore::open(config)?;
    core.plug(plugins::manifest(), &cfg)
        .map_err(Failure::Plugin)?;
    let skipped = core.skipped_records()?;
    if !skipped.is_empty() {
        let ids: Vec<String> = skipped.iter().map(ToString::to_string).collect();
        eprintln!(
            "txc: skipped {} process record(s) written before core held agents only: {}",
            ids.len(),
            ids.join(", ")
        );
    }
    Ok(core)
}
