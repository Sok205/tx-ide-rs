//! `CoreConfig`: everything the library needs, handed in by the caller. The library never reads
//! the process environment.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agent::{AgentId, DefinitionChoice};
use crate::core::LaunchOptions;
use crate::engine::EngineKind;
use crate::error::CoreError;

/// The longest tmux socket path `open` accepts. `sun_path` holds 104 bytes on macOS and 108 on
/// Linux, and tmux fails opaquely near the limit; 100 leaves room for the NUL and some slack.
pub const MAX_SOCKET_PATH_BYTES: usize = 100;

pub const SOCKET_FILE: &str = "tmux.sock";
pub const CONF_FILE: &str = "tmux.conf";
/// Under the data directory: one directory per agent, and one per plugin.
pub const AGENTS_DIR: &str = "agents";
pub const PLUGINS_DIR: &str = "plugins";
/// Under the data directory: the detached worktrees agents run in, one tree per agent.
pub const WORKTREES_DIR: &str = "worktrees";
/// Under an agent's directory: the engine's archive of its conversations (`LlmEngine::archive`).
pub const ARCHIVE_DIR: &str = "archive";
/// Under the data directory: the definitions tx ships (`agents/`, `skills/`, `mcp.json`), written
/// by `ui install`. Not `<data>/agents/`, which holds the agent records.
pub const SHIPPED_DEFINITIONS_DIR: &str = "definitions";
/// Under the config directory: the user's own definitions, in the same layout; they win by name.
pub const USER_DEFINITIONS_DIR: &str = "definitions";

/// The five directories tx keeps its files in, resolved by the front end (XDG, `TX_APPNAME`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directories {
    /// Written by the user: `config.yaml`, `roles/`, `definitions/`.
    pub config: PathBuf,
    /// Must survive: records, the log, plugin data, the files `ui install` generates.
    pub data: PathBuf,
    /// May be lost: locks, profiles.
    pub state: PathBuf,
    /// Rebuildable.
    pub cache: PathBuf,
    /// Per boot: the tmux socket.
    pub run: PathBuf,
}

