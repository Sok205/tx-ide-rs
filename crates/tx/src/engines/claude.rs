//! The Claude Code adapter (engines/claude.py): launch/op argv, the `_strip_identity` flag
//! grammar, transcript paths (the cwd munge) and the history-bundle layout.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session::State;
use crate::skills::{SKILLS_ENV, link_skills};
use crate::storage::{Home, expand_user};

use super::adapter::{
    CapturedChat, DEFAULT_EFFORT, Effort, EngineAdapter, EngineError, LaunchEnv, LaunchOptions,
    StateSource, Stripped, capture_keys, copy2, copytree, env_get, is_shell_control, realpath,
    split_command, takes_next_token,
};

/// Claude's own home honors `$CLAUDE_CONFIG_DIR` like the CLI — NOT `$TX_IDE_HOME`.
pub const CLAUDE_HOME_ENV: &str = "CLAUDE_CONFIG_DIR";
pub const DEFAULT_CLAUDE_HOME: &str = "~/.claude";

pub const CLAUDE_BIN: &str = "claude";
/// Where claude discovers workspace skills.
pub const SKILLS_DIR: &str = ".claude/skills";
pub const TRANSCRIPT_SUFFIX: &str = ".jsonl";
pub const BUNDLE_TRANSCRIPT_NAME: &str = "transcript.jsonl";
pub const SKIP_PERMISSIONS_FLAG: &str = "--dangerously-skip-permissions";
pub const APPEND_SYSTEM_PROMPT_FLAG: &str = "--append-system-prompt";
pub const CHROME_FLAG: &str = "--chrome";
pub const NO_CHROME_FLAG: &str = "--no-chrome";
pub const READ_ONLY_TOOLS: [&str; 3] = ["Edit", "Write", "NotebookEdit"];
pub const READ_ONLY_ALLOWED_TOOLS: [&str; 1] = ["Bash"];
pub const READ_ONLY_SETTING_SOURCES: &str = "user";

// ----- history bundle layout (engine-neutral; the reference keeps it here) --------------------

pub fn bundle_dir(home: &Home, tx_id: &str, chat_id: &str) -> PathBuf {
    home.history_dir().join(tx_id).join(chat_id)
}

pub fn bundle_transcript_path(home: &Home, tx_id: &str, chat_id: &str) -> PathBuf {
    bundle_dir(home, tx_id, chat_id).join(BUNDLE_TRANSCRIPT_NAME)
}

/// Map an absolute path to Claude's project-dir name: every `/` and `.` becomes `-`.
pub fn munge(cwd: &str) -> String {
    cwd.replace(['/', '.'], "-")
}

// ----- chat-op command derivation (Claude's commander flag grammar) ---------------------------

const IDENTITY_VALUE_FLAGS: [&str; 1] = ["--session-id"];
const IDENTITY_OPTIONAL_VALUE_FLAGS: [&str; 5] =
    ["--resume", "-r", "--from-pr", "--worktree", "-w"];
const IDENTITY_BARE_FLAGS: [&str; 4] = ["--fork-session", "--continue", "-c", "--tmux"];

/// Bare claude flags (consume no following token). Every other `--flag` takes a value.
const BARE_FLAGS: [&str; 27] = [
    "--dangerously-skip-permissions",
    "--allow-dangerously-skip-permissions",
    "--ax-screen-reader",
    "--background",
    "--bg",
    "--forward-subagent-text",
    "--verbose",
    "--print",
    "-p",
    "--ide",
    "--strict-mcp-config",
    "--no-session-persistence",
    "--exclude-dynamic-system-prompt-sections",
    "--replay-user-messages",
    "--include-partial-messages",
    "--include-hook-events",
    "--disable-slash-commands",
    "--chrome",
    "--no-chrome",
    "--bare",
    "--brief",
    "--safe-mode",
    "--mcp-debug",
    "--help",
    "-h",
    "--version",
    "-v",
];

/// Variadic persona flags (commander `<values...>`).
const VARIADIC_VALUE_FLAGS: [&str; 9] = [
    "--add-dir",
    "--allowedTools",
    "--allowed-tools",
    "--disallowedTools",
    "--disallowed-tools",
    "--mcp-config",
    "--betas",
    "--file",
    "--tools",
];

