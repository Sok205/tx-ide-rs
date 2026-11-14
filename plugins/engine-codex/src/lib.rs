//! The Codex engine for tx-core: `codex [resume ID | fork ID] [-m <model>] -c model_reasoning_effort=<effort>
//! [-c developer_instructions=<profile text>] [-c hooks.…] --disable apps [-c mcp_servers.…]
//! [<tool switches>] --dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust
//! [prompt]`, with granted skills linked under `.agents/skills`. An agent gets exactly the MCP
//! servers its definition declares ([`mcp_overrides`]: ChatGPT apps and every server of the
//! operator's `config.toml` are switched off), and an agent definition's built-in `tools` become
//! the [`tool_switches`] that remove what it does not offer.
//! Without a `model` option Codex runs the user's configured default (`model` in its
//! `config.toml`): tx never overrides it.
//!
//! Engine data, under `ext.codex` of the record, is this adapter's alone to read:
//!
//! ```json
//! {"conversation_id": "01a19173-…", "transcript_path": "/Users/w/.codex/sessions/2026/10/31/rollout-…-01a19173-….jsonl",
//!  "model": "gpt-6.1-sol", "effort": "medium"}
//! ```
//!
//! `model`, `effort` and `command` are launch options: `configure` validates them
//! (`CodexOptions`, with this crate's own [`Effort`] vocabulary: what `model_reasoning_effort`
//! accepts) and merges them in before a launch reads them. `command` replaces the program
//! started, [`CODEX_BIN`] by default, and keeps every argument: a wrapper that extends the launch.
//!
//! A resume continues `conversation_id`; a fork branches the source's `conversation_id`; both are
//! refused when there is none yet. Codex picks its own id, so both keys come from the
//! `SessionStart` payload, which Codex fires once the first prompt is submitted. A Start or
//! Resume plan carries no engine data; a Fork plan returns the record's data without them, so the
//! fork never shows the source's conversation while it waits for its own. A fork runs in the
//! current directory (the fork's worktree) without asking: `-c tui.resume_cwd=current`. The data is rewritten only when the id differs; other keys of
//! the object survive. Paths Codex reports are resolved (`/private/tmp/…` for `/tmp/…`).
//!
//! `archive` keeps a copy of the rollout at `<archive>/<conversation_id>.jsonl` (Codex inlines
//! everything in the rollout, so there are no sidecars); `restore` puts it back at exactly the
//! recorded `transcript_path` when that file is gone. The rollout's name carries its creation
//! time, so a record without a `transcript_path` cannot be restored and says so.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tx_core::definitions::{AUTHORIZATION_HEADER, McpServer, bearer_variable, variable_name};
use tx_core::{
    Agent, Conversation, ENGINE_ARGS_OPTION, EngineError, EngineKind, IdleReason, Input, Key,
    LaunchPlan, LaunchRequest, LlmEngine, LlmEngineChange, LlmEngineHook, LlmEngineState, Profile,
    WorkspaceAction, engine_data, hook_shell_command, mirror_file, mirror_file_exact,
};

pub const CODEX_BIN: &str = "codex";
pub const SKILLS_DIR: &str = ".agents/skills";
pub const MODEL_FLAG: &str = "-m";
pub const RESUME_SUBCOMMAND: &str = "resume";
pub const FORK_SUBCOMMAND: &str = "fork";
/// Answers the "Working directory · fork" prompt `codex fork` shows when the current directory
/// differs from the session's: always the current one. What option 4 ("Always use current
/// directory") saves under `[tui]` in `config.toml` (codex-cli 0.164.0); passed per launch.
pub const FORK_CWD_OVERRIDE: &str = "tui.resume_cwd=current";
pub const REASONING_EFFORT_KEY: &str = "model_reasoning_effort";
pub const DEVELOPER_INSTRUCTIONS_KEY: &str = "developer_instructions";
pub const BYPASS_APPROVALS_FLAG: &str = "--dangerously-bypass-approvals-and-sandbox";
pub const BYPASS_HOOK_TRUST_FLAG: &str = "--dangerously-bypass-hook-trust";
pub const DISABLE_FLAG: &str = "--disable";
/// The feature behind ChatGPT's apps and connectors: off on every launch, so an agent gets only
/// the MCP servers it declares.
pub const APPS_FEATURE: &str = "apps";
/// The operator's own config, whose MCP servers every launch switches off.
pub const CONFIG_FILE: &str = "config.toml";
const MCP_TOOL_PREFIX: &str = "mcp__";
/// Codex's built-in tools have no allowlist; each Claude tool name an agent definition leaves out
/// (or cuts) is a switch that removes Codex's counterpart. A group is off only when none of its
/// names is offered.
pub const BUILT_IN_SWITCHES: [(&[&str], &[&str]); 6] = [
    (&["Bash"], &[DISABLE_FLAG, "shell_tool"]),
    (&["Read"], &[DISABLE_FLAG, "view_image"]),
    (
        &["WebSearch", "WebFetch"],
        &["-c", "web_search=\"disabled\""],
    ),
    // Code-mode models take their subagents from v2, the others from v1 (`multi_agent`).
    (
        &["Agent", "Task"],
        &[
            "-c",
            "features.multi_agent_v2.disable_native_tools=true",
            DISABLE_FLAG,
            "multi_agent",
        ],
    ),
    (
        &["AskUserQuestion"],
        &["-c", "tools.experimental_request_user_input.enabled=false"],
    ),
    (&["ScheduleWakeup"], &[DISABLE_FLAG, "sleep_tool"]),
];
/// What every launch carries that no switch removes (codex-cli 0.166.0): reported by
/// `tx check --engine codex`, never silently accepted.
pub const FORCED: [&str; 5] = [
    "the tools exec and wait, through which every other tool runs (the model's code mode)",
    "apply_patch (include_apply_patch_tool=false leaves it) and clock__curr_time",
    "the MCP resource tools (list and read resources), with or without MCP servers",
    "with the shared CODEX_HOME: the global AGENTS.md, the user's hooks.json, and the user and \
     bundled skills (a per-agent CODEX_HOME is pending)",
    "Codex's own base instructions: the profile goes in developer_instructions beside them, and \
     its multi-agent role text even when the subagent tools are off",
];

/// The Claude tool names that mean something on Codex; any other in `tools` has no counterpart.
const MAPPED_TOOLS: [&str; 12] = [
    "Bash",
    "Read",
    "Edit",
    "Write",
    "WebSearch",
    "WebFetch",
    "Agent",
    "Task",
    "AskUserQuestion",
    "ScheduleWakeup",
    "Skill",
    "ToolSearch",
];

/// What one agent definition gets on Codex that its file does not say: tools that cannot be
/// separated or removed, and listed names with no Codex counterpart.
pub fn definition_notes(agent: &tx_core::definitions::AgentDefinition) -> Vec<String> {
    let lists = |tool: &str| agent.tools.iter().any(|listed| listed == tool);
    let cuts = |tool: &str| agent.disallowed_tools.iter().any(|cut| cut == tool);
    let mut notes = Vec::new();
    if lists("Bash") && !(lists("Edit") && lists("Write")) {
        notes.push(
            "`Bash` is listed: files stay writable through the shell without Edit/Write".into(),
        );
    }
    if lists("Read") && !lists("Bash") {
        notes.push(
            "`Read` without `Bash`: codex reads only through the shell, so nothing reads".into(),
        );
    }
    if lists("WebSearch") != lists("WebFetch") {
        notes.push("web search and fetch are one switch on codex: listing one keeps both".into());
    }
    for tool in ["Read", "Edit", "Write"] {
        if cuts(tool) {
            notes.push(format!("`{tool}` cannot be cut on codex"));
        }
    }
    let unmapped: Vec<&str> = agent
        .tools
        .iter()
        .map(String::as_str)
        .filter(|tool| !MAPPED_TOOLS.contains(tool))
        .collect();
    if !unmapped.is_empty() {
        notes.push(format!("no codex counterpart: {}", unmapped.join(", ")));
    }
    notes
}