impl Directories {
    /// `root/{config,data,state,cache,run}`. Test support, not a production fallback.
    pub fn under(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config: root.join("config"),
            data: root.join("data"),
            state: root.join("state"),
            cache: root.join("cache"),
            run: root.join("run"),
        }
    }

    /// Each directory with the flag that names it, in a fixed order.
    pub fn flags(&self) -> [(&'static str, &Path); 5] {
        [
            ("--config-dir", &self.config),
            ("--data-dir", &self.data),
            ("--state-dir", &self.state),
            ("--cache-dir", &self.cache),
            ("--run-dir", &self.run),
        ]
    }

    /// The definition layers, lowest first: the shipped `<data>/definitions/`, then the user's
    /// `<config>/definitions/`, which wins by name.
    pub fn definition_roots(&self) -> [PathBuf; 2] {
        [
            self.data.join(SHIPPED_DEFINITIONS_DIR),
            self.config.join(USER_DEFINITIONS_DIR),
        ]
    }

    /// Each directory with the variable core exports it as into every session.
    pub fn environment(&self) -> [(&'static str, &Path); 5] {
        [
            ("TX_CONFIG_DIR", &self.config),
            ("TX_DATA_DIR", &self.data),
            ("TX_STATE_DIR", &self.state),
            ("TX_CACHE_DIR", &self.cache),
            ("TX_RUN_DIR", &self.run),
        ]
    }
}

/// `config.yaml`'s `agent:` block: what a launch uses for everything the caller left unset.
/// `Default` is what a missing or empty file means; [`AgentDefaults::none`] is a core nobody
/// configured.
///
/// ```yaml
/// agent:
///   engine: claude
///   roles: []                    # empty: the definition's `roles`
///   definition: developer        # null: roles alone, with the engine's base tools
///   model: claude-opus-5-5       # null: the engine's own default
///   effort: 3                    # 1-5 or the engine's word; null: the engine's own default
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentDefaults {
    pub engine: EngineKind,
    /// Empty: the definition's own `roles`.
    pub roles: Vec<String>,
    /// The agent definition every launch without one of its own runs (its prompt, tools, skills
    /// and roles). `None`: roles alone, with the engine's base tools.
    pub definition: Option<String>,
    /// A model of `engine`: another engine gets its own default.
    pub model: Option<String>,
    /// A number on the tx scale or an engine's word, as text; the engine checks it.
    #[serde(deserialize_with = "crate::effort::deserialize_optional_effort")]
    pub effort: Option<String>,
}

impl Default for AgentDefaults {
    fn default() -> Self {
        Self {
            engine: EngineKind::Claude,
            roles: Vec::new(),
            definition: Some("developer".to_owned()),
            model: Some("claude-opus-5-5".to_owned()),
            effort: Some("3".to_owned()),
        }
    }
}

impl AgentDefaults {
    /// Nothing configured: claude, with no roles, definition, model or effort of its own. What
    /// [`CoreConfig::new`] holds until the caller hands in a file's values.
    pub fn none() -> Self {
        Self {
            engine: EngineKind::Claude,
            roles: Vec::new(),
            definition: None,
            model: None,
            effort: None,
        }
    }

    /// The launch `tx create` makes: what the caller chose, with these defaults for the rest.
    ///
    /// - An unset `engine` is `self.engine`.
    /// - Empty `roles` are `self.roles`, but only when no `definition` was chosen either: a
    ///   definition without roles runs its own `roles`.
    /// - `self.model` goes in when the launch's engine is `self.engine`, and `self.effort` always;
    ///   both before `options`, so a caller's key for the same name wins.
    /// - `definition` passes through; a launch without one runs `self.definition`, filled in at
    ///   create so that forks, respawns and in-process callers get it too.
    pub fn launch(
        &self,
        engine: Option<EngineKind>,
        roles: Vec<String>,
        definition: Option<DefinitionChoice>,
        options: impl IntoIterator<Item = (String, String)>,
        prompt: impl Into<String>,
    ) -> LaunchOptions {
        let engine = engine.unwrap_or(self.engine);
        let roles = if roles.is_empty() && definition.is_none() {
            self.roles.clone()
        } else {
            roles
        };
        let model = self.model.clone().filter(|_| engine == self.engine);
        let file_options = [("model", model), ("effort", self.effort.clone())]
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)));
        let mut launch = LaunchOptions::agent(engine)
            .profile(roles)
            .initial_prompt(prompt);
        if let Some(choice) = definition {
            launch = launch.definition(choice);
        }
        for (key, value) in file_options.chain(options) {
            launch = launch.option(key, value);
        }
        launch
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CoreConfig {
    pub directories: Directories,
    /// Overrides `<run>/tmux.sock` when that path would be too long.
    pub tmux_socket: Option<PathBuf>,
    /// Base environment for every session, applied before the per-session env.
    pub env: Vec<(String, String)>,
    /// `config.yaml`'s `agent:` block. Its `definition` is what a launch without one of its own
    /// runs (create, fork, and the respawn of a record from before definitions); the rest is
    /// applied by [`AgentDefaults::launch`]. [`AgentDefaults::none`] until the caller sets it.
    pub agent_defaults: AgentDefaults,
    /// The caller's environment. Never passed into a session whole: a launch takes from it only
    /// the variables its MCP servers name (`${NAME}`), and refuses when one is unset.
    pub caller_env: Vec<(String, String)>,
    /// Role search dirs, in order; a later dir overrides an earlier one.
    pub profile_dirs: Vec<PathBuf>,
    /// Skill search dirs, in order; a later dir overrides an earlier one.
    pub skill_dirs: Vec<PathBuf>,
    /// The argv prefix the hooks an engine wires run (`[tx, "chat", "hook"]`). Empty: none.
    pub hook_command: Vec<String>,
}

impl CoreConfig {
    pub fn new(directories: Directories) -> Self {
        Self {
            directories,
            tmux_socket: None,
            env: Vec::new(),
            agent_defaults: AgentDefaults::none(),
            caller_env: Vec::new(),
            profile_dirs: Vec::new(),
            skill_dirs: Vec::new(),
            hook_command: Vec::new(),
        }
    }