/// Split a source `cmd` into (binary, inherited persona flags): identity flags and the baked
/// positional prompt dropped, shell wrapping truncated.
fn strip_identity(source_cmd: &str) -> Result<Stripped, EngineError> {
    let tokens = split_command(source_cmd)?;
    let binary = tokens.first().map_or(CLAUDE_BIN, String::as_str).to_owned();
    let mut inherited = Vec::new();
    let mut index = 1;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if is_shell_control(token) {
            break;
        }
        if IDENTITY_VALUE_FLAGS.contains(&token) {
            index += 2;
        } else if IDENTITY_OPTIONAL_VALUE_FLAGS.contains(&token) {
            index += if takes_next_token(&tokens, index) {
                2
            } else {
                1
            };
        } else if IDENTITY_BARE_FLAGS.contains(&token) {
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
    let mut stripped = Vec::with_capacity(command.len());
    let mut index = 0;
    while index < command.len() {
        let token = command[index].as_str();
        if token == SKIP_PERMISSIONS_FLAG {
            index += 1;
        } else if token == "--permission-mode"
            || (token == "--setting-sources"
                && command.get(index + 1).map(String::as_str) == Some(READ_ONLY_SETTING_SOURCES))
        {
            index += 2;
        } else if [
            "--allowedTools",
            "--allowed-tools",
            "--disallowedTools",
            "--disallowed-tools",
        ]
        .contains(&token)
        {
            index += 1;
            while index < command.len() && !command[index].starts_with('-') {
                index += 1;
            }
        } else {
            stripped.push(command[index].clone());
            index += 1;
        }
    }
    stripped
}

fn apply_access(command: Vec<String>, read_only: bool) -> Vec<String> {
    let mut command = strip_access_flags(command);
    if !read_only {
        if !command.iter().any(|token| token == SKIP_PERMISSIONS_FLAG) {
            command.push(SKIP_PERMISSIONS_FLAG.to_owned());
        }
        return command;
    }
    command.push("--allowedTools".to_owned());
    command.extend(READ_ONLY_ALLOWED_TOOLS.map(String::from));
    command.push("--disallowedTools".to_owned());
    command.extend(READ_ONLY_TOOLS.map(String::from));
    command.extend(
        [
            "--permission-mode",
            "dontAsk",
            "--setting-sources",
            READ_ONLY_SETTING_SOURCES,
        ]
        .map(String::from),
    );
    command
}

// ----- the adapter ------------------------------------------------------------------------

/// Claude hook event name → live state. `Notification` is payload-dependent, so absent.
const EVENT_TO_STATE: &[(&str, State)] = &[
    ("UserPromptSubmit", State::Working),
    ("PreToolUse", State::Working),
    ("PostToolUse", State::Working),
    ("PostToolUseFailure", State::Working),
    ("SubagentStart", State::Working),
    ("PreCompact", State::Working),
    ("Stop", State::Waiting),
    ("StopFailure", State::Waiting),
    ("PermissionRequest", State::Waiting),
    ("SessionEnd", State::Idle),
];

/// The Claude Code adapter.
#[derive(Clone, Debug)]
pub struct ClaudeEngine {
    home: Home,
    claude_home: PathBuf,
}

impl ClaudeEngine {
    /// `claude_config_dir` is `$CLAUDE_CONFIG_DIR` (unset → `~/.claude`), `~` expanded against
    /// `home_dir` (`$HOME`).
    pub fn new(home: Home, claude_config_dir: Option<&str>, home_dir: Option<&Path>) -> Self {
        let raw = claude_config_dir.unwrap_or(DEFAULT_CLAUDE_HOME);
        Self {
            home,
            claude_home: expand_user(raw, home_dir),
        }
    }

    pub fn claude_home(&self) -> &Path {
        &self.claude_home
    }

    pub fn projects_root(&self) -> PathBuf {
        self.claude_home.join("projects")
    }

    /// claude derives the project dir from the cwd's *realpath*, so symlinks resolve first.
    pub fn project_dir(&self, cwd: &str) -> PathBuf {
        let real = realpath(Path::new(cwd));
        self.projects_root().join(munge(&real.to_string_lossy()))
    }

    pub fn transcript_path(&self, chat_id: &str, cwd: &str) -> PathBuf {
        self.project_dir(cwd)
            .join(format!("{chat_id}{TRANSCRIPT_SUFFIX}"))
    }

    /// The sibling `<chat-uuid>/` dir holding subagents/ + tool-results/.
    pub fn sidecar_dir(&self, chat_id: &str, cwd: &str) -> PathBuf {
        self.project_dir(cwd).join(chat_id)
    }

    pub fn find_transcript(&self, chat_id: &str, cwd: &str) -> Option<PathBuf> {
        Some(self.transcript_path(chat_id, cwd)).filter(|path| path.exists())
    }

    fn launch(&self, options: &LaunchOptions<'_>) -> Vec<String> {
        let mut command = vec![CLAUDE_BIN.to_owned()];
        if let Some(model) = options.model.filter(|model| !model.is_empty()) {
            command.extend(["--model".to_owned(), model.to_owned()]);
        }
        let effort = options.effort.unwrap_or(DEFAULT_EFFORT);
        command.extend(["--effort".to_owned(), effort.as_str().to_owned()]);
        let chrome = if options.browser {
            CHROME_FLAG
        } else {
            NO_CHROME_FLAG
        };
        command.push(chrome.to_owned());
        if let Some(priming) = options.role_priming.filter(|p| !p.is_empty()) {
            command.extend([APPEND_SYSTEM_PROMPT_FLAG.to_owned(), priming.to_owned()]);
        }
        let mut command = apply_access(command, options.read_only);
        if let Some(prompt) = options.initial_prompt.filter(|p| !p.is_empty()) {
            command.push(prompt.to_owned());
        }
        command
    }
}

impl EngineAdapter for ClaudeEngine {
    fn binary(&self) -> &str {
        CLAUDE_BIN
    }

    fn capture_session_id(&self, hook_payload: &Value) -> Option<CapturedChat> {
        capture_keys(hook_payload, "session_id", "transcript_path")
    }

    fn build_launch_command(
        &self,
        options: &LaunchOptions<'_>,
        _env: &mut LaunchEnv,
    ) -> Result<Vec<String>, EngineError> {
        Ok(self.launch(options))
    }

    fn resume_command(
        &self,
        chat_id: &str,
        source_cmd: Option<&str>,
        read_only: bool,
    ) -> Result<Vec<String>, EngineError> {
        let (binary, inherited) = match source_cmd {
            Some(source_cmd) => strip_identity(source_cmd)?,
            None => (CLAUDE_BIN.to_owned(), Vec::new()),
        };
        let mut command = vec![binary, "--resume".to_owned(), chat_id.to_owned()];
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
        let mut command = vec![
            binary,
            "--resume".to_owned(),
            chat_id.to_owned(),
            "--fork-session".to_owned(),
        ];
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

    /// claude → opus at medium effort; the seed appended unconditionally.
    fn distiller_command(&self, seed: &str) -> Vec<String> {
        let mut command = self.launch(&LaunchOptions {
            model: Some("opus"),
            effort: Some(Effort::Medium),
            ..LaunchOptions::default()
        });
        command.push(seed.to_owned());
        command
    }

    /// Copy Claude's cwd-keyed chat files so resume/fork can start in a new worktree.
    fn prepare_chat_for_cwd(
        &self,
        chat_id: &str,
        source_cwd: &str,
        target_cwd: &str,
    ) -> Result<(), EngineError> {
        if realpath(Path::new(source_cwd)) == realpath(Path::new(target_cwd)) {
            return Ok(());
        }
        let source_transcript = self.transcript_path(chat_id, source_cwd);
        if !source_transcript.is_file() {
            return Ok(());
        }
        let target_transcript = self.transcript_path(chat_id, target_cwd);
        if let Some(parent) = target_transcript.parent() {
            std::fs::create_dir_all(parent).map_err(EngineError::io(parent))?;
        }
        copy2(&source_transcript, &target_transcript)?;
        let source_sidecars = self.sidecar_dir(chat_id, source_cwd);
        if source_sidecars.is_dir() {
            copytree(&source_sidecars, &self.sidecar_dir(chat_id, target_cwd))?;
        }
        Ok(())
    }

    /// Claude argv is cwd-independent; the workspace half is the skill grant.
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
        Ok(command.to_owned())
    }

    fn is_read_only_command(&self, command: &str) -> bool {
        let Ok(tokens) = split_command(command) else {
            return false;
        };
        is_read_only_tokens(&tokens).unwrap_or(false)
    }

    fn resolve_transcript(&self, chat_id: &str, cwd: &str) -> PathBuf {
        self.transcript_path(chat_id, cwd)
    }

    /// The moved-cwd fallback: glob `projects/*/<chat>.jsonl`, preferring the `cwd_hint` munge
    /// when several match.
    fn relocate_transcript(&self, chat_id: &str, cwd_hint: Option<&str>) -> Option<PathBuf> {
        let file_name = format!("{chat_id}{TRANSCRIPT_SUFFIX}");
        let mut matches: Vec<PathBuf> = std::fs::read_dir(self.projects_root())
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path().join(&file_name))
            .filter(|path| std::fs::symlink_metadata(path).is_ok())
            .collect();
        matches.sort();
        if matches.len() > 1
            && let Some(cwd) = cwd_hint.filter(|cwd| !cwd.is_empty())
        {
            let preferred = self.transcript_path(chat_id, cwd);
            if matches.contains(&preferred) {
                return Some(preferred);
            }
        }
        matches.into_iter().next()
    }

    /// Claude's on-disk JSONL IS the engine-neutral form: a plain line-by-line decode.
    fn iter_messages(&self, transcript: &Path) -> Result<Vec<Value>, EngineError> {
        let text = std::fs::read_to_string(transcript).map_err(EngineError::io(transcript))?;
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_str(line).map_err(|source| EngineError::Json {
                    path: transcript.to_owned(),
                    source,
                })
            })
            .collect()
    }

    /// The sibling `<chat-id>/` dir, relative to the *resolved* transcript.
    fn bundle_sidecars(&self, src_transcript: &Path, chat_id: &str) -> Vec<PathBuf> {
        let parent = src_transcript.parent().unwrap_or(Path::new(""));
        vec![parent.join(chat_id)]
    }

    fn event_to_state(&self) -> &'static [(&'static str, State)] {
        EVENT_TO_STATE
    }

    fn state_source(&self) -> StateSource {
        StateSource::HookEvents
    }
}