/// Codex-only tools no Claude name stands for: off for every agent definition.
pub const CODEX_ONLY_FEATURES: [&str; 3] = ["goals", "tool_suggest", "image_generation"];
/// The MCP server the interactive TUI adds on its own (codex-cli 0.166.0). `enabled=false` alone
/// is refused for a server no config layer defines, so it is given a dead URL first.
pub const TUI_WORKTREES_SERVER: &str = "codex_worktrees";
const DEAD_URL: &str = "http://127.0.0.1:9/none";
/// The slash command that exits the CLI cleanly, idle or mid-turn (codex-cli 0.164.0: exit 0
/// within half a second). Not Ctrl-C: idle it exits at once, but mid-turn it opens a menu.
pub const QUIT_COMMAND: &str = "/quit";
pub const DEFAULT_EFFORT: Effort = Effort::High;
/// The `ext` key this engine owns.
pub const EXT_KEY: &str = "codex";
pub const CONVERSATION_ID_KEY: &str = "conversation_id";
pub const TRANSCRIPT_PATH_KEY: &str = "transcript_path";
pub const MODEL_KEY: &str = "model";
pub const EFFORT_KEY: &str = "effort";
pub const COMMAND_KEY: &str = "command";

/// A reasoning-effort tier, as `model_reasoning_effort` accepts them. The CLI passes the string
/// through; the API refuses anything else (codex-cli 0.164.0: "Supported values are: 'none',
/// 'minimal', 'low', 'medium', 'high', 'xhigh', and 'max'"), so tx checks up front.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", try_from = "String")]
pub enum Effort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl Effort {
    pub const ALL: [Effort; 7] = [
        Effort::None,
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Effort::None => "none",
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "unknown effort {0:?} (codex expects 1-5 or none, minimal, low, medium, high, xhigh or max)"
)]
pub struct InvalidEffort(pub String);

impl FromStr for Effort {
    type Err = InvalidEffort;

    /// A word, or a level `1`–`5` on the tx scale (`tx_core::effort_word`).
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let word = tx_core::effort_word(text).unwrap_or(text);
        Self::ALL
            .into_iter()
            .find(|effort| effort.as_str() == word)
            .ok_or_else(|| InvalidEffort(text.to_owned()))
    }
}

impl TryFrom<String> for Effort {
    type Error = InvalidEffort;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

/// The launch options `configure` accepts. Every key is optional; an unknown key is refused.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// The program started in place of [`CODEX_BIN`], given tx's whole argument list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Appended after tx's own flags, before the prompt: `tx create … --engine-args ARGS`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}
/// The hooks wired per launch, one `-c hooks.<Event>=…` each; each runs
/// `<hook_command> <Event>`. Codex skips them without `--dangerously-bypass-hook-trust`.
pub const HOOK_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "Interrupt",
    "SessionEnd",
];

/// The tool through which the model asks the operator a question and waits for the answer.
pub const QUESTION_TOOL: &str = "request_user_input";

/// Codex's own directory: `CODEX_HOME`, else `$HOME/.codex`. Rollouts live under
/// `<codex home>/sessions/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexAdapter {
    pub codex_home: PathBuf,
}

impl CodexAdapter {
    /// The command line every launch shares after the `args` that name the conversation:
    /// model and effort from the data, the profile, the hooks, the bypass flags, and the
    /// prompt (none when empty); `stored_data` is what the plan stores.
    fn plan(
        &self,
        request: &LaunchRequest<'_>,
        mut args: Vec<String>,
        stored_data: Option<Value>,
        prompt: &str,
    ) -> Result<LaunchPlan, EngineError> {
        if let Some((name, provider)) = request.profile.provided_skills().first() {
            return Err(EngineError::Refused(format!(
                "codex: skill {name:?} is provided by {} and does not exist on codex",
                provider.as_str()
            )));
        }
        let data = engine_data(self, request.session);
        let option = |key: &str| data.get(key).and_then(Value::as_str);
        let effort = option(EFFORT_KEY).unwrap_or(DEFAULT_EFFORT.as_str());
        if let Some(model) = option(MODEL_KEY) {
            args.extend([MODEL_FLAG.to_owned(), model.to_owned()]);
        }
        args.extend(["-c".to_owned(), format!("{REASONING_EFFORT_KEY}={effort}")]);
        if !request.profile.text().is_empty() {
            args.extend([
                "-c".to_owned(),
                format!(
                    "{DEVELOPER_INSTRUCTIONS_KEY}={}",
                    toml_string(request.profile.text())
                ),
            ]);
        }
        if !request.hook_command.is_empty() {
            for event in HOOK_EVENTS {
                args.extend(["-c".to_owned(), hook_override(request.hook_command, event)]);
            }
        }
        args.extend([DISABLE_FLAG.to_owned(), APPS_FEATURE.to_owned()]);
        for override_ in mcp_overrides(request.profile, &self.configured_servers()?)? {
            args.extend(["-c".to_owned(), override_]);
        }
        args.extend(tool_switches(request.profile));
        args.extend([
            BYPASS_APPROVALS_FLAG.to_owned(),
            BYPASS_HOOK_TRUST_FLAG.to_owned(),
        ]);
        args.extend(
            data.get(ENGINE_ARGS_OPTION)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
        if !prompt.is_empty() {
            args.push(prompt.to_owned());
        }
        let mut plan = LaunchPlan::new(option(COMMAND_KEY).unwrap_or(CODEX_BIN)).args(args);
        plan.engine_data = stored_data;
        for link in skill_links(SKILLS_DIR, request.profile.skill_dirs()) {
            plan = plan.workspace(link);
        }
        Ok(plan)
    }

    /// The MCP server names in `<codex home>/config.toml`; none without the file.
    fn configured_servers(&self) -> Result<Vec<String>, EngineError> {
        let path = self.codex_home.join(CONFIG_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(EngineError::Io { path, source }),
        };
        let config: toml::Table = text
            .parse()
            .map_err(|error| EngineError::Refused(format!("{}: {error}", path.display())))?;
        Ok(match config.get("mcp_servers") {
            Some(toml::Value::Table(servers)) => servers.keys().cloned().collect(),
            _ => Vec::new(),
        })
    }

    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
        }
    }

    /// The conversation recorded for `session`; refused when there is none yet (Codex reports
    /// its id only once a first prompt was submitted).
    fn conversation_of<'a>(&self, session: &'a Agent, verb: &str) -> Result<&'a str, EngineError> {
        engine_data(self, session)
            .get(CONVERSATION_ID_KEY)
            .and_then(Value::as_str)
            .ok_or_else(|| {
                EngineError::Refused(format!(
                    "codex cannot {verb} {}: no conversation recorded yet (ext.{EXT_KEY}.{CONVERSATION_ID_KEY})",
                    session.name
                ))
            })
    }

    /// The conversation `SessionStart` recorded and the rollout path it came with. `None`
    /// before the first prompt; the path alone is `None` on a record that only ever held the id.
    fn recorded_conversation<'a>(&self, session: &'a Agent) -> Option<(&'a str, Option<&'a Path>)> {
        let data = engine_data(self, session);
        let conversation = data.get(CONVERSATION_ID_KEY)?.as_str()?;
        let transcript = data
            .get(TRANSCRIPT_PATH_KEY)
            .and_then(Value::as_str)
            .map(Path::new);
        Some((conversation, transcript))
    }
}

/// The archive's copy of a conversation: `<archive>/<conversation id>.jsonl`.
fn archived_rollout(archive_directory: &Path, conversation: &str) -> PathBuf {
    archive_directory.join(format!("{conversation}.jsonl"))
}

impl LlmEngine for CodexAdapter {
    fn kind(&self) -> EngineKind {
        EngineKind::Codex
    }