    pub fn tmux_socket(mut self, socket: impl Into<PathBuf>) -> Self {
        self.tmux_socket = Some(socket.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn agent_defaults(mut self, defaults: AgentDefaults) -> Self {
        self.agent_defaults = defaults;
        self
    }

    pub fn caller_env<I, K, V>(mut self, variables: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.caller_env.extend(
            variables
                .into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        self
    }

    pub fn profile_dirs<I, P>(mut self, dirs: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.profile_dirs.extend(dirs.into_iter().map(Into::into));
        self
    }

    pub fn skill_dirs<I, P>(mut self, dirs: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PathBuf>,
    {
        self.skill_dirs.extend(dirs.into_iter().map(Into::into));
        self
    }

    pub fn hook_command<I, S>(mut self, words: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.hook_command = words.into_iter().map(Into::into).collect();
        self
    }

    pub fn socket_path(&self) -> PathBuf {
        self.tmux_socket
            .clone()
            .unwrap_or_else(|| self.directories.run.join(SOCKET_FILE))
    }

    /// `<data>/tmux.conf`: the server config, generated by `ui install`.
    pub fn tmux_conf_path(&self) -> PathBuf {
        self.directories.data.join(CONF_FILE)
    }

    /// The data directory of one agent: `<data>/agents/<id>/`.
    pub fn agent_dir(&self, id: &AgentId) -> PathBuf {
        self.directories.data.join(AGENTS_DIR).join(id.to_string())
    }

    /// Where the engine archives one agent's conversations: `<data>/agents/<id>/archive/`.
    /// Nothing is created.
    pub fn archive_directory(&self, id: &AgentId) -> PathBuf {
        self.agent_dir(id).join(ARCHIVE_DIR)
    }

    /// A plugin's own data directory: `<data>/plugins/<plugin>/`. Nothing is created.
    pub fn plugin_data_dir(&self, plugin: &str) -> PathBuf {
        self.directories.data.join(PLUGINS_DIR).join(plugin)
    }

    /// The root of the agents' worktrees: `<data>/worktrees/<repository-key>/<slug>--<name>`.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.directories.data.join(WORKTREES_DIR)
    }

    /// The flags a re-invocation of `tx` / `txi` needs to open this same core: the five
    /// directories, and the socket when it is overridden.
    pub fn cli_arguments(&self) -> Vec<String> {
        let mut arguments = Vec::new();
        for (flag, directory) in self.directories.flags() {
            arguments.push(flag.to_owned());
            arguments.push(directory.to_string_lossy().into_owned());
        }
        if let Some(socket) = &self.tmux_socket {
            arguments.push("--socket".to_owned());
            arguments.push(socket.to_string_lossy().into_owned());
        }
        arguments
    }

    /// Fails before any file is touched when the socket path cannot fit a `sockaddr_un` or is
    /// not absolute (a relative socket would move with the caller's cwd and be exported into
    /// sessions as such).
    pub fn check_socket_path(&self) -> Result<(), CoreError> {
        let socket = self.socket_path();
        if socket.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
            return Err(CoreError::SocketPathTooLong(socket));
        }
        if !socket.is_absolute() {
            return Err(CoreError::SocketPathNotAbsolute(socket));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn paths_follow_the_directories() {
        let config = CoreConfig::new(Directories::under("/tmp/tx"));
        assert_eq!(config.socket_path(), PathBuf::from("/tmp/tx/run/tmux.sock"));
        assert_eq!(
            config.tmux_conf_path(),
            PathBuf::from("/tmp/tx/data/tmux.conf")
        );
        assert_eq!(
            config.plugin_data_dir("chat"),
            PathBuf::from("/tmp/tx/data/plugins/chat")
        );
        assert_eq!(
            config.worktrees_dir(),
            PathBuf::from("/tmp/tx/data/worktrees")
        );
        let id = AgentId::mint();
        assert_eq!(
            config.agent_dir(&id),
            PathBuf::from(format!("/tmp/tx/data/agents/{id}"))
        );
        assert_eq!(
            config.archive_directory(&id),
            PathBuf::from(format!("/tmp/tx/data/agents/{id}/archive"))
        );
    }

    #[test]
    fn explicit_socket_wins() {
        let mut config = CoreConfig::new(Directories::under("/tmp/tx"));
        config.tmux_socket = Some("/tmp/s.sock".into());
        assert_eq!(config.socket_path(), PathBuf::from("/tmp/s.sock"));
        assert_eq!(
            config.tmux_conf_path(),
            PathBuf::from("/tmp/tx/data/tmux.conf")
        );
    }

    #[test]
    fn cli_arguments_name_every_directory_and_an_overridden_socket() {
        let config = CoreConfig::new(Directories::under("/r"));
        assert_eq!(
            config.cli_arguments(),
            [
                "--config-dir",
                "/r/config",
                "--data-dir",
                "/r/data",
                "--state-dir",
                "/r/state",
                "--cache-dir",
                "/r/cache",
                "--run-dir",
                "/r/run",
            ]
        );
        let config = config.tmux_socket("/s.sock");
        assert_eq!(config.cli_arguments()[10..], ["--socket", "/s.sock"]);
    }

    #[test]
    fn empty_by_default() {
        let config = CoreConfig::new(Directories::under("/x"));
        assert!(config.env.is_empty());
        assert!(config.profile_dirs.is_empty());
        assert!(config.skill_dirs.is_empty());
        assert!(config.hook_command.is_empty());
        assert_eq!(config.agent_defaults, AgentDefaults::none());
        assert_eq!(config.directories.state, Path::new("/x/state"));
    }

    fn fields(
        launch: &LaunchOptions,
    ) -> (
        EngineKind,
        &[String],
        &serde_json::Value,
        &str,
        Option<&str>,
    ) {
        let LaunchOptions::Agent {
            engine,
            profile,
            options,
            initial_prompt,
            definition,
        } = launch;
        (
            *engine,
            profile,
            options,
            initial_prompt,
            definition.as_ref().map(|choice| choice.agent.as_str()),
        )
    }

    fn file() -> AgentDefaults {
        AgentDefaults {
            engine: EngineKind::Claude,
            roles: vec!["COMMON".to_owned()],
            definition: Some("developer".to_owned()),
            model: Some("opus".to_owned()),
            effort: Some("3".to_owned()),
        }
    }

    #[test]
    fn a_launch_that_chose_nothing_takes_the_file() {
        let launch = file().launch(None, Vec::new(), None, [], "start");
        let (engine, roles, options, prompt, definition) = fields(&launch);
        assert_eq!(engine, EngineKind::Claude);
        assert_eq!(roles, ["COMMON"]);
        assert_eq!(
            *options,
            serde_json::json!({"model": "opus", "effort": "3"})
        );
        assert_eq!(prompt, "start");
        // Filled at create, from the same struct.
        assert_eq!(definition, None);
    }

    #[test]
    fn a_chosen_engine_wins_and_drops_the_files_model() {
        let launch = file().launch(Some(EngineKind::Codex), Vec::new(), None, [], "p");
        let (engine, _, options, _, _) = fields(&launch);
        assert_eq!(engine, EngineKind::Codex);
        assert_eq!(*options, serde_json::json!({"effort": "3"}));
    }

    #[test]
    fn the_files_model_applies_to_its_own_engine_when_chosen_explicitly() {
        let launch = file().launch(Some(EngineKind::Claude), Vec::new(), None, [], "p");
        let (_, _, options, _, _) = fields(&launch);
        assert_eq!(
            *options,
            serde_json::json!({"model": "opus", "effort": "3"})
        );
    }

    #[test]
    fn chosen_roles_replace_the_files_whole_list() {
        let launch = file().launch(None, vec!["QA".to_owned()], None, [], "p");
        assert_eq!(fields(&launch).1, ["QA"]);
    }

    #[test]
    fn a_chosen_definition_without_roles_runs_its_own() {
        let launch = file().launch(None, Vec::new(), Some(DefinitionChoice::new("qa")), [], "p");
        let (_, roles, _, _, definition) = fields(&launch);
        assert!(roles.is_empty());
        assert_eq!(definition, Some("qa"));
    }

    #[test]
    fn a_chosen_definition_with_roles_keeps_both() {
        let launch = file().launch(
            None,
            vec!["QA".to_owned()],
            Some(DefinitionChoice::new("qa")),
            [],
            "p",
        );
        let (_, roles, _, _, definition) = fields(&launch);
        assert_eq!(roles, ["QA"]);
        assert_eq!(definition, Some("qa"));
    }

    #[test]
    fn a_callers_option_beats_the_file_and_others_pass_through() {
        let launch = file().launch(
            None,
            Vec::new(),
            None,
            [
                ("model".to_owned(), "sonnet".to_owned()),
                ("effort".to_owned(), "low".to_owned()),
                ("x".to_owned(), "y".to_owned()),
            ],
            "p",
        );
        assert_eq!(
            *fields(&launch).2,
            serde_json::json!({"model": "sonnet", "effort": "low", "x": "y"})
        );
    }

    #[test]
    fn a_null_model_and_effort_leave_the_options_to_the_caller() {
        let launch = AgentDefaults::none().launch(None, Vec::new(), None, [], "p");
        let (engine, roles, options, _, definition) = fields(&launch);
        assert_eq!(engine, EngineKind::Claude);
        assert!(roles.is_empty());
        assert_eq!(*options, serde_json::json!({}));
        assert_eq!(definition, None);
    }

    #[test]
    fn the_default_is_a_missing_file() {
        let parsed: AgentDefaults = serde_norway::from_str("{}").unwrap();
        assert_eq!(parsed, AgentDefaults::default());
        assert_eq!(parsed.definition.as_deref(), Some("developer"));
        assert_eq!(parsed.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(parsed.effort.as_deref(), Some("3"));
        assert_ne!(parsed, AgentDefaults::none());
    }

    /// Directories whose `run/tmux.sock` lands at exactly `len` bytes.
    fn directories_for_socket_len(len: usize) -> Directories {
        let root = format!("/{}", "d".repeat(len - "/".len() - "/run/tmux.sock".len()));
        let directories = Directories::under(&root);
        assert_eq!(
            CoreConfig::new(directories.clone())
                .socket_path()
                .as_os_str()
                .len(),
            len
        );
        directories
    }

    #[test]
    fn socket_path_of_100_bytes_is_ok() {
        let config = CoreConfig::new(directories_for_socket_len(MAX_SOCKET_PATH_BYTES));
        assert!(config.check_socket_path().is_ok());
    }

    #[test]
    fn socket_path_of_101_bytes_is_too_long() {
        let config = CoreConfig::new(directories_for_socket_len(MAX_SOCKET_PATH_BYTES + 1));
        let error = config.check_socket_path().unwrap_err();
        let CoreError::SocketPathTooLong(path) = &error else {
            panic!("{error:?}")
        };
        assert_eq!(*path, config.socket_path());
        assert!(error.to_string().contains("101 bytes"), "{error}");
    }

    #[test]
    fn relative_or_empty_socket_is_refused() {
        for bad in ["", "rel.sock", "."] {
            let config = CoreConfig::new(Directories::under("/abs")).tmux_socket(bad);
            assert!(
                matches!(
                    config.check_socket_path(),
                    Err(CoreError::SocketPathNotAbsolute(_))
                ),
                "{bad:?}"
            );
        }
        assert!(
            CoreConfig::new(Directories::under("rel"))
                .check_socket_path()
                .is_err()
        );
    }

    #[test]
    fn explicit_long_socket_is_checked_too() {
        let mut config = CoreConfig::new(Directories::under("/short"));
        config.tmux_socket = Some(PathBuf::from(format!("/{}", "s".repeat(120))));
        assert!(matches!(
            config.check_socket_path(),
            Err(CoreError::SocketPathTooLong(_))
        ));
    }
}