/// The read-only shape check; `None` where the reference hits its `ValueError`/`IndexError`.
fn is_read_only_tokens(tokens: &[String]) -> Option<bool> {
    if tokens.iter().any(|token| token == SKIP_PERMISSIONS_FLAG) {
        return Some(false);
    }
    let value_after = |flag: &str| -> Option<usize> {
        let position = tokens.iter().position(|token| token == flag)?;
        Some(position + 1)
    };
    let permission_mode = tokens.get(value_after("--permission-mode")?)?;
    let tools_index = value_after("--disallowedTools")?;
    let allowed_index = value_after("--allowedTools")?;
    let setting_sources = tokens.get(value_after("--setting-sources")?)?;
    let list_from = |start: usize| -> Vec<&str> {
        tokens[start.min(tokens.len())..]
            .iter()
            .take_while(|token| !token.starts_with('-'))
            .flat_map(|token| token.split(','))
            .collect()
    };
    let denied = list_from(tools_index);
    let allowed = list_from(allowed_index);
    Some(
        permission_mode == "dontAsk"
            && READ_ONLY_TOOLS.iter().all(|tool| denied.contains(tool))
            && !denied.contains(&"Bash")
            && READ_ONLY_ALLOWED_TOOLS
                .iter()
                .all(|tool| allowed.contains(tool))
            && setting_sources == READ_ONLY_SETTING_SOURCES,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const YOLO: &str = SKIP_PERMISSIONS_FLAG;
    const RO_BLOCK: [&str; 10] = [
        "--allowedTools",
        "Bash",
        "--disallowedTools",
        "Edit",
        "Write",
        "NotebookEdit",
        "--permission-mode",
        "dontAsk",
        "--setting-sources",
        "user",
    ];

    fn engine() -> ClaudeEngine {
        ClaudeEngine::new(Home::new("/tx"), Some("/cfg"), Some(Path::new("/h")))
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn launch(options: LaunchOptions<'_>) -> Vec<String> {
        engine()
            .build_launch_command(&options, &mut Vec::new())
            .unwrap()
    }

    #[test]
    fn launch_command_defaults_and_options() {
        assert_eq!(
            launch(LaunchOptions::default()),
            argv(&["claude", "--effort", "high", "--no-chrome", YOLO])
        );
        assert_eq!(
            launch(LaunchOptions {
                model: Some("opus"),
                effort: Some(Effort::Medium),
                initial_prompt: Some("go"),
                browser: true,
                role_priming: Some("C\n\nP"),
                ..LaunchOptions::default()
            }),
            argv(&[
                "claude",
                "--model",
                "opus",
                "--effort",
                "medium",
                "--chrome",
                "--append-system-prompt",
                "C\n\nP",
                YOLO,
                "go"
            ])
        );
        let mut expected = argv(&["claude", "--effort", "high", "--no-chrome"]);
        expected.extend(argv(&RO_BLOCK));
        expected.push("go".into());
        assert_eq!(
            launch(LaunchOptions {
                read_only: true,
                initial_prompt: Some("go"),
                model: Some(""),
                ..LaunchOptions::default()
            }),
            expected
        );
    }

    #[test]
    fn distiller_is_opus_medium_with_the_seed_appended() {
        assert_eq!(
            engine().distiller_command(""),
            argv(&[
                "claude",
                "--model",
                "opus",
                "--effort",
                "medium",
                "--no-chrome",
                YOLO,
                ""
            ])
        );
    }

    fn fork(cmd: &str) -> Vec<String> {
        let command = engine().fork_command(cmd, "c", false).unwrap();
        assert_eq!(
            command[1..4],
            argv(&["--resume", "c", "--fork-session"]),
            "{cmd}"
        );
        assert_eq!(command.last().map(String::as_str), Some(YOLO), "{cmd}");
        command[4..command.len() - 1].to_vec()
    }

    #[test]
    fn strip_identity_grammar() {
        // Expected values from the Python reference (`_strip_identity` via fork_command).
        let cases: &[(&str, &[&str])] = &[
            ("claude --session-id abc --model opus", &["--model", "opus"]),
            (
                "claude --resume abc --fork-session --model opus",
                &["--model", "opus"],
            ),
            ("claude --resume --model opus", &["--model", "opus"]),
            (
                "claude -r abc -c --continue --tmux -w feat --worktree --from-pr 12",
                &[],
            ),
            ("", &[]),
            (
                "claude --unknown-flag val \"prompt\"",
                &["--unknown-flag", "val"],
            ),
            (
                "claude --unknown-flag --model opus",
                &["--unknown-flag", "--model", "opus"],
            ),
            (
                "claude --add-dir /a /b --model opus",
                &["--add-dir", "/a", "/b", "--model", "opus"],
            ),
            (
                "claude --allowedTools Bash Read --model opus",
                &["--model", "opus"],
            ),
            ("claude -p \"prompt\"", &["-p"]),
            ("claude --dangerously-skip-permissions \"P\"", &[]),
            (
                "claude --append-system-prompt \"multi word\" --model opus",
                &["--append-system-prompt", "multi word", "--model", "opus"],
            ),
            ("claude \"prompt\" --model opus", &["--model", "opus"]),
            (
                "claude --model opus 'seed' && echo hi",
                &["--model", "opus"],
            ),
            ("claude --model opus > out.log", &["--model", "opus"]),
            ("claude --unknown \">x\"", &["--unknown"]),
            ("claude --permission-mode dontAsk --x", &["--x"]),
            ("claude --setting-sources user", &[]),
            (
                "claude --setting-sources project",
                &["--setting-sources", "project"],
            ),
            (
                "claude --allowedTools Bash Read --disallowedTools Edit --model x",
                &["--model", "x"],
            ),
            ("claude --session-id", &[]),
        ];
        for (cmd, inherited) in cases {
            assert_eq!(fork(cmd), argv(inherited), "{cmd}");
        }
        let command = engine()
            .fork_command("/bin/claude --model x", "c", false)
            .unwrap();
        assert_eq!(command[0], "/bin/claude");
    }

    #[test]
    fn resume_and_seed_commands() {
        let src = "claude --model opus --effort high --no-chrome --append-system-prompt P --dangerously-skip-permissions \"seed\"";
        let persona = [
            "--model",
            "opus",
            "--effort",
            "high",
            "--no-chrome",
            "--append-system-prompt",
            "P",
        ];
        let e = engine();
        assert_eq!(
            e.resume_command("c0", None, false).unwrap(),
            argv(&["claude", "--resume", "c0", YOLO])
        );
        let mut expected = argv(&["claude", "--resume", "c1"]);
        expected.extend(argv(&persona));
        expected.push(YOLO.into());
        assert_eq!(e.resume_command("c1", Some(src), false).unwrap(), expected);

        let mut expected = argv(&["claude"]);
        expected.extend(argv(&persona));
        expected.extend(argv(&RO_BLOCK));
        expected.push("S".into());
        assert_eq!(e.seed_command(src, "S", true).unwrap(), expected);

        // A read-only source resumed read-only re-applies the block exactly once.
        let ro_src = crate::shlex::shlex_join(&expected);
        let resumed = e.resume_command("c1", Some(&ro_src), true).unwrap();
        assert_eq!(
            resumed.iter().filter(|t| *t == "--permission-mode").count(),
            1
        );
        assert!(matches!(
            e.seed_command("claude 'unclosed", "S", false),
            Err(EngineError::Shlex { .. })
        ));
    }

    #[test]
    fn read_only_command_detection() {
        let e = engine();
        let block = crate::shlex::shlex_join(&RO_BLOCK);
        let cases = [
            (format!("claude --effort high {block}"), true),
            (format!("claude {block} {YOLO}"), false),
            ("claude --allowedTools Bash --disallowedTools Edit,Write NotebookEdit --permission-mode dontAsk --setting-sources user".to_owned(), true),
            ("claude --allowedTools Bash --disallowedTools Edit Write NotebookEdit Bash --permission-mode dontAsk --setting-sources user".to_owned(), false),
            ("claude --allowedTools Read --disallowedTools Edit Write NotebookEdit --permission-mode dontAsk --setting-sources user".to_owned(), false),
            ("claude --allowedTools Bash --disallowedTools Edit Write NotebookEdit --permission-mode dontAsk --setting-sources project".to_owned(), false),
            ("claude --allowedTools Bash --disallowedTools Edit Write NotebookEdit --permission-mode default --setting-sources user".to_owned(), false),
            ("claude --allowedTools Bash --disallowedTools Edit Write NotebookEdit --setting-sources user --permission-mode".to_owned(), false),
            ("claude --model opus --setting-sources project --allowedTools Bash --disallowedTools Edit Write NotebookEdit --permission-mode dontAsk --setting-sources user".to_owned(), false),
            ("claude 'unclosed".to_owned(), false),
        ];
        for (command, expected) in cases {
            assert_eq!(e.is_read_only_command(&command), expected, "{command}");
        }
    }

    #[test]
    fn munge_paths_and_home() {
        assert_eq!(munge("/a/b.c/.d"), "-a-b-c--d");
        let e = engine();
        assert_eq!(
            e.transcript_path("c1", "/nope/x.y"),
            PathBuf::from("/cfg/projects/-nope-x-y/c1.jsonl")
        );
        assert_eq!(
            e.sidecar_dir("c1", "/nope"),
            PathBuf::from("/cfg/projects/-nope/c1")
        );
        let unset = ClaudeEngine::new(Home::new("/tx"), None, Some(Path::new("/h")));
        assert_eq!(unset.projects_root(), PathBuf::from("/h/.claude/projects"));
        assert_eq!(
            bundle_transcript_path(&Home::new("/tx"), "s", "c"),
            PathBuf::from("/tx/history/s/c/transcript.jsonl")
        );
        assert_eq!(
            e.bundle_sidecars(Path::new("/p/-x/c1.jsonl"), "c1"),
            [PathBuf::from("/p/-x/c1")]
        );
    }

    #[test]
    fn capture_and_event_table() {
        let e = engine();
        let payload = serde_json::json!({"session_id": "u1", "transcript_path": "/p/u1.jsonl"});
        assert_eq!(
            e.capture_session_id(&payload),
            Some(CapturedChat {
                session_id: "u1".into(),
                transcript_path: "/p/u1.jsonl".into()
            })
        );
        assert_eq!(
            e.capture_session_id(&serde_json::json!({"transcript_path": "/p"})),
            None
        );
        assert_eq!(e.state_for_event("SessionEnd"), Some(State::Idle));
        assert_eq!(e.state_for_event("StopFailure"), Some(State::Waiting));
        assert_eq!(e.state_for_event("Notification"), None);
        assert_eq!(e.state_source(), StateSource::HookEvents);
        assert!(e.matches_binary("/usr/bin/claude -p"));
        assert!(!e.matches_binary("codex"));
    }

    #[test]
    fn transcript_resolution_fast_path_then_projects_glob() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let cfg = base.join("cfg");
        let e = ClaudeEngine::new(Home::new(base.join("tx")), cfg.to_str(), None);
        let work = base.join("w");
        std::fs::create_dir(&work).unwrap();
        let work = work.to_str().unwrap();
        assert_eq!(e.locate_transcript("c1", Some(work)), None);

        let write = |dir: &str| {
            let path = cfg.join("projects").join(dir).join("c1.jsonl");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{}\n").unwrap();
            path
        };
        let other = write("-zz");
        assert_eq!(
            e.locate_transcript("c1", Some("/gone")),
            Some(other.clone())
        );
        assert_eq!(e.locate_transcript("c1", None), Some(other.clone()));
        let first = write("-aa");
        assert_eq!(e.locate_transcript("c1", Some("/gone")), Some(first));
        let fast = write(&munge(work));
        assert_eq!(e.locate_transcript("c1", Some(work)), Some(fast.clone()));
        assert_eq!(e.find_transcript("c1", work), Some(fast.clone()));
        assert_eq!(e.find_transcript("c2", work), None);
        // A dangling link misses the fast path but is still a glob hit (lexists): the cwd
        // munge is preferred over the first sorted match.
        std::fs::remove_file(&fast).unwrap();
        std::os::unix::fs::symlink(base.join("missing"), &fast).unwrap();
        assert_eq!(e.find_transcript("c1", work), None);
        assert_eq!(e.locate_transcript("c1", Some(work)), Some(fast));
    }

    #[test]
    fn prepare_chat_for_cwd_copies_transcript_and_merges_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let e = ClaudeEngine::new(Home::new(base.join("tx")), base.join("cfg").to_str(), None);
        let (src, dst) = (base.join("src"), base.join("dst"));
        std::fs::create_dir(&src).unwrap();
        let (src, dst) = (src.to_str().unwrap(), dst.to_str().unwrap());

        e.prepare_chat_for_cwd("c1", src, dst).unwrap();
        assert!(!e.project_dir(dst).exists());

        let transcript = e.transcript_path("c1", src);
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        std::fs::write(&transcript, "{\"l\":1}\n").unwrap();
        e.prepare_chat_for_cwd("c1", src, src).unwrap();
        e.prepare_chat_for_cwd("c1", src, dst).unwrap();
        assert_eq!(
            std::fs::read_to_string(e.transcript_path("c1", dst)).unwrap(),
            "{\"l\":1}\n"
        );
        assert!(!e.sidecar_dir("c1", dst).exists());

        let sidecar = e.sidecar_dir("c1", src).join("tool-results");
        std::fs::create_dir_all(&sidecar).unwrap();
        std::fs::write(sidecar.join("a.txt"), "A").unwrap();
        let target = e.sidecar_dir("c1", dst).join("tool-results");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("z.txt"), "Z").unwrap();
        e.prepare_chat_for_cwd("c1", src, dst).unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&target)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["a.txt", "z.txt"]);
        assert_eq!(
            std::fs::metadata(&transcript).unwrap().modified().unwrap(),
            std::fs::metadata(e.transcript_path("c1", dst))
                .unwrap()
                .modified()
                .unwrap()
        );
    }

    #[test]
    fn iter_messages_decodes_each_nonblank_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "{\"b\":1,\"a\":2}\n\n  [1]\n").unwrap();
        let messages = engine().iter_messages(&path).unwrap();
        assert_eq!(
            messages,
            [serde_json::json!({"b": 1, "a": 2}), serde_json::json!([1])]
        );
        std::fs::write(&path, "{\n").unwrap();
        assert!(matches!(
            engine().iter_messages(&path),
            Err(EngineError::Json { .. })
        ));
    }
}