    fn ext_key(&self) -> &'static str {
        EXT_KEY
    }

    fn configure(&self, session: &Agent, options: &Value) -> Result<Value, EngineError> {
        if !options.is_object() {
            return Err(EngineError::Refused(format!(
                "codex options: expected a JSON object, got {options}"
            )));
        }
        let options: CodexOptions = serde_json::from_value(options.clone())
            .map_err(|error| EngineError::Refused(format!("codex options: {error}")))?;
        if options.model.as_deref() == Some("") {
            return Err(EngineError::Refused(
                "codex options: model must not be empty".into(),
            ));
        }
        if options.command.as_deref() == Some("") {
            return Err(EngineError::Refused(
                "codex options: command must not be empty".into(),
            ));
        }
        let mut data = engine_data(self, session)
            .as_object()
            .cloned()
            .unwrap_or_default();
        if let Some(model) = options.model {
            data.insert(MODEL_KEY.into(), model.into());
        }
        if let Some(effort) = options.effort {
            data.insert(EFFORT_KEY.into(), effort.as_str().into());
        }
        if let Some(command) = options.command {
            data.insert(COMMAND_KEY.into(), command.into());
        }
        if let Some(args) = options.args {
            data.insert(ENGINE_ARGS_OPTION.into(), args.into());
        }
        Ok(Value::Object(data))
    }

    fn start(&self, request: &LaunchRequest<'_>, prompt: &str) -> Result<LaunchPlan, EngineError> {
        self.plan(request, Vec::new(), None, prompt)
    }

    fn resume(&self, request: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        let args = vec![
            RESUME_SUBCOMMAND.to_owned(),
            self.conversation_of(request.session, "resume")?.to_owned(),
        ];
        self.plan(request, args, None, "")
    }

    /// The fork record's data starts as a copy of the source's, so the plan stores it minus
    /// the source's conversation: the fork's own arrives on `SessionStart`.
    fn fork(
        &self,
        request: &LaunchRequest<'_>,
        source: &Agent,
        prompt: &str,
    ) -> Result<LaunchPlan, EngineError> {
        let args = vec![
            FORK_SUBCOMMAND.to_owned(),
            self.conversation_of(source, "fork")?.to_owned(),
            "-c".to_owned(),
            FORK_CWD_OVERRIDE.to_owned(),
        ];
        let fork_data = without_conversation(engine_data(self, request.session));
        self.plan(request, args, Some(fork_data), prompt)
    }

    fn stop(&self, _: &Agent) -> Option<Input> {
        Some(Input::line(QUIT_COMMAND))
    }

    /// Escape, as the status line says ("esc to interrupt"); Ctrl-C mid-turn opens a menu. The
    /// agent is at its prompt afterwards.
    fn interrupt(&self, _: &Agent) -> Option<(Input, Option<LlmEngineState>)> {
        Some((Input::Key(Key::Escape), Some(LlmEngineState::Idle)))
    }

    /// A turn stops on the operator (`Idle`, waiting) at an approval (`PermissionRequest`,
    /// which tx's bypass flag keeps from firing) or at the model's question (`PreToolUse` of
    /// [`QUESTION_TOOL`]); the next `PostToolUse` means they answered. Unverified: no Codex was
    /// at hand, so both come from reading Codex 0.169's source, not from a captured payload.
    /// `SessionEnd` changes nothing: Codex also fires it after 30 min idle while the process
    /// lives, and its payload cannot tell the two apart; core ends the session when the process
    /// is gone.
    fn on_hook(&self, session: &Agent, hook: &LlmEngineHook) -> LlmEngineChange {
        let asks = hook.payload.get("tool_name").and_then(Value::as_str) == Some(QUESTION_TOOL);
        let change = match hook.name.as_str() {
            "SessionStart" | "Stop" | "Interrupt" => LlmEngineChange::idle(IdleReason::Finished),
            "PermissionRequest" => LlmEngineChange::idle(IdleReason::Waiting),
            "PreToolUse" if asks => LlmEngineChange::idle(IdleReason::Waiting),
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => {
                LlmEngineChange::to(LlmEngineState::Working)
            }
            _ => LlmEngineChange::NONE,
        };
        let engine_data = match hook.name.as_str() {
            "SessionStart" => conversation_started(engine_data(self, session), &hook.payload),
            _ => None,
        };
        LlmEngineChange {
            engine_data,
            ..change
        }
    }

    /// The rollout at the recorded `transcript_path`, keyed by conversation id so the
    /// conversations before a `/new` stay archived: by offset while the process lives, compared
    /// whole once the record is terminal (the process gone, the rollout final). No conversation
    /// yet, or a rollout not on disk, archives nothing.
    fn archive(&self, session: &Agent, archive_directory: &Path) -> Result<(), EngineError> {
        let Some((conversation, Some(transcript))) = self.recorded_conversation(session) else {
            return Ok(());
        };
        if !transcript.is_file() {
            return Ok(());
        }
        let archived = archived_rollout(archive_directory, conversation);
        if session.state == LlmEngineState::Ended {
            mirror_file_exact(transcript, &archived)
        } else {
            mirror_file(transcript, &archived)
        }
    }

    /// When the recorded rollout is gone and the archive has it: the whole file back at exactly
    /// the recorded `transcript_path`, its dated directories created. A live rollout is never
    /// overwritten; a conversation the archive does not hold restores nothing. A record whose
    /// archive has the rollout but that recorded no path is refused: the name carries the
    /// creation time, and a rebuilt one would not be the one Codex looks for.
    fn restore(&self, session: &Agent, archive_directory: &Path) -> Result<bool, EngineError> {
        let Some((conversation, transcript)) = self.recorded_conversation(session) else {
            return Ok(false);
        };
        let archived = archived_rollout(archive_directory, conversation);
        if !archived.is_file() {
            return Ok(false);
        }
        let Some(transcript) = transcript else {
            return Err(EngineError::Refused(format!(
                "codex cannot restore {}: the archive holds {} but the record has no \
                 ext.{EXT_KEY}.{TRANSCRIPT_PATH_KEY} to put it back at",
                session.name,
                archived.display()
            )));
        };
        if transcript.is_file() {
            return Ok(false);
        }
        mirror_file(&archived, transcript)?;
        Ok(true)
    }

    /// The recorded `conversation_id` and `transcript_path`; `None` until both are known.
    fn conversation(&self, session: &Agent) -> Option<Conversation> {
        let data = engine_data(self, session);
        Some(Conversation {
            id: data.get(CONVERSATION_ID_KEY)?.as_str()?.to_owned(),
            transcript_path: PathBuf::from(data.get(TRANSCRIPT_PATH_KEY)?.as_str()?),
        })
    }
}

/// The engine data after a `SessionStart`: `current` with the payload's `session_id` as
/// `conversation_id` and its `transcript_path`. `None` when the payload lacks either string or
/// both are already the current ones (a resume launched with only the id gains its path).
pub fn conversation_started(current: &Value, payload: &Value) -> Option<Value> {
    let conversation_id = payload.get("session_id")?.as_str()?;
    let transcript_path = payload.get(TRANSCRIPT_PATH_KEY)?.as_str()?;
    let recorded = |key: &str| current.get(key).and_then(Value::as_str);
    if recorded(CONVERSATION_ID_KEY) == Some(conversation_id)
        && recorded(TRANSCRIPT_PATH_KEY) == Some(transcript_path)
    {
        return None;
    }
    let mut data = current.as_object().cloned().unwrap_or_default();
    data.insert(CONVERSATION_ID_KEY.into(), conversation_id.into());
    data.insert(TRANSCRIPT_PATH_KEY.into(), transcript_path.into());
    Some(Value::Object(data))
}

/// `data` without `conversation_id` and `transcript_path`: a fork keeps its launch options and
/// waits for its own conversation on `SessionStart`.
pub fn without_conversation(data: &Value) -> Value {
    let mut data = data.as_object().cloned().unwrap_or_default();
    data.remove(CONVERSATION_ID_KEY);
    data.remove(TRANSCRIPT_PATH_KEY);
    Value::Object(data)
}

