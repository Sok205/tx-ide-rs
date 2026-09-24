//! The OpenAI Codex adapter (engines/codex.py): launch/op argv, the subcommand-identity strip,
//! rollout lookup by id, and the standalone-update hand-off in `prepare_workspace`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::session::State;
use crate::shlex::shlex_join;
use crate::skills::{SKILLS_ENV, link_skills};
use crate::storage::{Home, expand_user};

use super::adapter::{
    CapturedChat, DEFAULT_EFFORT, EngineAdapter, EngineError, LaunchEnv, LaunchOptions,
    StateSource, Stripped, capture_keys, env_get, is_shell_control, split_command,
    takes_next_token,
};
use super::codex_rollout;
use super::codex_update::{self, UpdateRequest};

/// Codex's own home honors `$CODEX_HOME` like the CLI — NOT `$TX_IDE_HOME`.
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";
pub const HOME_ENV: &str = "HOME";
pub const PATH_ENV: &str = "PATH";
pub const DEFAULT_CODEX_HOME_BASENAME: &str = ".codex";
pub const DEFAULT_CODEX_HOME: &str = "~/.codex";

pub const CODEX_BIN: &str = "codex";
/// Where codex discovers workspace skills.
pub const SKILLS_DIR: &str = ".agents/skills";

pub const CODEX_MODEL: &str = "gpt-5.6-sol";
pub const REASONING_EFFORT_KEY: &str = "model_reasoning_effort";
/// Additive instructions channel (appended to codex's base prompt).
pub const DEVELOPER_INSTRUCTIONS_KEY: &str = "developer_instructions";
pub const CONFIG_FILE_NAME: &str = "config.toml";
/// `$TX_IDE_HOME/<this>` holds the update log.
pub const UPDATE_STATE_DIR: &str = "codex-update";

pub const BYPASS_APPROVALS_FLAG: &str = "--dangerously-bypass-approvals-and-sandbox";
pub const BYPASS_HOOK_TRUST_FLAG: &str = "--dangerously-bypass-hook-trust";
pub const YOLO_FLAGS: [&str; 2] = [BYPASS_APPROVALS_FLAG, BYPASS_HOOK_TRUST_FLAG];
pub const READ_ONLY_FLAGS: [&str; 5] = [
    "--sandbox",
    "danger-full-access",
    "--ask-for-approval",
    "never",
    BYPASS_HOOK_TRUST_FLAG,
];

pub const SESSIONS_DIR: &str = "sessions";
pub const ROLLOUT_PREFIX: &str = "rollout-";
pub const TRANSCRIPT_SUFFIX: &str = ".jsonl";

// ----- chat-op command derivation --------------------------------------------------------

const IDENTITY_SUBCOMMANDS: [&str; 2] = ["resume", "fork"];
const IDENTITY_BARE_FLAGS: [&str; 3] = ["--last", "--all", "--include-non-interactive"];
/// Bare flags (consume no following token); codex `-c` is a VALUE flag.
const BARE_FLAGS: [&str; 10] = [
    BYPASS_APPROVALS_FLAG,
    BYPASS_HOOK_TRUST_FLAG,
    "--oss",
    "--search",
    "--no-alt-screen",
    "--strict-config",
    "--help",
    "-h",
    "--version",
    "-V",
];
const VARIADIC_VALUE_FLAGS: [&str; 2] = ["-i", "--image"];

