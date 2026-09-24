//! `SpawnSpec` (spawn.py): the single validated value object for a spawn, plus the role inference
//! from a launch command. The engine table is passed in explicitly (the reference reads the
//! process-wide registry every adapter self-registers into).

use crate::engines::EngineRegistry;
use crate::engines::adapter::LaunchEnv;
use crate::session::{Engine, Role};
use crate::shlex::shlex_quote;

/// Nvim companion command: `tmux new-session -d` strips the terminal's OSC11 background hint, so
/// dark + tokyonight-moon is forced.
pub const NVIM_BASE_COMMAND: &str = "nvim +'set background=dark | colorscheme tokyonight-moon'";
pub const SHELL_COMMANDS: [&str; 5] = ["zsh", "bash", "sh", "fish", "dash"];

/// Role (not engine) from a launch command's binary: any registered engine → llm, `nvim` → nvim,
/// a login shell → shell, else other. Pass an already-resolved command.
pub fn infer_role(command: &str, engines: &EngineRegistry) -> Role {
    let binary = command
        .split_whitespace()
        .next()
        .map_or("", |first| first.rsplit('/').next().unwrap_or(first));
    if engines.engine_for_command(command).is_some() {
        Role::Llm
    } else if binary == "nvim" {
        Role::Nvim
    } else if SHELL_COMMANDS.contains(&binary) {
        Role::Shell
    } else {
        Role::Other
    }
}

/// D11: the nvim launch command with `--listen <socket>` right after the binary, ahead of every
/// `+cmd` / file argument. The record keeps the command without it.
pub fn nvim_listen_command(cmd: &str, socket: &str) -> String {
    let (binary, rest) = cmd.split_once(' ').unwrap_or((cmd, ""));
    let mut launch = format!("{binary} --listen {}", shlex_quote(socket));
    if !rest.is_empty() {
        launch.push(' ');
        launch.push_str(rest);
    }
    launch
}

/// Everything needed to bring one session into being. Built via the constructors below, then
/// adjusted through the public fields (the reference's keyword arguments). `cmd` is the final
/// engine command that is persisted; `launch_cmd`, when set, is what tmux runs instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnSpec {
    pub name: String,
    pub role: Role,
    pub cwd: String,
    pub cmd: String,
    pub tags: Vec<String>,
    /// Explicit effort-group override (`--group`); `None` = derived at read time.
    pub group: Option<String>,
    /// Work-ancestor override (a chat-op's SOURCE id); `None` = the executing managed session.
    pub parent: Option<String>,
    pub env: LaunchEnv,
    pub read_only: bool,
    /// Declared at spawn (`--engine`), stamped on the record, never inferred from `cmd` later.
    /// `None` is only valid for a non-llm session.
    pub engine: Option<Engine>,
    /// A chat-op that records its own `ChatRef` sets this so the spawn writes no pending one.
    pub records_own_chat: bool,
    /// Execution-only wrapper (read-only sandbox); the record keeps `cmd`.
    pub launch_cmd: Option<String>,
}

impl SpawnSpec {
    fn new(name: String, role: Role, cwd: String, cmd: String) -> Self {
        Self {
            name,
            role,
            cwd,
            cmd,
            tags: Vec::new(),
            group: None,
            parent: None,
            env: Vec::new(),
            read_only: false,
            engine: None,
            records_own_chat: false,
            launch_cmd: None,
        }
    }

    /// A worker / agent / shell session (`tx spawn`); the role is inferred from `cmd`.
    pub fn for_process(
        name: impl Into<String>,
        tags: Vec<String>,
        cwd: impl Into<String>,
        cmd: impl Into<String>,
        engines: &EngineRegistry,
    ) -> Self {
        let cmd = cmd.into();
        let role = infer_role(&cmd, engines);
        Self {
            tags,
            ..Self::new(name.into(), role, cwd.into(), cmd)
        }
    }

    /// An nvim companion (`tx spawn-nvim`), optionally opening a diffview against `diff_base`
    /// and/or a file.
    pub fn for_nvim(
        name: impl Into<String>,
        tags: Vec<String>,
        cwd: impl Into<String>,
        diff_base: Option<&str>,
        open_file: Option<&str>,
    ) -> Self {
        let mut cmd = NVIM_BASE_COMMAND.to_owned();
        if let Some(base) = diff_base {
            cmd.push_str(&format!(" +'DiffviewOpen {base}'"));
        }
        if let Some(file) = open_file {
            cmd.push(' ');
            cmd.push_str(&shlex_quote(file));
        }
        Self {
            tags,
            ..Self::new(name.into(), Role::Nvim, cwd.into(), cmd)
        }
    }

    /// A view home base (`tx spawn-view`): never a store record, so it carries no tags (Q4).
    pub fn for_view(
        name: impl Into<String>,
        cwd: impl Into<String>,
        cmd: impl Into<String>,
        engines: &EngineRegistry,
    ) -> Self {
        let cmd = cmd.into();
        let role = infer_role(&cmd, engines);
        Self::new(name.into(), role, cwd.into(), cmd)
    }
}

