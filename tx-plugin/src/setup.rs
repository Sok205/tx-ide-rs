//! What every program that runs as tx shares: the five directories, the core config,
//! `config.yaml`, opening the core, and how a failure maps to an exit code.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::Arc;

use tx_core::plugin::external::PLUGIN_GIVEN_ENV;
use tx_core::plugin::external::TRACE_HOOK;
use tx_core::plugin::{Activation, Entry, ExecContext, Index, Launcher};
use tx_core::{CoreConfig, CoreError, Directories, Plugin, PluginConfig, Registry, TxCore};

use crate::args::{self, Global};
use crate::config::Config;
use crate::sdk::{self, TX_BIN_ENV};

/// Renames all five directories at once, as `NVIM_APPNAME` does for nvim.
pub const APPNAME_ENV: &str = "TX_APPNAME";
pub const DEFAULT_APPNAME: &str = "tx-ide";
/// Under the data dir the roles plugins ship (written by `tx update`); under the config dir
/// the user's own.
pub const ROLES_DIR: &str = "roles";
/// Under each harness root: the skills, the shipped ones and then the user's.
pub const SKILLS_DIR: &str = "skills";
/// The internal binary the wired hooks run; installed next to `tx`.
pub const TXI: &str = "txi";
/// The binary people type; `txi` finds it next to itself.
pub const TX: &str = "tx";
/// What stands for `--plugin-dir`, `--profile-dir` and `--skill-dir` when the flag is absent:
/// the directories, colon-separated.
pub const PLUGIN_PATH_ENV: &str = "TX_PLUGIN_PATH";
pub const PROFILE_PATH_ENV: &str = "TX_PROFILE_PATH";
pub const SKILL_PATH_ENV: &str = "TX_SKILL_PATH";
/// What stands for `--config FILE` when the flag is absent.
pub const CONFIG_FILE_ENV: &str = "TX_CONFIG_FILE";
/// What stands for `--env KEY=VALUE` when the flag is absent: a JSON object of strings.
pub const SESSION_ENV_ENV: &str = "TX_SESSION_ENV";
/// The file that gets one line for every plugin program started; unset: no trace.
pub const PLUGIN_TRACE_ENV: &str = "TX_PLUGIN_TRACE";

/// Why `run` stopped: usage is exit 2 with the usage text, everything else exit 1.
#[derive(Debug)]
pub enum Failure {
    Usage(args::UsageError),
    Core(CoreError),
    Plugin(tx_core::PluginError),
    Verb(tx_core::VerbError),
    /// The environment names an invalid application or leaves a directory unresolvable.
    Environment(String),
    /// A definition file that cannot be read where a listing needs it.
    Harness(tx_core::harness::HarnessError),
    /// An update step, or the stamp it keeps, failed.
    Update(tx_core::updates::UpdateError),
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
            Failure::Harness(error) => error.fmt(f),
            Failure::Update(error) => error.fmt(f),
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

pub fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(&path))
        .unwrap_or(path)
}

/// Without `--profile-dir`: the roles `tx update` materialises, then the user's own, which
/// override them.
fn default_profile_dirs(directories: &Directories) -> Vec<PathBuf> {
    vec![
        directories.data.join(ROLES_DIR),
        directories.config.join(ROLES_DIR),
    ]
}

/// Without `--skill-dir`: `skills/` under each harness root, the shipped ones and then the
/// user's.
fn default_skill_dirs(directories: &Directories) -> Vec<PathBuf> {
    directories
        .harness_roots()
        .into_iter()
        .map(|dir| dir.join(SKILLS_DIR))
        .collect()
}

/// The directories a repeatable flag named, else the ones its variable lists (colon-separated),
/// else `None`: the defaults apply. A variable that lists none is like one that is not set, as
/// an empty one is.
fn named_dirs(
    flags: &[PathBuf],
    variable: &str,
    environment: &dyn Fn(&str) -> Option<OsString>,
) -> Option<Vec<PathBuf>> {
    if !flags.is_empty() {
        return Some(flags.to_vec());
    }
    let listed: Vec<PathBuf> = std::env::split_paths(&environment(variable)?)
        .filter(|path| !path.as_os_str().is_empty())
        .collect();
    (!listed.is_empty()).then_some(listed)
}

/// Directories as one colon-separated value.
fn path_list(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|dir| dir.to_string_lossy())
        .collect::<Vec<_>>()
        .join(":")
}