/// `hooks.<event>=[{matcher="",hooks=[{type="command",command="…"}]}]`, TOML inline. The
/// command is a TOML basic string; JSON string escapes are valid TOML escapes.
pub fn hook_override(hook_command: &[String], event: &str) -> String {
    let command = Value::from(hook_shell_command(hook_command, event)).to_string();
    format!("hooks.{event}=[{{matcher=\"\",hooks=[{{type=\"command\",command={command}}}]}}]")
}

/// A TOML string: a JSON string is a valid TOML basic string.
fn toml_string(text: &str) -> String {
    Value::from(text).to_string()
}

fn toml_strings(items: &[String]) -> String {
    Value::from(items.to_vec()).to_string()
}

fn toml_table(entries: &BTreeMap<String, String>) -> String {
    let fields: Vec<String> = entries
        .iter()
        .map(|(key, value)| format!("{} = {}", toml_string(key), toml_string(value)))
        .collect();
    format!("{{{}}}", fields.join(", "))
}

/// The `-c` overrides that define one MCP server, `mcp_servers.<name>.*` (see [`mcp_overrides`]
/// for how secrets pass).
pub fn server_overrides(name: &str, server: &McpServer) -> Result<Vec<String>, EngineError> {
    let mut overrides = Vec::new();
    let key = |field: &str| format!("mcp_servers.{name}.{field}");
    match server {
        McpServer::Stdio { command, args, env } => {
            overrides.push(format!("{}={}", key("command"), toml_string(command)));
            if !args.is_empty() {
                overrides.push(format!("{}={}", key("args"), toml_strings(args)));
            }
            let mut literal = BTreeMap::new();
            let mut passed = Vec::new();
            for (variable, value) in env {
                match variable_name(value) {
                    Some(source) if source == variable => passed.push(variable.clone()),
                    Some(source) => {
                        return Err(EngineError::Refused(format!(
                            "codex: MCP server {name}: env {variable}=${{{source}}}: codex passes a variable only under its own name"
                        )));
                    }
                    None => {
                        literal.insert(variable.clone(), value.clone());
                    }
                }
            }
            if !literal.is_empty() {
                overrides.push(format!("{}={}", key("env"), toml_table(&literal)));
            }
            if !passed.is_empty() {
                overrides.push(format!("{}={}", key("env_vars"), toml_strings(&passed)));
            }
        }
        McpServer::Http { url, headers } => {
            overrides.push(format!("{}={}", key("url"), toml_string(url)));
            let mut literal = BTreeMap::new();
            let mut from_environment = BTreeMap::new();
            for (header, value) in headers {
                if header.eq_ignore_ascii_case(AUTHORIZATION_HEADER)
                    && let Some(variable) = bearer_variable(value)
                {
                    overrides.push(format!(
                        "{}={}",
                        key("bearer_token_env_var"),
                        toml_string(variable)
                    ));
                } else if let Some(variable) = variable_name(value) {
                    from_environment.insert(header.clone(), variable.to_owned());
                } else {
                    literal.insert(header.clone(), value.clone());
                }
            }
            if !literal.is_empty() {
                overrides.push(format!("{}={}", key("http_headers"), toml_table(&literal)));
            }
            if !from_environment.is_empty() {
                overrides.push(format!(
                    "{}={}",
                    key("env_http_headers"),
                    toml_table(&from_environment)
                ));
            }
        }
    }
    Ok(overrides)
}

/// The `-c` overrides that give the agent exactly its MCP servers: each declared server as
/// `mcp_servers.<name>.*`, minus what `disallowedTools` cuts (`mcp__<server>` drops the server,
/// `mcp__<server>__<tool>` one tool), and `enabled=false` for every server of the operator's
/// `config.toml` it does not declare, and the TUI's own [`TUI_WORKTREES_SERVER`]. A secret (`${NAME}`) is never expanded: an `env` entry
/// passes the variable of the same name through (`env_vars`); a header passes it as
/// `env_http_headers`, or `bearer_token_env_var` for `Authorization: Bearer ${NAME}`.
pub fn mcp_overrides(profile: &Profile, configured: &[String]) -> Result<Vec<String>, EngineError> {
    let mut cut_tools: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    let mut cut_servers: Vec<&str> = Vec::new();
    for tool in profile.disallowed_tools() {
        let Some(rest) = tool.strip_prefix(MCP_TOOL_PREFIX) else {
            continue;
        };
        match rest.split_once("__") {
            Some((server, name)) if !name.is_empty() && name != "*" => {
                cut_tools.entry(server).or_default().push(name.to_owned())
            }
            Some((server, _)) => cut_servers.push(server),
            None => cut_servers.push(rest),
        }
    }
    let mut overrides = Vec::new();
    for (name, server) in profile.mcp_servers() {
        if cut_servers.contains(&name.as_str()) {
            continue;
        }
        overrides.extend(server_overrides(name, server)?);
        if let Some(tools) = cut_tools.get(name.as_str()) {
            overrides.push(format!(
                "mcp_servers.{name}.disabled_tools={}",
                toml_strings(tools)
            ));
        }
    }
    for name in configured {
        if !profile.mcp_servers().contains_key(name) || cut_servers.contains(&name.as_str()) {
            overrides.push(format!("mcp_servers.{name}.enabled=false"));
        }
    }
    if !configured.iter().any(|name| name == TUI_WORKTREES_SERVER) {
        overrides.push(format!(
            "mcp_servers.{TUI_WORKTREES_SERVER}.url={}",
            toml_string(DEAD_URL)
        ));
        overrides.push(format!("mcp_servers.{TUI_WORKTREES_SERVER}.enabled=false"));
    }
    Ok(overrides)
}

/// The switches that take away the built-in tools an agent definition does not offer: every
/// [`BUILT_IN_SWITCHES`] group none of whose names is listed (and not cut), and the
/// [`CODEX_ONLY_FEATURES`]. Without a definition nothing is switched.
pub fn tool_switches(profile: &Profile) -> Vec<String> {
    let Some(tools) = profile.tools() else {
        return Vec::new();
    };
    let offered = |name: &str| {
        tools.iter().any(|tool| tool == name)
            && !profile.disallowed_tools().iter().any(|tool| tool == name)
    };
    let mut switches = Vec::new();
    for (names, switch) in BUILT_IN_SWITCHES {
        if !names.iter().any(|name| offered(name)) {
            switches.extend(switch.iter().map(|word| (*word).to_owned()));
        }
    }
    for feature in CODEX_ONLY_FEATURES {
        switches.extend([DISABLE_FLAG.to_owned(), feature.to_owned()]);
    }
    switches
}

/// One symlink per granted skill dir: `<engine dir>/<skill name>` → the skill dir.
pub fn skill_links(engine_dir: &str, skill_dirs: &[PathBuf]) -> Vec<WorkspaceAction> {
    skill_dirs
        .iter()
        .filter_map(|dir| {
            let name = dir.file_name()?;
            Some(WorkspaceAction::Symlink {
                src: dir.clone(),
                dst: PathBuf::from(engine_dir).join(name),
            })
        })
        .collect()
}

/// The plugin wrapper: registers [`CodexAdapter`] under the plugin id `engine-codex`, with the
/// Codex home resolved once from the environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct CodexPlugin;

/// `CODEX_HOME`, else `$HOME/.codex`.
pub fn codex_home_from_env() -> PathBuf {
    match std::env::var_os("CODEX_HOME") {
        Some(directory) if !directory.is_empty() => PathBuf::from(directory),
        _ => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".codex"),
    }
}