/// Test doubles shared with the service / reconcile tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use serde_json::Value;

    use crate::engines::EngineRegistry;
    use crate::engines::adapter::{
        CapturedChat, EngineAdapter, EngineError, LaunchEnv, LaunchOptions, StateSource,
    };
    use crate::session::{Engine, State};

    /// An adapter whose binary is all that matters; a command is "read-only" when it carries
    /// ` --read-only`.
    pub(crate) struct Named(pub &'static str);

    impl EngineAdapter for Named {
        fn binary(&self) -> &str {
            self.0
        }
        fn capture_session_id(&self, _: &Value) -> Option<CapturedChat> {
            None
        }
        fn build_launch_command(
            &self,
            _: &LaunchOptions<'_>,
            _: &mut LaunchEnv,
        ) -> Result<Vec<String>, EngineError> {
            Ok(vec![self.0.into()])
        }
        fn resume_command(
            &self,
            _: &str,
            _: Option<&str>,
            _: bool,
        ) -> Result<Vec<String>, EngineError> {
            Ok(Vec::new())
        }
        fn fork_command(&self, _: &str, _: &str, _: bool) -> Result<Vec<String>, EngineError> {
            Ok(Vec::new())
        }
        fn seed_command(&self, _: &str, _: &str, _: bool) -> Result<Vec<String>, EngineError> {
            Ok(Vec::new())
        }
        fn distiller_command(&self, _: &str) -> Vec<String> {
            Vec::new()
        }
        fn prepare_chat_for_cwd(&self, _: &str, _: &str, _: &str) -> Result<(), EngineError> {
            Ok(())
        }
        fn prepare_workspace(
            &self,
            command: &str,
            _: &str,
            _: &[(String, String)],
        ) -> Result<String, EngineError> {
            Ok(command.to_owned())
        }
        fn is_read_only_command(&self, command: &str) -> bool {
            command.contains(" --read-only")
        }
        fn resolve_transcript(&self, _: &str, _: &str) -> PathBuf {
            PathBuf::new()
        }
        fn iter_messages(&self, _: &Path) -> Result<Vec<Value>, EngineError> {
            Ok(Vec::new())
        }
        fn bundle_sidecars(&self, _: &Path, _: &str) -> Vec<PathBuf> {
            Vec::new()
        }
        fn event_to_state(&self) -> &'static [(&'static str, State)] {
            &[]
        }
        fn state_source(&self) -> StateSource {
            StateSource::HookEvents
        }
    }

    pub(crate) fn registry() -> EngineRegistry {
        let mut engines = EngineRegistry::new();
        engines.register(Engine::Claude, Rc::new(Named("claude")));
        engines.register(Engine::Codex, Rc::new(Named("codex")));
        engines.register(Engine::Antigravity, Rc::new(Named("agy")));
        engines
    }
}

#[cfg(test)]
mod tests {
    use super::testing::registry;
    use super::*;

    #[test]
    fn infer_role_table() {
        let engines = registry();
        // python: infer_role(c) with claude / codex / antigravity registered
        let cases = [
            ("claude --model x", Role::Llm),
            ("/usr/local/bin/codex", Role::Llm),
            ("agy", Role::Llm),
            ("nvim +'x'", Role::Nvim),
            ("/opt/bin/nvim", Role::Nvim),
            ("zsh", Role::Shell),
            ("/bin/bash -l", Role::Shell),
            ("dash", Role::Shell),
            ("sleep 30", Role::Other),
            ("claude-code", Role::Other),
            ("", Role::Other),
            ("   ", Role::Other),
        ];
        for (command, role) in cases {
            assert_eq!(infer_role(command, &engines), role, "{command:?}");
        }
        assert_eq!(infer_role("claude", &EngineRegistry::new()), Role::Other);
    }

    #[test]
    fn nvim_listen_goes_right_after_the_binary() {
        assert_eq!(
            nvim_listen_command(NVIM_BASE_COMMAND, "/h/nvim/a b.sock"),
            "nvim --listen '/h/nvim/a b.sock' +'set background=dark | colorscheme tokyonight-moon'"
        );
        assert_eq!(nvim_listen_command("nvim", "/s"), "nvim --listen /s");
    }

    #[test]
    fn nvim_command_shapes() {
        let plain = SpawnSpec::for_nvim("rev", vec!["t".into()], "/w", None, None);
        assert_eq!(plain.cmd, NVIM_BASE_COMMAND);
        assert_eq!((plain.role, plain.tags.len()), (Role::Nvim, 1));
        let full = SpawnSpec::for_nvim("rev", vec![], "/w", Some("abc123"), Some("/w/my plan.md"));
        assert_eq!(
            full.cmd,
            format!("{NVIM_BASE_COMMAND} +'DiffviewOpen abc123' '/w/my plan.md'")
        );
    }

    #[test]
    fn process_and_view_specs() {
        let engines = registry();
        let spec = SpawnSpec::for_process("w", vec!["t".into()], "/r", "claude", &engines);
        assert_eq!((spec.role, spec.engine), (Role::Llm, None));
        assert!(!spec.read_only && !spec.records_own_chat && spec.launch_cmd.is_none());
        let view = SpawnSpec::for_view("Views", "/r", "zsh", &engines);
        assert_eq!((view.role, view.tags.len()), (Role::Shell, 0));
    }
}