/// Split a source codex `cmd` into (binary, inherited persona flags): the identity subcommand +
/// its id, its session-picker flags and the baked positional prompt dropped.
fn strip_identity(source_cmd: &str) -> Result<Stripped, EngineError> {
    let tokens = split_command(source_cmd)?;
    let binary = tokens.first().map_or(CODEX_BIN, String::as_str).to_owned();
    let mut index = 1;
    if tokens
        .get(index)
        .is_some_and(|token| IDENTITY_SUBCOMMANDS.contains(&token.as_str()))
    {
        index += 1;
        if tokens
            .get(index)
            .is_some_and(|token| !token.starts_with('-'))
        {
            index += 1;
        }
    }
    let mut inherited = Vec::new();
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if is_shell_control(token) {
            break;
        }
        if IDENTITY_BARE_FLAGS.contains(&token) {
            index += 1;
        } else if BARE_FLAGS.contains(&token) {
            inherited.push(token.to_owned());
            index += 1;
        } else if VARIADIC_VALUE_FLAGS.contains(&token) {
            inherited.push(token.to_owned());
            index += 1;
            while index < tokens.len()
                && !tokens[index].starts_with('-')
                && !is_shell_control(&tokens[index])
            {
                inherited.push(tokens[index].clone());
                index += 1;
            }
        } else if token.starts_with('-') {
            if takes_next_token(&tokens, index) {
                inherited.extend_from_slice(&tokens[index..index + 2]);
                index += 2;
            } else {
                inherited.push(token.to_owned());
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    Ok((binary, inherited))
}

/// Drop tx's writable/read-only controls before applying the destination session's mode.
fn strip_access_flags(command: Vec<String>) -> Vec<String> {
    const VALUE_FLAGS: [&str; 4] = ["--sandbox", "-s", "--ask-for-approval", "-a"];
    let mut stripped = Vec::with_capacity(command.len());
    let mut index = 0;
    while index < command.len() {
        let token = command[index].as_str();
        if YOLO_FLAGS.contains(&token) {
            index += 1;
        } else if VALUE_FLAGS.contains(&token) {
            index += 2;
        } else {
            stripped.push(command[index].clone());
            index += 1;
        }
    }
    stripped
}

fn apply_access(command: Vec<String>, read_only: bool) -> Vec<String> {
    let mut command = strip_access_flags(command);
    if read_only {
        command.extend(READ_ONLY_FLAGS.map(String::from));
    } else {
        for flag in YOLO_FLAGS {
            if !command.iter().any(|token| token == flag) {
                command.push(flag.to_owned());
            }
        }
    }
    command
}

// ----- the adapter ------------------------------------------------------------------------

/// Codex has NO session-end event and `SessionStart` is capture-only.
const EVENT_TO_STATE: &[(&str, State)] = &[
    ("UserPromptSubmit", State::Working),
    ("PreToolUse", State::Working),
    ("PostToolUse", State::Working),
    ("PreCompact", State::Working),
    ("PostCompact", State::Working),
    ("SubagentStart", State::Working),
    ("Stop", State::Waiting),
    ("PermissionRequest", State::Waiting),
];

/// The tx process's own environment as far as codex cares, read once at the edge.
#[derive(Clone, Debug, Default)]
pub struct CodexHostEnv {
    /// `$CODEX_HOME`.
    pub codex_home: Option<String>,
    /// `$HOME` (for `~` expansion).
    pub home_dir: Option<PathBuf>,
    /// `$PATH`.
    pub path: Option<String>,
}

/// The OpenAI Codex adapter.
#[derive(Clone, Debug)]
pub struct CodexEngine {
    home: Home,
    host: CodexHostEnv,
    tx_executable: PathBuf,
}

impl CodexEngine {
    /// `tx_executable` is the running `tx` binary (`std::env::current_exe()`), re-executed as the
    /// detached update child.
    pub fn new(home: Home, host: CodexHostEnv, tx_executable: PathBuf) -> Self {
        Self {
            home,
            host,
            tx_executable,
        }
    }

    /// Codex's home as the SPAWNED process will resolve it: launch-env overrides win over the
    /// parent's environment, and a present-but-empty `CODEX_HOME` clears rather than inherits.
    pub fn codex_home(&self, launch_env: &[(String, String)]) -> PathBuf {
        let explicit = match launch_env.iter().find(|(key, _)| key == CODEX_HOME_ENV) {
            Some((_, value)) => Some(value.as_str()),
            None => self.host.codex_home.as_deref(),
        };
        if let Some(explicit) = explicit.filter(|value| !value.is_empty()) {
            return expand_user(explicit, self.host.home_dir.as_deref());
        }
        if let Some(home) = env_get(launch_env, HOME_ENV).filter(|home| !home.is_empty()) {
            return Path::new(home).join(DEFAULT_CODEX_HOME_BASENAME);
        }
        expand_user(DEFAULT_CODEX_HOME, self.host.home_dir.as_deref())
    }

    pub fn sessions_root(&self) -> PathBuf {
        self.codex_home(&[]).join(SESSIONS_DIR)
    }

    /// The configured `developer_instructions` from config.toml, prepended to role priming; an
    /// absent/unreadable/invalid file or a blank/non-string value is `None`.
    pub fn configured_developer_instructions(
        &self,
        launch_env: &[(String, String)],
    ) -> Option<String> {
        let text =
            std::fs::read_to_string(self.codex_home(launch_env).join(CONFIG_FILE_NAME)).ok()?;
        let table: toml::Table = text.parse().ok()?;
        let value = table.get(DEVELOPER_INSTRUCTIONS_KEY)?.as_str()?;
        (!value.trim().is_empty()).then(|| value.to_owned())
    }

    /// The most recent `sessions/**/rollout-*-<id>.jsonl`, or `None`.
    pub fn find_rollout(&self, chat_id: &str) -> Option<PathBuf> {
        let suffix = format!("-{chat_id}{TRANSCRIPT_SUFFIX}");
        let mut matches = Vec::new();
        collect_rollouts(&self.sessions_root(), &suffix, &mut matches);
        matches.sort();
        matches.pop()
    }

    fn launch(&self, options: &LaunchOptions<'_>, env: &[(String, String)]) -> Vec<String> {
        let effort = options.effort.unwrap_or(DEFAULT_EFFORT);
        let model = options.model.filter(|model| !model.is_empty());
        let mut command = vec![
            CODEX_BIN.to_owned(),
            "-m".to_owned(),
            model.unwrap_or(CODEX_MODEL).to_owned(),
            "-c".to_owned(),
            format!("{REASONING_EFFORT_KEY}={}", effort.as_str()),
        ];
        if let Some(priming) = options.role_priming.filter(|p| !p.is_empty()) {
            let instructions = match self.configured_developer_instructions(env) {
                Some(configured) => format!("{configured}\n\n{priming}"),
                None => priming.to_owned(),
            };
            command.extend([
                "-c".to_owned(),
                format!("{DEVELOPER_INSTRUCTIONS_KEY}={instructions}"),
            ]);
        }
        let mut command = apply_access(command, options.read_only);
        if let Some(prompt) = options.initial_prompt.filter(|p| !p.is_empty()) {
            command.push(prompt.to_owned());
        }
        command
    }
}

/// pathlib's `**` walk: hidden entries included, symlinked dirs not descended.
fn collect_rollouts(directory: &Path, suffix: &str, matches: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() >= ROLLOUT_PREFIX.len() + suffix.len()
            && name.starts_with(ROLLOUT_PREFIX)
            && name.ends_with(suffix)
        {
            matches.push(entry.path());
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_rollouts(&entry.path(), suffix, matches);
        }
    }
}

impl EngineAdapter for CodexEngine {
    fn binary(&self) -> &str {
        CODEX_BIN
    }

    fn capture_session_id(&self, hook_payload: &Value) -> Option<CapturedChat> {
        capture_keys(hook_payload, "session_id", "transcript_path")
    }

    /// A positional prompt auto-submits in the TUI; `browser` is accepted and ignored.
    fn build_launch_command(
        &self,
        options: &LaunchOptions<'_>,
        env: &mut LaunchEnv,
    ) -> Result<Vec<String>, EngineError> {
        Ok(self.launch(options, env))
    }

    fn resume_command(
        &self,
        chat_id: &str,
        source_cmd: Option<&str>,
        read_only: bool,
    ) -> Result<Vec<String>, EngineError> {
        let (binary, inherited) = match source_cmd {
            Some(source_cmd) => strip_identity(source_cmd)?,
            None => (CODEX_BIN.to_owned(), Vec::new()),
        };
        let mut command = vec![binary, "resume".to_owned(), chat_id.to_owned()];
        command.extend(inherited);
        Ok(apply_access(command, read_only))
    }

    fn fork_command(
        &self,
        source_cmd: &str,
        chat_id: &str,
        read_only: bool,
    ) -> Result<Vec<String>, EngineError> {
        let (binary, inherited) = strip_identity(source_cmd)?;
        let mut command = vec![binary, "fork".to_owned(), chat_id.to_owned()];
        command.extend(inherited);
        Ok(apply_access(command, read_only))
    }

    fn seed_command(
        &self,
        source_cmd: &str,
        seed: &str,
        read_only: bool,
    ) -> Result<Vec<String>, EngineError> {
        let (binary, mut command) = strip_identity(source_cmd)?;
        command.insert(0, binary);
        let mut command = apply_access(command, read_only);
        command.push(seed.to_owned());
        Ok(command)
    }

    /// A fresh codex at default model/effort with `seed` appended (even when empty).
    fn distiller_command(&self, seed: &str) -> Vec<String> {
        let mut command = self.launch(&LaunchOptions::default(), &[]);
        command.push(seed.to_owned());
        command
    }

    /// Rollouts are global by id, not cwd-keyed: nothing to relocate.
    fn prepare_chat_for_cwd(&self, _: &str, _: &str, _: &str) -> Result<(), EngineError> {
        Ok(())
    }

    /// Grant skills, and move a standalone install's update check out of the interactive
    /// session: schedule the detached installer run and disable codex's own startup check.
    fn prepare_workspace(
        &self,
        command: &str,
        cwd: &str,
        env: &[(String, String)],
    ) -> Result<String, EngineError> {
        link_skills(
            &self.home,
            Path::new(cwd),
            env_get(env, SKILLS_ENV),
            SKILLS_DIR,
        )?;
        let tokens = split_command(command)?;
        let Some(binary) = tokens.first() else {
            return Ok(command.to_owned());
        };
        if tokens.iter().any(|token| is_shell_control(token)) {
            return Ok(command.to_owned());
        }
        let codex_home = self.codex_home(env);
        let Some(executable) = codex_update::standalone_executable(
            binary,
            env_get(env, PATH_ENV),
            self.host.path.as_deref(),
            &codex_home,
        ) else {
            return Ok(command.to_owned());
        };
        let request = UpdateRequest {
            executable: &executable,
            codex_home: &codex_home,
            environment: env,
            state_directory: &self.home.root().join(UPDATE_STATE_DIR),
            tx_executable: &self.tx_executable,
        };
        if let Err(error) = codex_update::schedule_update(&request) {
            eprintln!(
                "tx: could not schedule the Codex update check: {error}; launching the current version"
            );
        }
        Ok(shlex_join(&codex_update::disable_startup_update_check(
            &tokens,
        )))
    }

    fn is_read_only_command(&self, command: &str) -> bool {
        let Ok(tokens) = split_command(command) else {
            return false;
        };
        if tokens.iter().any(|token| token == BYPASS_APPROVALS_FLAG) {
            return false;
        }
        let value_after = |flag: &str| {
            let position = tokens.iter().position(|token| token == flag)?;
            tokens.get(position + 1).map(String::as_str)
        };
        let (Some(sandbox), Some(approval)) =
            (value_after("--sandbox"), value_after("--ask-for-approval"))
        else {
            return false;
        };
        sandbox == "danger-full-access" && approval == "never"
    }

    /// The rollout by id; `cwd` is unused. A non-existent sentinel when none is on disk yet.
    fn resolve_transcript(&self, chat_id: &str, _cwd: &str) -> PathBuf {
        self.find_rollout(chat_id).unwrap_or_else(|| {
            self.sessions_root()
                .join(format!("{ROLLOUT_PREFIX}{chat_id}{TRANSCRIPT_SUFFIX}"))
        })
    }

    /// Q7 FIX: a codex chat's fallback is its own rollout tree, never Claude's projects root.
    fn relocate_transcript(&self, chat_id: &str, _cwd_hint: Option<&str>) -> Option<PathBuf> {
        self.find_rollout(chat_id)
    }

    /// Responses items → `{role, text, is_environment_context}`.
    fn iter_messages(&self, transcript: &Path) -> Result<Vec<Value>, EngineError> {
        Ok(codex_rollout::iter_messages(transcript)?
            .into_iter()
            .map(|message| {
                json!({
                    "role": message.role,
                    "text": message.text,
                    "is_environment_context": message.is_environment_context,
                })
            })
            .collect())
    }

    /// The rollout inlines everything: no sidecar.
    fn bundle_sidecars(&self, _: &Path, _: &str) -> Vec<PathBuf> {
        Vec::new()
    }

    fn event_to_state(&self) -> &'static [(&'static str, State)] {
        EVENT_TO_STATE
    }

    fn state_source(&self) -> StateSource {
        StateSource::HookEvents
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::adapter::Effort;

    const DEFAULTS: [&str; 5] = [
        "codex",
        "-m",
        "gpt-5.6-sol",
        "-c",
        "model_reasoning_effort=high",
    ];

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn engine_with(host: CodexHostEnv) -> CodexEngine {
        CodexEngine::new(Home::new("/tx"), host, PathBuf::from("/bin/tx"))
    }

    fn engine() -> CodexEngine {
        engine_with(CodexHostEnv {
            codex_home: Some("/cx".into()),
            home_dir: Some("/h".into()),
            path: None,
        })
    }

    fn env(pairs: &[(&str, &str)]) -> LaunchEnv {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn launch_defaults_effort_model_prompt_read_only() {
        let e = engine();
        let launch =
            |options: LaunchOptions<'_>| e.build_launch_command(&options, &mut Vec::new()).unwrap();
        let mut expected = argv(&DEFAULTS);
        expected.extend(argv(&YOLO_FLAGS));
        assert_eq!(launch(LaunchOptions::default()), expected);
        assert_eq!(
            launch(LaunchOptions {
                browser: true,
                ..LaunchOptions::default()
            }),
            expected
        );
        let max = launch(LaunchOptions {
            effort: Some(Effort::Max),
            model: Some("o3"),
            ..LaunchOptions::default()
        });
        assert_eq!(
            max[1..5],
            argv(&["-m", "o3", "-c", "model_reasoning_effort=max"])
        );
        let mut expected = argv(&DEFAULTS);
        expected.extend(argv(&READ_ONLY_FLAGS));
        expected.push("go".into());
        assert_eq!(
            launch(LaunchOptions {
                read_only: true,
                initial_prompt: Some("go"),
                ..LaunchOptions::default()
            }),
            expected
        );
        let mut expected = argv(&DEFAULTS);
        expected.extend(argv(&YOLO_FLAGS));
        expected.push(String::new());
        assert_eq!(e.distiller_command(""), expected);
    }

    #[test]
    fn codex_home_precedence() {
        let e = engine();
        assert_eq!(
            e.codex_home(&env(&[("CODEX_HOME", "/l")])),
            PathBuf::from("/l")
        );
        assert_eq!(
            e.codex_home(&env(&[("CODEX_HOME", ""), ("HOME", "/u")])),
            PathBuf::from("/u/.codex")
        );
        assert_eq!(e.codex_home(&env(&[("HOME", "/u")])), PathBuf::from("/cx"));
        assert_eq!(
            e.codex_home(&env(&[("CODEX_HOME", "")])),
            PathBuf::from("/h/.codex")
        );
        assert_eq!(
            e.codex_home(&env(&[("CODEX_HOME", "~/x")])),
            PathBuf::from("/h/x")
        );
        let unset = engine_with(CodexHostEnv {
            home_dir: Some("/h".into()),
            ..CodexHostEnv::default()
        });
        assert_eq!(
            unset.codex_home(&env(&[("HOME", "/u")])),
            PathBuf::from("/u/.codex")
        );
        assert_eq!(unset.sessions_root(), PathBuf::from("/h/.codex/sessions"));
    }

    #[test]
    fn role_priming_merges_configured_developer_instructions() {
        let dir = tempfile::tempdir().unwrap();
        let e = engine();
        let instructions = |config: Option<&str>| {
            let home = dir
                .path()
                .join(format!("cx-{}", config.map_or(0, str::len)));
            std::fs::create_dir_all(&home).unwrap();
            if let Some(text) = config {
                std::fs::write(home.join(CONFIG_FILE_NAME), text).unwrap();
            }
            let mut launch_env = env(&[("CODEX_HOME", home.to_str().unwrap())]);
            let command = e
                .build_launch_command(
                    &LaunchOptions {
                        role_priming: Some("C\n\nP"),
                        ..LaunchOptions::default()
                    },
                    &mut launch_env,
                )
                .unwrap();
            assert_eq!(command[5], "-c");
            command[6].clone()
        };
        assert_eq!(
            instructions(Some("developer_instructions = \"Base\"\n")),
            "developer_instructions=Base\n\nC\n\nP"
        );
        for config in [
            None,
            Some("this is = not [toml\n"),
            Some("developer_instructions = \"  \"\n"),
            Some("developer_instructions = 3\n"),
        ] {
            assert_eq!(
                instructions(config),
                "developer_instructions=C\n\nP",
                "{config:?}"
            );
        }
    }

    fn fork(cmd: &str) -> Vec<String> {
        let command = engine().fork_command(cmd, "r", false).unwrap();
        assert_eq!(command[..3], argv(&["codex", "fork", "r"]), "{cmd}");
        assert_eq!(command[command.len() - 2..], argv(&YOLO_FLAGS), "{cmd}");
        command[3..command.len() - 2].to_vec()
    }

    #[test]
    fn strip_identity_grammar() {
        // Expected values from the Python reference.
        let cases: &[(&str, &[&str])] = &[
            ("codex resume abc -m x", &["-m", "x"]),
            ("codex fork abc --last -m x", &["-m", "x"]),
            ("codex resume --last --all --include-non-interactive", &[]),
            ("codex resume", &[]),
            ("codex --oss -m x \"prompt\"", &["--oss", "-m", "x"]),
            ("codex -c k=v -m x", &["-c", "k=v", "-m", "x"]),
            (
                "codex -i a.png b.png -m x",
                &["-i", "a.png", "b.png", "-m", "x"],
            ),
            ("codex --unknown v \"p\"", &["--unknown", "v"]),
            ("codex --unknown --search", &["--unknown", "--search"]),
            ("codex --search \"p\" | tee x", &["--search"]),
            ("codex -m x --sandbox read-only -a never", &["-m", "x"]),
            ("", &[]),
        ];
        for (cmd, inherited) in cases {
            assert_eq!(fork(cmd), argv(inherited), "{cmd}");
        }
    }

    #[test]
    fn resume_and_seed_commands() {
        let e = engine();
        let src = "codex -m gpt-5.6-sol -c model_reasoning_effort=high --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust \"seed\"";
        let mut expected = argv(&["codex", "resume", "r0"]);
        expected.extend(argv(&YOLO_FLAGS));
        assert_eq!(e.resume_command("r0", None, false).unwrap(), expected);
        let mut expected = argv(&["codex", "resume", "r1"]);
        expected.extend(argv(&DEFAULTS[1..]));
        expected.extend(argv(&YOLO_FLAGS));
        assert_eq!(e.resume_command("r1", Some(src), false).unwrap(), expected);
        let mut expected = argv(&DEFAULTS);
        expected.extend(argv(&READ_ONLY_FLAGS));
        expected.push("S".into());
        assert_eq!(e.seed_command(src, "S", true).unwrap(), expected);
    }

    #[test]
    fn read_only_command_detection() {
        let e = engine();
        let cases = [
            (
                "codex -m x --sandbox danger-full-access --ask-for-approval never",
                true,
            ),
            (
                "codex --sandbox danger-full-access --ask-for-approval never --dangerously-bypass-approvals-and-sandbox",
                false,
            ),
            ("codex --sandbox read-only --ask-for-approval never", false),
            (
                "codex --sandbox danger-full-access --ask-for-approval on-request",
                false,
            ),
            ("codex --sandbox danger-full-access", false),
            (
                "codex --sandbox danger-full-access --ask-for-approval",
                false,
            ),
            ("codex 'x", false),
        ];
        for (command, expected) in cases {
            assert_eq!(e.is_read_only_command(command), expected, "{command}");
        }
    }

    #[test]
    fn rollout_lookup_sentinel_and_q7_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let cx = dir.path().join("cx");
        let e = engine_with(CodexHostEnv {
            codex_home: Some(cx.to_str().unwrap().to_owned()),
            ..CodexHostEnv::default()
        });
        let sentinel = cx.join("sessions/rollout-r1.jsonl");
        assert_eq!(e.resolve_transcript("r1", "/w"), sentinel);
        assert_eq!(e.locate_transcript("r1", Some("/w")), None);
        assert_eq!(e.locate_transcript("r1", None), None);
        let write = |date: &str, name: &str| {
            let path = cx.join("sessions").join(date).join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{}\n").unwrap();
            path
        };
        write("2026/09/16", "rollout-2026-09-16T10-00-00-r1x.jsonl");
        write("2026/09/16", "rollout-r1.jsonl");
        assert_eq!(e.find_rollout("r1"), None);
        let early = write("2026/09/16", "rollout-2026-09-16T10-00-00-r1.jsonl");
        assert_eq!(e.resolve_transcript("r1", "/anything"), early);
        let later = write("2026/09/17", "rollout-2026-09-17T10-00-00-r1.jsonl");
        assert_eq!(e.resolve_transcript("r1", ""), later);
        assert_eq!(e.locate_transcript("r1", None), Some(later));
        assert_eq!(
            e.bundle_sidecars(Path::new("/s/rollout-x-r1.jsonl"), "r1"),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn capture_messages_and_event_table() {
        let e = engine();
        let payload = json!({"session_id": "u2", "transcript_path": "/s/rollout-x-u2.jsonl"});
        assert_eq!(
            e.capture_session_id(&payload).map(|c| c.session_id),
            Some("u2".into())
        );
        assert_eq!(e.state_for_event("PostCompact"), Some(State::Working));
        assert_eq!(e.state_for_event("SessionEnd"), None);
        assert_eq!(e.state_for_event("SessionStart"), None);
        assert!(e.matches_binary("/opt/codex resume"));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"session_meta\",\"payload\":{}}\n{\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"hi\"}]}}\n",
        )
        .unwrap();
        assert_eq!(
            e.iter_messages(&path).unwrap(),
            [json!({"role": "user", "text": "hi", "is_environment_context": false})]
        );
    }

    #[test]
    fn prepare_workspace_passthrough_for_non_standalone_and_shell_commands() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let home = Home::new(base.join("tx"));
        let e = CodexEngine::new(
            home.clone(),
            CodexHostEnv {
                codex_home: Some(base.join("cx").to_str().unwrap().to_owned()),
                home_dir: None,
                path: Some(base.join("bin").to_str().unwrap().to_owned()),
            },
            PathBuf::from("/bin/true"),
        );
        let work = base.join("w");
        std::fs::create_dir(&work).unwrap();
        let skills = env(&[("TX_SKILLS", "")]);
        for command in ["codex -m x", "codex -m x | tee /dev/null", ""] {
            assert_eq!(
                e.prepare_workspace(command, work.to_str().unwrap(), &skills)
                    .unwrap(),
                command
            );
        }
        assert!(!home.root().join(UPDATE_STATE_DIR).exists());
    }
}