impl tx_core::Plugin for CodexPlugin {
    fn id(&self) -> &'static str {
        "engine-codex"
    }

    fn register(&self, r: &mut tx_core::Registry) -> Result<(), tx_core::PluginError> {
        r.engine(Box::new(CodexAdapter::new(codex_home_from_env())));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tx_core::{Profile, ResolvedProfile};

    fn profile(text: &str, skills: &[&str]) -> (tempfile::TempDir, Profile) {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::write(
            ResolvedProfile {
                text: text.into(),
                skill_dirs: skills.iter().map(PathBuf::from).collect(),
                ..ResolvedProfile::default()
            },
            &dir.path().join("p.md"),
        )
        .unwrap();
        (dir, profile)
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    fn hook(name: &str, payload: serde_json::Value) -> LlmEngineHook {
        LlmEngineHook {
            name: name.into(),
            payload,
            at: 2.0,
        }
    }

    /// The adapter under test, with a fixed Codex home.
    fn adapter() -> CodexAdapter {
        CodexAdapter::new("/h")
    }

    /// A starting agent record with this engine's data `data` (absent when `Null`).
    fn record(data: serde_json::Value) -> Agent {
        session(LlmEngineState::Starting, data)
    }

    /// An agent record in `state` whose `ext.CodexAdapter` is `data` (absent when `Null`).
    fn session(state: LlmEngineState, data: serde_json::Value) -> Agent {
        use tx_core::AgentId;
        let mut ext = std::collections::BTreeMap::new();
        if !data.is_null() {
            ext.insert(EXT_KEY.to_owned(), data);
        }
        Agent {
            id: AgentId::mint(),
            name: "agent".into(),
            engine: adapter().kind(),
            working_directory: "/tmp".into(),
            roles: Vec::new(),
            definition: None,
            state,
            idle_reason: None,
            created_at: 1.0,
            last_modified_at: 1.0,
            last_activity_at: None,
            tags: Vec::new(),
            parent_id: None,
            ext,
            other: serde_json::Map::new(),
        }
    }

    /// (current state, current engine data, hook name, payload) → the change.
    type Row = (
        LlmEngineState,
        serde_json::Value,
        &'static str,
        serde_json::Value,
        LlmEngineChange,
    );

    fn check(table: Vec<Row>) {
        for (current, data, name, payload, expected) in table {
            assert_eq!(
                adapter().on_hook(
                    &session(current, data.clone()),
                    &hook(name, payload.clone())
                ),
                expected,
                "{current:?} {data} {name} {payload}"
            );
        }
    }

    fn to(state: LlmEngineState) -> LlmEngineChange {
        LlmEngineChange::to(state)
    }

    fn finished() -> LlmEngineChange {
        LlmEngineChange::idle(IdleReason::Finished)
    }

    fn waiting() -> LlmEngineChange {
        LlmEngineChange::idle(IdleReason::Waiting)
    }

    #[test]
    fn hooks_to_states() {
        use LlmEngineState::*;
        use serde_json::json;
        let none = json!({});
        let null = serde_json::Value::Null;
        check(vec![
            (
                Idle,
                null.clone(),
                "UserPromptSubmit",
                none.clone(),
                to(Working),
            ),
            (
                Working,
                null.clone(),
                "PermissionRequest",
                json!({"tool_name": "shell"}),
                waiting(),
            ),
            // The operator answered: a tool ran.
            (Idle, null.clone(), "PostToolUse", none.clone(), to(Working)),
            (
                Working,
                null.clone(),
                "PostToolUse",
                none.clone(),
                to(Working),
            ),
            (Working, null.clone(), "Stop", none.clone(), finished()),
            (Working, null.clone(), "Interrupt", none.clone(), finished()),
            (
                Idle,
                null.clone(),
                "SessionEnd",
                json!({"reason": "other"}),
                LlmEngineChange::NONE,
            ),
            (
                Working,
                null.clone(),
                "PreToolUse",
                json!({"tool_name": "request_user_input", "tool_input": {"questions": []}}),
                waiting(),
            ),
            (
                Idle,
                null.clone(),
                "PreToolUse",
                json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
                to(Working),
            ),
            (
                Idle,
                null.clone(),
                "Notification",
                none.clone(),
                LlmEngineChange::NONE,
            ),
        ]);
    }

    #[test]
    fn session_start_records_the_conversation() {
        use LlmEngineState::*;
        use serde_json::json;
        let null = serde_json::Value::Null;
        let path = "/private/tmp/h/sessions/2026/10/31/rollout-2026-10-31T10-00-00-id-1.jsonl";
        let first = json!({"conversation_id": "id-1", "transcript_path": path});
        let started = |id: &str| {
            json!({
                "session_id": id,
                "transcript_path": path.replace("id-1", id),
                "cwd": "/private/tmp/w",
                "hook_event_name": "SessionStart",
                "source": "startup",
                "model": "gpt-6.1-sol",
            })
        };
        check(vec![
            // Codex picks its id: the first start after the first prompt writes it.
            (
                Starting,
                null.clone(),
                "SessionStart",
                started("id-1"),
                finished().engine_data(first.clone()),
            ),
            // The same id again: state only.
            (
                Starting,
                first.clone(),
                "SessionStart",
                started("id-1"),
                finished(),
            ),
            // A resume launched with only the id gains the transcript path.
            (
                Starting,
                json!({"conversation_id": "id-1"}),
                "SessionStart",
                started("id-1"),
                finished().engine_data(first.clone()),
            ),
            // A payload without both strings changes no data.
            (
                Starting,
                null.clone(),
                "SessionStart",
                json!({}),
                finished(),
            ),
            (
                Starting,
                null.clone(),
                "SessionStart",
                json!({"session_id": "id-1"}),
                finished(),
            ),
            (
                Starting,
                null.clone(),
                "SessionStart",
                json!({"session_id": "id-1", "transcript_path": null}),
                finished(),
            ),
            // Hand-edited non-object data counts as absent.
            (
                Idle,
                json!(["junk"]),
                "SessionStart",
                started("id-1"),
                finished().engine_data(first.clone()),
            ),
            // Other keys of the object survive a new conversation.
            (
                Idle,
                json!({"conversation_id": "id-1", "transcript_path": "/old", "effort": "high"}),
                "SessionStart",
                started("id-2"),
                finished().engine_data(json!({
                    "conversation_id": "id-2",
                    "transcript_path": path.replace("id-1", "id-2"),
                    "effort": "high",
                })),
            ),
            // Only SessionStart carries data.
            (Working, null.clone(), "Stop", started("id-3"), finished()),
        ]);
    }

    #[test]
    fn ext_key_is_codex() {
        assert_eq!(adapter().ext_key(), "codex");
        assert_eq!(adapter().ext_key(), EXT_KEY);
    }

    #[test]
    fn id_is_codex() {
        assert_eq!(adapter().kind(), EngineKind::Codex);
    }

    #[test]
    fn a_command_replaces_the_program_and_keeps_every_argument() {
        let (_dir, profile) = profile("", &[]);
        let plain = adapter()
            .start(
                &LaunchRequest::new(&record(serde_json::json!({})), &profile),
                "go",
            )
            .unwrap();
        let session = record(serde_json::json!({"command": "/bin/codex-wrapped"}));
        let wrapped = adapter()
            .start(&LaunchRequest::new(&session, &profile), "go")
            .unwrap();
        assert_eq!(wrapped.program, "/bin/codex-wrapped");
        assert_eq!(wrapped.args, plain.args);
    }

    #[test]
    fn args_come_after_every_flag_of_tx_and_before_the_prompt() {
        let (_dir, profile) = profile("", &[]);
        let plain = adapter()
            .start(
                &LaunchRequest::new(&record(serde_json::json!({})), &profile),
                "go",
            )
            .unwrap();
        let session = record(serde_json::json!({"args": ["--search", "-c", "k=v"]}));
        let extended = adapter()
            .start(&LaunchRequest::new(&session, &profile), "go")
            .unwrap();
        let tail = plain.args.len() - 1;
        assert_eq!(extended.args[..tail], plain.args[..tail]);
        assert_eq!(
            extended.args[tail..],
            argv(&["--search", "-c", "k=v", "go"])
        );
    }

    #[test]
    fn defaults() {
        let (_dir, profile) = profile("", &[]);
        let session = record(serde_json::Value::Null);
        let plan = adapter()
            .start(&LaunchRequest::new(&session, &profile), "")
            .unwrap();
        assert_eq!(plan.program, "codex");
        // No `-m`: Codex runs the model its own configuration names.
        assert_eq!(
            plan.args,
            argv(&[
                "-c",
                "model_reasoning_effort=high",
                "--disable",
                "apps",
                "-c",
                "mcp_servers.codex_worktrees.url=\"http://127.0.0.1:9/none\"",
                "-c",
                "mcp_servers.codex_worktrees.enabled=false",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
            ])
        );
        assert!(plan.workspace.is_empty());
    }

    #[test]
    fn an_agent_definition_gets_exactly_its_servers_and_tools() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "model = \"m\"\n[mcp_servers.other]\ncommand = \"x\"\n[mcp_servers.probe]\ncommand = \"old\"\n",
        )
        .unwrap();
        let servers = BTreeMap::from([
            (
                "probe".to_owned(),
                McpServer::Stdio {
                    command: "python3".into(),
                    args: vec!["probe.py".into()],
                    env: BTreeMap::from([
                        ("MODE".to_owned(), "test".to_owned()),
                        ("TOKEN".to_owned(), "${TOKEN}".to_owned()),
                    ]),
                },
            ),
            (
                "web".to_owned(),
                McpServer::Http {
                    url: "https://x/mcp".into(),
                    headers: BTreeMap::from([
                        ("Authorization".to_owned(), "Bearer ${WEB_TOKEN}".to_owned()),
                        ("X-Key".to_owned(), "${KEY}".to_owned()),
                        ("X-Team".to_owned(), "tx".to_owned()),
                    ]),
                },
            ),
        ]);
        let profile = Profile::write(
            ResolvedProfile {
                tools: Some(vec!["Bash".into(), "WebFetch".into()]),
                disallowed_tools: vec!["mcp__probe__delete_thing".into()],
                mcp_servers: servers,
                ..ResolvedProfile::default()
            },
            &dir.path().join("p.md"),
        )
        .unwrap();
        let session = record(serde_json::Value::Null);
        let args = CodexAdapter::new(dir.path())
            .start(&LaunchRequest::new(&session, &profile), "go")
            .unwrap()
            .args;
        let overrides: Vec<&str> = args
            .windows(2)
            .filter(|pair| pair[0] == "-c" && pair[1].starts_with("mcp_servers."))
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(
            overrides,
            [
                r#"mcp_servers.probe.command="python3""#,
                r#"mcp_servers.probe.args=["probe.py"]"#,
                r#"mcp_servers.probe.env={"MODE" = "test"}"#,
                r#"mcp_servers.probe.env_vars=["TOKEN"]"#,
                r#"mcp_servers.probe.disabled_tools=["delete_thing"]"#,
                r#"mcp_servers.web.url="https://x/mcp""#,
                r#"mcp_servers.web.bearer_token_env_var="WEB_TOKEN""#,
                r#"mcp_servers.web.http_headers={"X-Team" = "tx"}"#,
                r#"mcp_servers.web.env_http_headers={"X-Key" = "KEY"}"#,
                "mcp_servers.other.enabled=false",
                r#"mcp_servers.codex_worktrees.url="http://127.0.0.1:9/none""#,
                "mcp_servers.codex_worktrees.enabled=false",
            ]
        );
        let joined = args.join(" ");
        assert!(joined.contains("--disable apps"));
        assert!(!joined.contains("shell_tool"), "Bash is listed");
        assert!(!joined.contains("web_search"), "WebFetch is listed");
        for switch in [
            "-c features.multi_agent_v2.disable_native_tools=true",
            "-c tools.experimental_request_user_input.enabled=false",
            "--disable sleep_tool",
            "--disable goals",
            "--disable tool_suggest",
            "--disable image_generation",
            "--disable view_image",
        ] {
            assert!(joined.contains(switch), "{switch}");
        }
    }

    #[test]
    fn every_option() {
        let (_dir, profile) = profile("be good", &["/skills/one"]);
        let session = record(serde_json::json!({"model": "o3", "effort": "low"}));
        let plan = adapter()
            .start(&LaunchRequest::new(&session, &profile), "go")
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "-m",
                "o3",
                "-c",
                "model_reasoning_effort=low",
                "-c",
                "developer_instructions=\"be good\"",
                "--disable",
                "apps",
                "-c",
                "mcp_servers.codex_worktrees.url=\"http://127.0.0.1:9/none\"",
                "-c",
                "mcp_servers.codex_worktrees.enabled=false",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
                "go",
            ])
        );
        assert_eq!(
            plan.workspace,
            vec![WorkspaceAction::Symlink {
                src: "/skills/one".into(),
                dst: ".agents/skills/one".into()
            }]
        );
    }

    #[test]
    fn start_carries_no_engine_data() {
        let (_dir, profile) = profile("", &[]);
        let session = record(serde_json::Value::Null);
        let plan = adapter()
            .start(&LaunchRequest::new(&session, &profile), "hi")
            .unwrap();
        assert_eq!(plan.args[0], "-c");
        assert_eq!(plan.args.last().map(String::as_str), Some("hi"));
        assert_eq!(plan.engine_data, None, "Codex reports its id by hook");
    }

    #[test]
    fn resume_is_the_subcommand_on_the_recorded_conversation() {
        let (_dir, profile) = profile("", &[]);
        let session = record(serde_json::json!({"conversation_id": "sess-7", "model": "o3"}));
        let plan = adapter()
            .resume(&LaunchRequest::new(&session, &profile))
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "resume",
                "sess-7",
                "-m",
                "o3",
                "-c",
                "model_reasoning_effort=high",
                "--disable",
                "apps",
                "-c",
                "mcp_servers.codex_worktrees.url=\"http://127.0.0.1:9/none\"",
                "-c",
                "mcp_servers.codex_worktrees.enabled=false",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
            ])
        );
        assert_eq!(plan.engine_data, None);
    }

    #[test]
    fn fork_is_the_subcommand_on_the_source_conversation_in_the_current_directory() {
        let (_dir, profile) = profile("", &[]);
        let source = record(serde_json::json!({"conversation_id": "sess-7"}));
        let session = record(serde_json::Value::Null);
        let plan = adapter()
            .fork(&LaunchRequest::new(&session, &profile), &source, "go")
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "fork",
                "sess-7",
                "-c",
                "tui.resume_cwd=current",
                "-c",
                "model_reasoning_effort=high",
                "--disable",
                "apps",
                "-c",
                "mcp_servers.codex_worktrees.url=\"http://127.0.0.1:9/none\"",
                "-c",
                "mcp_servers.codex_worktrees.enabled=false",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
                "go",
            ])
        );
        assert_eq!(plan.engine_data, Some(serde_json::json!({})));
    }

    #[test]
    fn a_fork_drops_the_source_conversation_and_keeps_its_options() {
        use serde_json::json;
        let (_dir, profile) = profile("", &[]);
        let source_data = json!({
            "conversation_id": "sess-7",
            "transcript_path": "/h/sessions/rollout-sess-7.jsonl",
            "model": "gpt-6.1-sol",
            "effort": "low",
        });
        let source = record(source_data.clone());
        // Core starts the fork's record as a copy of the source's data; `configure` merged the
        // fork's own options into it.
        let mut fork_data = source_data;
        fork_data["effort"] = json!("medium");
        let session = record(fork_data);
        let plan = adapter()
            .fork(&LaunchRequest::new(&session, &profile), &source, "go")
            .unwrap();
        assert_eq!(
            plan.args[..4],
            argv(&["fork", "sess-7", "-c", "tui.resume_cwd=current"])
        );
        assert_eq!(
            plan.engine_data,
            Some(json!({"model": "gpt-6.1-sol", "effort": "medium"}))
        );
    }

    #[test]
    fn stop_is_quit_submitted_and_interrupt_is_escape() {
        let session = record(serde_json::Value::Null);
        assert_eq!(adapter().stop(&session), Some(Input::line("/quit")));
        assert_eq!(
            adapter().interrupt(&session),
            Some((Input::Key(Key::Escape), Some(LlmEngineState::Idle)))
        );
        assert_eq!(
            adapter().receive_message(&tx_core::EnvelopeSender::User { via: Some("notes") }, "hi"),
            Input::line("<from-user session=\"notes\">hi</from-user>")
        );
    }

    #[test]
    fn only_a_fork_overrides_the_working_directory_prompt() {
        let (_dir, profile) = profile("", &[]);
        let session = record(serde_json::json!({"conversation_id": "sess-7"}));
        let request = LaunchRequest::new(&session, &profile);
        for (name, plan) in [
            ("start", adapter().start(&request, "go").unwrap()),
            ("resume", adapter().resume(&request).unwrap()),
        ] {
            assert!(!plan.args.contains(&FORK_CWD_OVERRIDE.to_owned()), "{name}");
        }
    }

    #[test]
    fn resume_and_fork_without_a_conversation_are_refused() {
        let (_dir, profile) = profile("", &[]);
        for data in [
            serde_json::Value::Null,
            serde_json::json!({"model": "o3"}),
            serde_json::json!("junk"),
        ] {
            let session = record(data);
            let error = adapter()
                .resume(&LaunchRequest::new(&session, &profile))
                .unwrap_err();
            assert!(
                matches!(&error, EngineError::Refused(text) if text.contains("cannot resume agent")),
                "{error:?}"
            );
            let target = record(serde_json::Value::Null);
            let error = adapter()
                .fork(&LaunchRequest::new(&target, &profile), &session, "go")
                .unwrap_err();
            assert!(
                matches!(&error, EngineError::Refused(text) if text.contains("cannot fork agent")),
                "{error:?}"
            );
        }
    }

    #[test]
    fn an_empty_prompt_is_absent() {
        let (_dir, profile) = profile("", &[]);
        let session = record(serde_json::Value::Null);
        let plan = adapter()
            .start(&LaunchRequest::new(&session, &profile), "")
            .unwrap();
        assert_eq!(plan.args.len(), 10);
        assert!(!plan.args.contains(&"-m".to_owned()));
    }

    #[test]
    fn configure_merges_valid_options_into_the_data() {
        use serde_json::json;
        let session = record(json!({"conversation_id": "c", "effort": "high"}));
        assert_eq!(
            adapter()
                .configure(
                    &session,
                    &json!({"model": "gpt-6.1-sol", "effort": "medium", "command": "codex-wrapped",
                            "args": ["--search"]})
                )
                .unwrap(),
            json!({"conversation_id": "c", "model": "gpt-6.1-sol", "effort": "medium",
                   "command": "codex-wrapped", "args": ["--search"]})
        );
        assert_eq!(
            adapter().configure(&session, &json!({})).unwrap(),
            json!({"conversation_id": "c", "effort": "high"})
        );
        let empty = record(serde_json::Value::Null);
        assert_eq!(adapter().configure(&empty, &json!({})).unwrap(), json!({}));
        assert_eq!(
            adapter()
                .configure(&empty, &json!({"effort": "none"}))
                .unwrap(),
            json!({"effort": "none"})
        );
    }

    #[test]
    fn configure_refuses_unknown_keys_and_bad_values() {
        use serde_json::json;
        let session = record(serde_json::Value::Null);
        for (options, needle) in [
            (json!({"effort": "extreme"}), "unknown effort \"extreme\""),
            (json!({"effort": "6"}), "expects 1-5 or none"),
            (json!({"effort": true}), "invalid type: boolean `true`"),
            (json!({"model": ""}), "model must not be empty"),
            (json!({"command": ""}), "command must not be empty"),
            (
                json!({"args": [1]}),
                "invalid type: integer `1`, expected a string",
            ),
            (json!({"model": ["a"]}), "invalid type: sequence"),
            (json!({"sandbox": "none"}), "unknown field `sandbox`"),
            (json!([]), "expected a JSON object, got []"),
            (json!(null), "expected a JSON object, got null"),
        ] {
            let error = adapter().configure(&session, &options).unwrap_err();
            assert!(
                matches!(&error, EngineError::Refused(text) if text.contains(needle)),
                "{options}: {error:?}"
            );
        }
    }

    #[test]
    fn effort_names() {
        for effort in Effort::ALL {
            assert_eq!(effort.as_str().parse::<Effort>().unwrap(), effort);
            assert_eq!(serde_json::to_value(effort).unwrap(), effort.as_str());
        }
        assert_eq!(Effort::None.as_str(), "none");
        assert_eq!(Effort::XHigh.as_str(), "xhigh");
        assert!("ultra".parse::<Effort>().is_err());
    }

    /// `1`–`5` are the tx scale; `none` and `minimal` have no level.
    #[test]
    fn levels_one_to_five_are_the_scale_words() {
        let levels = [
            ("1", Effort::Low),
            ("2", Effort::Medium),
            ("3", Effort::High),
            ("4", Effort::XHigh),
            ("5", Effort::Max),
        ];
        for (level, effort) in levels {
            assert_eq!(level.parse::<Effort>().unwrap(), effort);
            assert_eq!(
                serde_json::from_value::<Effort>(level.into()).unwrap(),
                effort
            );
        }
        for text in ["0", "6", "x", "-1", "03"] {
            let error = text.parse::<Effort>().unwrap_err().to_string();
            assert!(error.contains("expects 1-5 or none, minimal"), "{error}");
        }
    }

    #[test]
    fn configure_stores_a_level_as_its_word() {
        use serde_json::json;
        let configured = adapter()
            .configure(&record(serde_json::Value::Null), &json!({"effort": "4"}))
            .unwrap();
        assert_eq!(configured, json!({"effort": "xhigh"}));
    }

    #[test]
    fn hook_command_wires_every_event_as_a_config_override() {
        let (_dir, profile) = profile("", &[]);
        let hook_command = ["/x\"y/tx".to_owned(), "chat".to_owned(), "hook".to_owned()];
        let session = record(serde_json::Value::Null);
        let plan = adapter()
            .start(
                &LaunchRequest::new(&session, &profile).hook_command(&hook_command),
                "",
            )
            .unwrap();
        let overrides: Vec<&str> = plan
            .args
            .windows(2)
            .filter(|pair| pair[0] == "-c" && pair[1].starts_with("hooks."))
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(overrides.len(), HOOK_EVENTS.len());
        assert_eq!(
            overrides[5],
            r#"hooks.Stop=[{matcher="",hooks=[{type="command",command="'/x\"y/tx' 'chat' 'hook' 'Stop' || exit 1"}]}]"#
        );
        assert_eq!(
            &plan.args[plan.args.len() - 2..],
            [BYPASS_APPROVALS_FLAG, BYPASS_HOOK_TRUST_FLAG]
        );
    }

    #[test]
    fn plugin_registers_the_engine() {
        use tx_core::{CoreConfig, Plugin, PluginConfig, TxCore};
        let dir = tempfile::Builder::new()
            .prefix("tx")
            .tempdir_in("/tmp")
            .unwrap();
        let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
            dir.path().join("state"),
        )))
        .unwrap();
        assert_eq!(CodexPlugin.id(), "engine-codex");
        core.plug(vec![Box::new(CodexPlugin)], &PluginConfig::default())
            .unwrap();
        assert_eq!(core.engine_kinds(), vec![EngineKind::Codex]);
    }

    fn write(path: &Path, data: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    /// A running record whose data names conversation `id` at rollout `transcript`.
    fn recorded(id: &str, transcript: &Path) -> Agent {
        session(
            LlmEngineState::Idle,
            serde_json::json!({CONVERSATION_ID_KEY: id, TRANSCRIPT_PATH_KEY: transcript}),
        )
    }

    /// Where Codex would put a rollout of `id` under `codex_home`, created on `date`.
    fn rollout(codex_home: &Path, date: &str, id: &str) -> PathBuf {
        codex_home.join("sessions").join(date).join(format!(
            "rollout-{}T10-00-00-{id}.jsonl",
            date.replace('/', "-")
        ))
    }

    fn rollout_bytes() -> Vec<u8> {
        (0..=255u8).cycle().take(204_800).collect()
    }

    #[test]
    fn archive_mirrors_the_rollout_by_conversation_id() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let transcript = rollout(dir.path(), "2026/10/31", "c1");
        write(&transcript, b"{\"type\":\"session_meta\"}\n");
        let archive = dir.path().join("archive");
        let engine = CodexAdapter::new(dir.path());
        let session = recorded("c1", &transcript);

        engine.archive(&session, &archive).unwrap();
        let archived = archive.join("c1.jsonl");
        assert_eq!(
            std::fs::read(&archived).unwrap(),
            b"{\"type\":\"session_meta\"}\n"
        );
        assert_eq!(
            std::fs::read_dir(&archive).unwrap().count(),
            1,
            "no sidecars"
        );
        let inode = std::fs::metadata(&archived).unwrap().ino();

        // A later turn appends in place.
        write(
            &transcript,
            b"{\"type\":\"session_meta\"}\n{\"type\":\"response_item\"}\n",
        );
        engine.archive(&session, &archive).unwrap();
        assert_eq!(
            std::fs::read(&archived).unwrap(),
            std::fs::read(&transcript).unwrap()
        );
        assert_eq!(std::fs::metadata(&archived).unwrap().ino(), inode);

        // After a `/new` the record names a second conversation; the first stays archived.
        let next = rollout(dir.path(), "2026/10/31", "c2");
        write(&next, b"{\"fresh\":true}\n");
        engine.archive(&recorded("c2", &next), &archive).unwrap();
        assert!(archived.is_file());
        assert_eq!(
            std::fs::read(archive.join("c2.jsonl")).unwrap(),
            b"{\"fresh\":true}\n"
        );
    }

    #[test]
    fn a_terminal_record_archives_by_whole_compare_a_running_one_by_offset() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = rollout(dir.path(), "2026/10/31", "c1");
        let data = rollout_bytes();
        write(&transcript, &data);
        let archive = dir.path().join("archive");
        let engine = CodexAdapter::new(dir.path());
        let running = recorded("c1", &transcript);
        engine.archive(&running, &archive).unwrap();
        assert_eq!(std::fs::read(archive.join("c1.jsonl")).unwrap(), data);

        // A rewrite 100 KiB before the end: a hook's offset mirror keeps the stale copy...
        let mut rewritten = data.clone();
        rewritten[data.len() - 102_400] ^= 0xFF;
        std::fs::write(&transcript, &rewritten).unwrap();
        engine.archive(&running, &archive).unwrap();
        assert_eq!(std::fs::read(archive.join("c1.jsonl")).unwrap(), data);
        // ...and the archive of a stopped record is exact.
        let stopped = Agent {
            state: LlmEngineState::Ended,
            ..recorded("c1", &transcript)
        };
        engine.archive(&stopped, &archive).unwrap();
        assert_eq!(std::fs::read(archive.join("c1.jsonl")).unwrap(), rewritten);
    }

    #[test]
    fn archive_without_a_conversation_or_a_live_rollout_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        let engine = CodexAdapter::new(dir.path());
        // No data yet (before the first prompt), the id alone, and a rollout not on disk.
        for session in [
            record(serde_json::Value::Null),
            record(serde_json::json!({"model": "gpt-6.1-sol"})),
            record(serde_json::json!({CONVERSATION_ID_KEY: "c1"})),
            recorded("c1", &rollout(dir.path(), "2026/10/31", "c1")),
        ] {
            engine.archive(&session, &archive).unwrap();
        }
        assert!(!archive.exists());
    }

    #[test]
    fn restore_puts_a_missing_rollout_back_at_the_recorded_path() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = rollout(dir.path(), "2026/10/31", "c1");
        let archive = dir.path().join("archive");
        write(&archive.join("c1.jsonl"), b"{\"one\":1}\n");
        let engine = CodexAdapter::new(dir.path());
        let session = recorded("c1", &transcript);

        assert!(!dir.path().join("sessions").exists());
        assert!(engine.restore(&session, &archive).unwrap());
        assert_eq!(std::fs::read(&transcript).unwrap(), b"{\"one\":1}\n");
        // Restoring again changes nothing: the live copy is there.
        assert!(!engine.restore(&session, &archive).unwrap());
    }

    #[test]
    fn restore_never_overwrites_a_live_rollout() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = rollout(dir.path(), "2026/10/31", "c1");
        write(&transcript, b"LIVE");
        let archive = dir.path().join("archive");
        write(&archive.join("c1.jsonl"), b"ARCHIVED");
        let restored = CodexAdapter::new(dir.path())
            .restore(&recorded("c1", &transcript), &archive)
            .unwrap();
        assert!(!restored);
        assert_eq!(std::fs::read(&transcript).unwrap(), b"LIVE");
    }

    #[test]
    fn restore_without_an_archived_copy_restores_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = rollout(dir.path(), "2026/10/31", "c1");
        let archive = dir.path().join("archive");
        write(&archive.join("other.jsonl"), b"X");
        let engine = CodexAdapter::new(dir.path());
        assert!(
            !engine
                .restore(&recorded("c1", &transcript), &archive)
                .unwrap()
        );
        assert!(!transcript.exists());
        // No conversation at all: nothing to look for (the resume refuses on its own).
        assert!(
            !engine
                .restore(&record(serde_json::Value::Null), &archive)
                .unwrap()
        );
        // The id without a path, and nothing archived under it: still nothing to do.
        assert!(
            !engine
                .restore(
                    &record(serde_json::json!({CONVERSATION_ID_KEY: "c1"})),
                    &archive
                )
                .unwrap()
        );
    }

    #[test]
    fn restore_refuses_a_record_without_a_transcript_path() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("archive");
        write(&archive.join("c1.jsonl"), b"{\"one\":1}\n");
        let session = record(serde_json::json!({CONVERSATION_ID_KEY: "c1"}));
        let error = CodexAdapter::new(dir.path())
            .restore(&session, &archive)
            .unwrap_err();
        assert!(matches!(error, EngineError::Refused(_)), "{error:?}");
        let text = error.to_string();
        assert!(text.contains("cannot restore agent"), "{text}");
        assert!(text.contains("c1.jsonl"), "{text}");
        assert!(text.contains("ext.codex.transcript_path"), "{text}");
        assert!(!dir.path().join("sessions").exists());
    }

    #[test]
    fn archive_then_restore_round_trips_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = rollout(dir.path(), "2026/10/31", "c1");
        let bytes = rollout_bytes();
        write(&transcript, &bytes);
        let archive = dir.path().join("archive");
        let engine = CodexAdapter::new(dir.path());
        let session = recorded("c1", &transcript);
        engine.archive(&session, &archive).unwrap();
        std::fs::remove_dir_all(dir.path().join("sessions")).unwrap();
        assert!(engine.restore(&session, &archive).unwrap());
        assert_eq!(std::fs::read(&transcript).unwrap(), bytes);
    }

    #[test]
    fn conversation_is_the_recorded_id_and_path_once_both_are_known() {
        let adapter = CodexAdapter::new("/h");
        let full = record(
            serde_json::json!({CONVERSATION_ID_KEY: "c1", TRANSCRIPT_PATH_KEY: "/t/c1.jsonl", "model": "m"}),
        );
        assert_eq!(
            adapter.conversation(&full),
            Some(Conversation {
                id: "c1".into(),
                transcript_path: PathBuf::from("/t/c1.jsonl"),
            })
        );
        for partial in [
            serde_json::Value::Null,
            serde_json::json!({"model": "m"}),
            serde_json::json!({CONVERSATION_ID_KEY: "c1"}),
            serde_json::json!({TRANSCRIPT_PATH_KEY: "/t/c1.jsonl"}),
            serde_json::json!("not an object"),
        ] {
            assert_eq!(
                adapter.conversation(&record(partial.clone())),
                None,
                "{partial}"
            );
        }
    }
}