/// `$TX_SESSION_ENV`: a JSON object of strings, in the order written.
fn session_environment(text: &OsStr) -> Result<Vec<(String, String)>, Failure> {
    let refused = || {
        Failure::Environment(format!(
            "{SESSION_ENV_ENV} must be a JSON object of strings, got {text:?}"
        ))
    };
    let object: serde_json::Map<String, serde_json::Value> = text
        .to_str()
        .and_then(|text| serde_json::from_str(text).ok())
        .ok_or_else(refused)?;
    object
        .into_iter()
        .map(|(key, value)| match value {
            serde_json::Value::String(value) => Ok((key, value)),
            _ => Err(refused()),
        })
        .collect()
}

/// The core config from the globals and the process environment; touches no disk.
pub fn core_config(global: &Global) -> Result<CoreConfig, Failure> {
    Ok(core_config_in(global, &process_environment)?.caller_env(std::env::vars()))
}

/// Every global flag has a variable that stands for it when the flag is absent (flag, then
/// variable, then default), so that a child `tx` or a plugin program opens the core its parent
/// opened.
pub fn core_config_in(
    global: &Global,
    environment: &dyn Fn(&str) -> Option<OsString>,
) -> Result<CoreConfig, Failure> {
    let application = application_name(environment)?;
    let directories = directories(global, &application, environment)?;
    let profile_dirs = named_dirs(&global.profile_dirs, PROFILE_PATH_ENV, environment)
        .unwrap_or_else(|| default_profile_dirs(&directories));
    let skill_dirs = named_dirs(&global.skill_dirs, SKILL_PATH_ENV, environment)
        .unwrap_or_else(|| default_skill_dirs(&directories));
    let mut config = CoreConfig::new(directories)
        .profile_dirs(profile_dirs)
        .skill_dirs(skill_dirs)
        // Every session carries the application name next to the `TX_*_DIR` core exports.
        .env(APPNAME_ENV, application);
    if let Some(tx) = sdk::try_tx_bin_in(environment) {
        // The hooks a session's engine wires run `txi hook <Event>`, the `txi` installed next to
        // `tx`. Without a path to it the session still launches; it just stays `starting`. Not
        // `current_exe()`: in a plugin program that is the plugin.
        config = config
            .hook_command([
                tx.with_file_name(TXI).to_string_lossy().into_owned(),
                "hook".into(),
            ])
            // A plugin's command line starts with `tx`: its sessions are told where it is.
            .env(TX_BIN_ENV, tx.to_string_lossy());
    }
    if let Some(roots) = named_dirs(&global.plugin_dirs, PLUGIN_PATH_ENV, environment) {
        // Absolute: they reach tmux and sessions, which run elsewhere.
        let roots: Vec<PathBuf> = roots.into_iter().map(absolute).collect();
        config = config
            .env(PLUGIN_PATH_ENV, path_list(&roots))
            .plugin_dirs(roots);
    }
    if let Some(trace) = environment(PLUGIN_TRACE_ENV) {
        // The sessions a traced command launches are traced too.
        config = config.env(PLUGIN_TRACE_ENV, trace.to_string_lossy());
    }
    let session = match environment(SESSION_ENV_ENV) {
        Some(text) if global.env.is_empty() => session_environment(&text)?,
        _ => global.env.clone(),
    };
    for (key, value) in session {
        config = config.env(key, value);
    }
    // An explicit socket wins; inside a session the exported one applies when no directory was
    // named on the command line (the two were exported together). The run directory's own
    // socket is no override: a program that is told the socket through its environment then
    // holds the very config of the `tx` that started it, down to the flags it hands on.
    let socket = global.socket.clone().map(absolute).or_else(|| {
        if names_a_directory(global) {
            return None;
        }
        environment(tx_core::core::TMUX_SOCKET_ENV).map(PathBuf::from)
    });
    if let Some(socket) = socket.filter(|socket| *socket != config.socket_path()) {
        config = config.tmux_socket(socket);
    }
    // What was added to this process's environment on the way to it: no tmux call of the core
    // inherits it, so a server this process starts holds what the caller of `tx` held.
    Ok(config.given(given(environment)))
}

/// The names `$TX_PLUGIN_GIVEN` lists: the variables that were added to this process's
/// environment on the way to it. None in a process `tx` did not start as a plugin program,
/// directly or further up.
fn given(environment: &dyn Fn(&str) -> Option<OsString>) -> Vec<String> {
    environment(PLUGIN_GIVEN_ENV)
        .map(|names| {
            names
                .to_string_lossy()
                .split(':')
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// `--config FILE`, else `$TX_CONFIG_FILE`, else `<config>/config.yaml`.
pub fn config_file(global: &Global, config: &CoreConfig) -> PathBuf {
    config_file_in(global, config, &process_environment)
}

fn config_file_in(
    global: &Global,
    config: &CoreConfig,
    environment: &dyn Fn(&str) -> Option<OsString>,
) -> PathBuf {
    global
        .config
        .clone()
        .or_else(|| environment(CONFIG_FILE_ENV).map(PathBuf::from))
        .map(absolute)
        .unwrap_or_else(|| config.directories.config.join(crate::config::FILE_NAME))
}

/// What a plugin program is started with, for a `config` that [`core_config`] resolved from
/// `global`: the `tx` to run, the application name, the five directories and the socket, always;
/// and what stands for a global flag only where the flag (or its variable, which the program
/// inherits as it is) replaced a default. With these a plugin program resolves the same core.
/// Last, under `TX_PLUGIN_GIVEN`, which of them this process's own environment does not hold,
/// after the ones that were already added on the way to this process.
pub fn exec_context(global: &Global, config: &CoreConfig) -> ExecContext {
    exec_context_in(global, config, &process_environment)
}

/// [`exec_context`] for a process whose environment is `held`.
fn exec_context_in(
    global: &Global,
    config: &CoreConfig,
    held: &dyn Fn(&str) -> Option<OsString>,
) -> ExecContext {
    // What `core_config` put into every session's environment, before any `--env` of the
    // caller's.
    let exported = |key: &str| {
        config
            .env
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    let mut environment: Vec<(String, String)> = [TX_BIN_ENV, APPNAME_ENV]
        .into_iter()
        .filter_map(|key| Some((key.to_owned(), exported(key)?)))
        .collect();
    let mut set = |key: &str, value: String| environment.push((key.to_owned(), value));
    for (key, directory) in config.directories.environment() {
        set(key, directory.to_string_lossy().into_owned());
    }
    set(
        tx_core::core::TMUX_SOCKET_ENV,
        config.socket_path().to_string_lossy().into_owned(),
    );
    if let Some(roots) = &config.plugin_dirs {
        set(PLUGIN_PATH_ENV, path_list(roots));
    }
    for (variable, dirs) in [
        (PROFILE_PATH_ENV, &global.profile_dirs),
        (SKILL_PATH_ENV, &global.skill_dirs),
    ] {
        if !dirs.is_empty() {
            let dirs: Vec<PathBuf> = dirs.iter().cloned().map(absolute).collect();
            set(variable, path_list(&dirs));
        }
    }
    if let Some(file) = &global.config {
        set(
            CONFIG_FILE_ENV,
            absolute(file.clone()).to_string_lossy().into_owned(),
        );
    }
    if !global.env.is_empty() {
        let object: serde_json::Map<String, serde_json::Value> = global
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone().into()))
            .collect();
        set(
            SESSION_ENV_ENV,
            serde_json::Value::Object(object).to_string(),
        );
    }
    // A variable the caller already held is handed on resolved and is not an addition; one
    // that was added further up stays on the list, whoever holds it by now.
    let mut given = config.given.clone();
    for (key, _) in &environment {
        if held(key).is_none() && !given.contains(key) {
            given.push(key.clone());
        }
    }
    if !given.is_empty() {
        environment.push((PLUGIN_GIVEN_ENV.to_owned(), given.join(":")));
    }
    ExecContext {
        environment,
        trace: exported(PLUGIN_TRACE_ENV).map(PathBuf::from),
    }
}

/// The config file; reads it, creates nothing.
pub fn load_config(global: &Global, config: &CoreConfig) -> Result<Config, Failure> {
    crate::config::load(&config_file(global, config)).map_err(Failure::Usage)
}

/// What this installation holds, resolved once per invocation: the core config, `config.yaml`,
/// the plugins installed in the plugin roots, and which of them are active.
pub struct Installation {
    pub config: CoreConfig,
    pub file: Config,
    /// The `plugins:` entries of `file`, as the activation read them.
    pub entries: BTreeMap<String, Entry>,
    pub index: Index,
    pub activation: Activation,
}

impl Installation {
    /// Resolves and reads everything, and writes nothing: a command that turns out to be misuse
    /// creates no file. [`Self::cache`] keeps the index for the next run.
    pub fn read(global: &Global) -> Result<Self, Failure> {
        Self::read_with(global, core_config(global)?)
    }

    /// [`Self::read`] from an already resolved config.
    pub fn read_with(global: &Global, config: CoreConfig) -> Result<Self, Failure> {
        let file = load_config(global, &config)?;
        let entries = crate::config::entries(&file.plugins);
        let index = Index::read(&config, crate::VERSION);
        let activation = Activation::new(&index, &entries, crate::RESERVED_VERBS);
        Ok(Self {
            config,
            file,
            entries,
            index,
            activation,
        })
    }

    /// Keeps the index in `<cache>/plugins-index.json`, once the command is known to run.
    pub fn cache(&self) {
        self.index.cache(&self.config);
    }

    /// How the programs of the active plugin `id` are started.
    pub fn launcher(&self, global: &Global, id: &str) -> Launcher {
        let (state, installed, _) = self
            .activation
            .active(&self.index)
            .find(|(state, _, _)| state.id == id)
            .expect("an active plugin");
        Launcher::new(
            &Arc::new(exec_context(global, &self.config)),
            &self.config,
            state,
            installed,
        )
    }

    /// Opens the core a plugin program runs on: each of `hosted` that is active in this
    /// installation is registered in-process, with the settings `config.yaml` gives its id;
    /// every other active plugin is behind proxies. So the core behaves in the program as it
    /// does in `tx`.
    pub fn open_hosted(
        self,
        global: &Global,
        hosted: Vec<Box<dyn Plugin>>,
    ) -> Result<TxCore, Failure> {
        let active: Vec<Box<dyn Plugin>> = hosted
            .into_iter()
            .filter(|plugin| self.activation.is_active(plugin.id()))
            .collect();
        let cfg = self.hosted_config(&active);
        self.open_plugged(global, active, &cfg)
    }

    /// [`Self::open_hosted`] for a retire: every one of `hosted` is registered, enabled or not,
    /// installed or not, since a plugin takes back what it generated after it stopped being
    /// active.
    pub fn open_retiring(
        self,
        global: &Global,
        hosted: Vec<Box<dyn Plugin>>,
    ) -> Result<TxCore, Failure> {
        let cfg = self.hosted_config(&hosted);
        self.open_plugged(global, hosted, &cfg)
    }

    /// One [`Registry`] per plugin of `hosted` that is active in this installation, in hosting
    /// order: `register` run with the settings `config.yaml` gives its id, as
    /// [`Self::open_hosted`] has it run. No core is opened: this is how a plugin program gets at
    /// its engine to serve it, or at its sinks to hand them an event.
    pub fn registries(&self, hosted: &[Box<dyn Plugin>]) -> Result<Vec<Registry>, Failure> {
        hosted
            .iter()
            .filter(|plugin| self.activation.is_active(plugin.id()))
            .map(|plugin| {
                let settings = self
                    .entries
                    .get(plugin.id())
                    .map_or(serde_json::Value::Null, |entry| entry.settings.clone());
                let mut registry = Registry::new(settings, self.config.clone());
                plugin.register(&mut registry).map_err(Failure::Plugin)?;
                Ok(registry)
            })
            .collect()
    }

    /// The first of [`Self::registries`] that holds an engine. `None`: none of `hosted`
    /// registers one.
    pub fn engine_registry(&self, hosted: &[Box<dyn Plugin>]) -> Result<Option<Registry>, Failure> {
        Ok(self
            .registries(hosted)?
            .into_iter()
            .find(|registry| !registry.engines().is_empty()))
    }

    /// Notes in the trace that this program runs as the hook entry of an engine. Nothing of tx
    /// starts a hook (the engine's CLI does), so the entry writes its own line. The plugin it
    /// names is the one of `hosted` that the activation gave an engine kind.
    pub fn trace_hook(&self, global: &Global, hosted: &[Box<dyn Plugin>]) {
        let engine = hosted.iter().map(|plugin| plugin.id()).find(|id| {
            self.activation
                .engines
                .values()
                .any(|engine| engine.plugin == *id)
        });
        if let Some(plugin) = engine
            && let Ok(program) = std::env::current_exe()
        {
            exec_context(global, &self.config).trace(TRACE_HOOK, plugin, &program);
        }
    }

    /// The settings `config.yaml` gives each of `plugins`; none of them is switched off here.
    fn hosted_config(&self, plugins: &[Box<dyn Plugin>]) -> PluginConfig {
        PluginConfig {
            settings: plugins
                .iter()
                .filter_map(|plugin| {
                    let settings = &self.entries.get(plugin.id())?.settings;
                    (!settings.is_null()).then(|| (plugin.id().to_owned(), settings.clone()))
                })
                .collect(),
            reserved: crate::RESERVED_VERBS
                .iter()
                .map(|verb| (*verb).to_owned())
                .collect(),
            ..PluginConfig::default()
        }
    }

    /// Opens the core with `plugins` registered in-process and every other active installed
    /// plugin behind proxies. Records every read skips (those whose engine is not the name of
    /// an engine kind) are named on stderr here, once per invocation.
    fn open_plugged(
        self,
        global: &Global,
        plugins: Vec<Box<dyn Plugin>>,
        cfg: &PluginConfig,
    ) -> Result<TxCore, Failure> {
        let context = exec_context(global, &self.config);
        let hosted: Vec<&'static str> = plugins.iter().map(|plugin| plugin.id()).collect();
        let mut core = TxCore::open(self.config.agent_defaults(self.file.agent))?;
        // Only now: a core that refuses to open has created nothing, the index included.
        self.index.cache(core.config());
        core.plug(plugins, cfg).map_err(Failure::Plugin)?;
        core.plug_external(&self.index, &self.activation, context, &hosted)
            .map_err(Failure::Plugin)?;
        for (id, reason) in core.skipped_records()? {
            eprintln!("tx: skipped record {id}: {reason}");
        }
        Ok(core)
    }
}

/// The core a plugin program opens: see [`Installation::open_hosted`].
pub fn open_hosted(global: &Global, hosted: Vec<Box<dyn Plugin>>) -> Result<TxCore, Failure> {
    Installation::read(global)?.open_hosted(global, hosted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// An environment of the given variables, with a home and a user so that every directory
    /// resolves.
    fn environment(variables: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let mut all = vec![
            ("HOME".to_owned(), "/home/u".to_owned()),
            ("USER".to_owned(), "u".to_owned()),
            ("TMPDIR".to_owned(), "/tmp".to_owned()),
        ];
        all.extend(
            variables
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        );
        move |key: &str| {
            all.iter()
                .rev()
                .find(|(name, _)| name == key)
                .map(|(_, value)| OsString::from(value))
                .filter(|value| !value.is_empty())
        }
    }

    fn config(global: &Global, variables: &[(&str, &str)]) -> CoreConfig {
        core_config_in(global, &environment(variables)).unwrap()
    }

    fn exported<'a>(config: &'a CoreConfig, key: &str) -> Option<&'a str> {
        config
            .env
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    fn paths(list: &[&str]) -> Vec<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn without_flags_or_variables_every_default_applies() {
        let config = config(&Global::default(), &[]);
        assert_eq!(config.plugin_dirs, None);
        assert_eq!(
            config.plugin_roots(),
            paths(&[
                "/home/u/.local/share/tx-ide/shipped",
                "/home/u/.config/tx-ide/plugins"
            ])
        );
        assert_eq!(
            config.profile_dirs,
            paths(&[
                "/home/u/.local/share/tx-ide/roles",
                "/home/u/.config/tx-ide/roles"
            ])
        );
        assert_eq!(config.env, [(APPNAME_ENV.to_owned(), "tx-ide".to_owned())]);
        assert!(config.hook_command.is_empty(), "no tx is known");
        assert_eq!(
            config_file_in(&Global::default(), &config, &environment(&[])),
            Path::new("/home/u/.config/tx-ide/config.yaml")
        );
    }

    #[test]
    fn a_variable_stands_for_its_flag_when_the_flag_is_absent() {
        let variables = [
            (PLUGIN_PATH_ENV, "/roots/shipped:/roots/mine"),
            (PROFILE_PATH_ENV, "/roles/a:/roles/b"),
            (SKILL_PATH_ENV, "/skills"),
            (CONFIG_FILE_ENV, "/elsewhere/config.yaml"),
            (SESSION_ENV_ENV, r#"{"B": "2", "A": "x=y"}"#),
        ];
        let global = Global::default();
        let config = config(&global, &variables);
        assert_eq!(
            config.plugin_dirs,
            Some(paths(&["/roots/shipped", "/roots/mine"]))
        );
        assert_eq!(config.profile_dirs, paths(&["/roles/a", "/roles/b"]));
        assert_eq!(config.skill_dirs, paths(&["/skills"]));
        assert_eq!(
            config_file_in(&global, &config, &environment(&variables)),
            Path::new("/elsewhere/config.yaml")
        );
        assert_eq!(
            config.env,
            [
                (APPNAME_ENV.to_owned(), "tx-ide".to_owned()),
                (
                    PLUGIN_PATH_ENV.to_owned(),
                    "/roots/shipped:/roots/mine".to_owned()
                ),
                ("B".to_owned(), "2".to_owned()),
                ("A".to_owned(), "x=y".to_owned()),
            ],
            "the session variables in the order written"
        );
    }

    #[test]
    fn a_flag_beats_its_variable() {
        let variables = [
            (PLUGIN_PATH_ENV, "/variable/roots"),
            (PROFILE_PATH_ENV, "/variable/roles"),
            (SKILL_PATH_ENV, "/variable/skills"),
            (CONFIG_FILE_ENV, "/variable/config.yaml"),
            (SESSION_ENV_ENV, r#"{"FROM": "variable"}"#),
        ];
        let global = Global {
            plugin_dirs: paths(&["/flag/roots"]),
            profile_dirs: paths(&["/flag/roles"]),
            skill_dirs: paths(&["/flag/skills"]),
            config: Some("/flag/config.yaml".into()),
            env: vec![("FROM".to_owned(), "flag".to_owned())],
            ..Global::default()
        };
        let config = config(&global, &variables);
        assert_eq!(config.plugin_dirs, Some(paths(&["/flag/roots"])));
        assert_eq!(config.profile_dirs, paths(&["/flag/roles"]));
        assert_eq!(config.skill_dirs, paths(&["/flag/skills"]));
        assert_eq!(
            config_file_in(&global, &config, &environment(&variables)),
            Path::new("/flag/config.yaml")
        );
        assert_eq!(exported(&config, "FROM"), Some("flag"));
        assert_eq!(exported(&config, PLUGIN_PATH_ENV), Some("/flag/roots"));
    }

    #[test]
    fn plugin_roots_are_made_absolute_and_empty_segments_dropped() {
        let config = config(
            &Global::default(),
            &[(PLUGIN_PATH_ENV, ":relative/root::/abs:")],
        );
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(
            config.plugin_dirs,
            Some(vec![cwd.join("relative/root"), PathBuf::from("/abs")])
        );
        // A list of nothing names nothing: the defaults apply, as for an empty variable.
        for nothing in ["", ":", "::"] {
            let config = self::config(&Global::default(), &[(PLUGIN_PATH_ENV, nothing)]);
            assert_eq!(config.plugin_dirs, None, "{nothing:?}");
            assert_eq!(exported(&config, PLUGIN_PATH_ENV), None, "{nothing:?}");
        }
    }

    #[test]
    fn a_session_environment_that_is_not_an_object_of_strings_is_refused() {
        for text in ["[1]", "{\"A\": 1}", "A=1", "{"] {
            let error =
                core_config_in(&Global::default(), &environment(&[(SESSION_ENV_ENV, text)]))
                    .unwrap_err();
            assert!(matches!(&error, Failure::Environment(_)), "{error}");
            assert!(
                error
                    .to_string()
                    .starts_with("TX_SESSION_ENV must be a JSON object of strings"),
                "{text}: {error}"
            );
        }
    }

    #[test]
    fn a_known_tx_names_the_hook_command_and_reaches_every_session() {
        let config = config(
            &Global::default(),
            &[
                (TX_BIN_ENV, "/opt/tx/bin/tx"),
                (PLUGIN_TRACE_ENV, "/t/trace"),
            ],
        );
        assert_eq!(config.hook_command, ["/opt/tx/bin/txi", "hook"]);
        assert_eq!(exported(&config, TX_BIN_ENV), Some("/opt/tx/bin/tx"));
        assert_eq!(exported(&config, PLUGIN_TRACE_ENV), Some("/t/trace"));
    }

    #[test]
    fn a_child_gets_the_resolved_core_and_only_the_flags_that_replace_a_default() {
        let variables = [(TX_BIN_ENV, "/opt/tx/bin/tx"), (APPNAME_ENV, "tx-dev")];
        let global = Global::default();
        let context = exec_context_in(
            &global,
            &config(&global, &variables),
            &environment(&variables),
        );
        assert_eq!(
            context.environment,
            [
                (TX_BIN_ENV, "/opt/tx/bin/tx"),
                (APPNAME_ENV, "tx-dev"),
                ("TX_CONFIG_DIR", "/home/u/.config/tx-dev"),
                ("TX_DATA_DIR", "/home/u/.local/share/tx-dev"),
                ("TX_STATE_DIR", "/home/u/.local/state/tx-dev"),
                ("TX_CACHE_DIR", "/home/u/.cache/tx-dev"),
                ("TX_RUN_DIR", "/tmp/tx-dev.u"),
                ("TX_TMUX_SOCKET", "/tmp/tx-dev.u/tmux.sock"),
                // The two the caller held are handed on, and are not additions.
                (
                    PLUGIN_GIVEN_ENV,
                    "TX_CONFIG_DIR:TX_DATA_DIR:TX_STATE_DIR:TX_CACHE_DIR:TX_RUN_DIR:TX_TMUX_SOCKET"
                ),
            ]
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
        );
        assert_eq!(context.trace, None);
    }

    /// The list a program is given: what its starter's caller did not hold, after what was
    /// already added on the way to the starter.
    fn given_by(global: &Global, variables: &[(&str, &str)]) -> Option<String> {
        exec_context_in(global, &config(global, variables), &environment(variables))
            .environment
            .into_iter()
            .find(|(key, _)| key == PLUGIN_GIVEN_ENV)
            .map(|(_, names)| names)
    }

    #[test]
    fn a_child_is_told_which_variables_its_caller_did_not_hold() {
        const DIRECTORIES: [(&str, &str); 5] = [
            ("TX_CONFIG_DIR", "/c"),
            ("TX_DATA_DIR", "/d"),
            ("TX_STATE_DIR", "/s"),
            ("TX_CACHE_DIR", "/k"),
            ("TX_RUN_DIR", "/r"),
        ];
        // A plain shell holds none of them.
        assert_eq!(
            given_by(&Global::default(), &[]).as_deref(),
            Some(
                "TX_APPNAME:TX_CONFIG_DIR:TX_DATA_DIR:TX_STATE_DIR:TX_CACHE_DIR:TX_RUN_DIR:TX_TMUX_SOCKET"
            )
        );
        // A caller that exports the core's coordinates keeps them: a flag of this one command
        // is an addition, whatever the caller held under another name.
        let mut sandbox = vec![(APPNAME_ENV, "tx-qa")];
        sandbox.extend(DIRECTORIES);
        let flagged = Global {
            profile_dirs: paths(&["/roles"]),
            ..Global::default()
        };
        assert_eq!(
            given_by(&flagged, &sandbox).as_deref(),
            Some("TX_TMUX_SOCKET:TX_PROFILE_PATH")
        );
        // A variable the caller held is not listed, whatever its value: the flag only changes
        // what is handed on.
        sandbox.push((PROFILE_PATH_ENV, "/other/roles"));
        assert_eq!(
            given_by(&flagged, &sandbox).as_deref(),
            Some("TX_TMUX_SOCKET")
        );
        sandbox.push(("TX_TMUX_SOCKET", "/r/tmux.sock"));
        assert_eq!(given_by(&flagged, &sandbox), None, "nothing was added");
    }

    #[test]
    fn what_was_added_further_up_stays_on_the_list() {
        // A plugin program: it holds everything by now, and its own list says what was added.
        let program = [
            (APPNAME_ENV, "tx-ide"),
            ("TX_DATA_DIR", "/d"),
            ("TX_PLUGIN_ID", "ide"),
            (PLUGIN_GIVEN_ENV, "TX_DATA_DIR::TX_PLUGIN_ID:"),
        ];
        let config = config(&Global::default(), &program);
        assert_eq!(config.given, ["TX_DATA_DIR", "TX_PLUGIN_ID"]);
        assert_eq!(
            given_by(&Global::default(), &program).as_deref(),
            Some(
                "TX_DATA_DIR:TX_PLUGIN_ID:TX_CONFIG_DIR:TX_STATE_DIR:TX_CACHE_DIR:TX_RUN_DIR:TX_TMUX_SOCKET"
            )
        );
        // Without the variable nothing was added to this process.
        assert!(self::config(&Global::default(), &[]).given.is_empty());
    }

    #[test]
    fn a_child_gets_a_variable_for_every_flag_given() {
        let variables = [(PLUGIN_TRACE_ENV, "/t/trace")];
        let global = Global {
            data_dir: Some("/d".into()),
            socket: Some("/s.sock".into()),
            plugin_dirs: paths(&["/roots/a", "/roots/b"]),
            profile_dirs: paths(&["/roles"]),
            skill_dirs: paths(&["/skills/a", "/skills/b"]),
            config: Some("/c/other.yaml".into()),
            env: vec![
                ("A".to_owned(), "1".to_owned()),
                ("B".to_owned(), "x=y".to_owned()),
            ],
            ..Global::default()
        };
        let context = exec_context_in(
            &global,
            &config(&global, &variables),
            &environment(&variables),
        );
        let get = |key: &str| {
            context
                .environment
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get(TX_BIN_ENV), None, "no tx is known here");
        assert_eq!(get("TX_DATA_DIR"), Some("/d"));
        assert_eq!(get("TX_TMUX_SOCKET"), Some("/s.sock"));
        assert_eq!(get(PLUGIN_PATH_ENV), Some("/roots/a:/roots/b"));
        assert_eq!(get(PROFILE_PATH_ENV), Some("/roles"));
        assert_eq!(get(SKILL_PATH_ENV), Some("/skills/a:/skills/b"));
        assert_eq!(get(CONFIG_FILE_ENV), Some("/c/other.yaml"));
        assert_eq!(get(SESSION_ENV_ENV), Some(r#"{"A":"1","B":"x=y"}"#));
        assert_eq!(context.trace, Some(PathBuf::from("/t/trace")));
        // What the child reads back is what the parent was given.
        let child = core_config_in(
            &Global::default(),
            &environment(
                &context
                    .environment
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect::<Vec<_>>(),
            ),
        )
        .unwrap();
        let parent = config(&global, &variables);
        assert_eq!(child.directories, parent.directories);
        assert_eq!(child.socket_path(), parent.socket_path());
        assert_eq!(child.cli_arguments(), parent.cli_arguments());
        assert_eq!(child.plugin_dirs, parent.plugin_dirs);
        assert_eq!(child.profile_dirs, parent.profile_dirs);
        assert_eq!(child.skill_dirs, parent.skill_dirs);
        assert_eq!(exported(&child, "A"), Some("1"));
        assert_eq!(exported(&child, "B"), Some("x=y"));
        // And it knows that all of it was added: its caller held none of these.
        assert!(parent.given.is_empty());
        assert_eq!(
            child.given,
            [
                APPNAME_ENV,
                "TX_CONFIG_DIR",
                "TX_DATA_DIR",
                "TX_STATE_DIR",
                "TX_CACHE_DIR",
                "TX_RUN_DIR",
                "TX_TMUX_SOCKET",
                PLUGIN_PATH_ENV,
                PROFILE_PATH_ENV,
                SKILL_PATH_ENV,
                CONFIG_FILE_ENV,
                SESSION_ENV_ENV,
            ]
        );
    }

    /// The socket every child is told is an override only when it is not the run directory's
    /// own: otherwise the child would hand on a `--socket` its parent never had.
    #[test]
    fn a_child_told_the_default_socket_holds_its_parents_config() {
        let global = Global::default();
        let parent = config(&global, &[]);
        assert_eq!(parent.tmux_socket, None);
        let context = exec_context_in(&global, &parent, &environment(&[]));
        let variables: Vec<(&str, &str)> = context
            .environment
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        assert!(variables.contains(&("TX_TMUX_SOCKET", "/tmp/tx-ide.u/tmux.sock")));
        let child = config(&global, &variables);
        assert_eq!(child.tmux_socket, None);
        assert_eq!(child.cli_arguments(), parent.cli_arguments());
        // One that is elsewhere stays an override, from the variable as from the flag.
        let moved = config(&global, &[("TX_TMUX_SOCKET", "/short/s.sock")]);
        assert_eq!(moved.tmux_socket, Some(PathBuf::from("/short/s.sock")));
        let flagged = Global {
            socket: Some("/tmp/tx-ide.u/tmux.sock".into()),
            ..Global::default()
        };
        assert_eq!(config(&flagged, &[]).tmux_socket, None);
    }

    #[test]
    fn a_variable_that_named_the_roots_is_handed_on_absolute() {
        let global = Global::default();
        let variables = [(PLUGIN_PATH_ENV, "relative/root")];
        let context = exec_context_in(
            &global,
            &config(&global, &variables),
            &environment(&variables),
        );
        let root = std::env::current_dir().unwrap().join("relative/root");
        assert!(
            context.environment.contains(&(
                PLUGIN_PATH_ENV.to_owned(),
                root.to_string_lossy().into_owned()
            )),
            "{:?}",
            context.environment
        );
    }
}
