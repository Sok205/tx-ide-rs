//! The Codex engine for tx-core: `codex [resume ID | fork ID] -m <model> -c model_reasoning_effort=<effort>
//! [-c developer_instructions=<profile text>] --dangerously-bypass-approvals-and-sandbox
//! --dangerously-bypass-hook-trust [prompt]`, with granted skills linked under `.agents/skills`.
//!
//! Engine data, under `ext.codex` of the record, is this adapter's alone to read:
//!
//! ```json
//! {"conversation_id": "01a19173-…", "transcript_path": "/Users/w/.codex/sessions/2026/10/31/rollout-…-01a19173-….jsonl"}
//! ```
//!
//! A resume continues `conversation_id`; a fork branches the source's `conversation_id`; both are
//! refused when there is none yet. Codex picks its own id, so a launch plan carries no engine
//! data: both keys come from the `SessionStart` payload, which Codex fires once the first prompt
//! is submitted. The data is rewritten only when the id differs; other keys of
//! the object survive. Paths Codex reports are resolved (`/private/tmp/…` for `/tmp/…`).

use std::path::PathBuf;

use serde_json::Value;
use tx_core::{
    Effort, EngineError, LaunchMode, LaunchPlan, LaunchRequest, LlmEngine, LlmEngineChange,
    LlmEngineHook, LlmEngineState, Session, WorkspaceAction, engine_data, hook_shell_command,
};

pub const CODEX_BIN: &str = "codex";
pub const SKILLS_DIR: &str = ".agents/skills";
pub const CODEX_MODEL: &str = "gpt-5.6-sol";
pub const RESUME_SUBCOMMAND: &str = "resume";
pub const FORK_SUBCOMMAND: &str = "fork";
pub const REASONING_EFFORT_KEY: &str = "model_reasoning_effort";
pub const DEVELOPER_INSTRUCTIONS_KEY: &str = "developer_instructions";
pub const BYPASS_APPROVALS_FLAG: &str = "--dangerously-bypass-approvals-and-sandbox";
pub const BYPASS_HOOK_TRUST_FLAG: &str = "--dangerously-bypass-hook-trust";
pub const DEFAULT_EFFORT: Effort = Effort::High;
/// The `ext` key this engine owns.
pub const EXT_KEY: &str = "codex";
pub const CONVERSATION_ID_KEY: &str = "conversation_id";
pub const TRANSCRIPT_PATH_KEY: &str = "transcript_path";
/// The hooks wired per launch, one `-c hooks.<Event>=…` each; each runs
/// `<hook_command> <Event>`. Codex skips them without `--dangerously-bypass-hook-trust`.
pub const HOOK_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "Interrupt",
    "SessionEnd",
];

/// Codex's own directory: `CODEX_HOME`, else `$HOME/.codex`. Rollouts live under
/// `<codex home>/sessions/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexAdapter {
    pub codex_home: PathBuf,
}

impl CodexAdapter {
    pub fn new(codex_home: impl Into<PathBuf>) -> Self {
        Self {
            codex_home: codex_home.into(),
        }
    }

    /// The conversation recorded for `session`; refused when there is none yet (Codex reports
    /// its id only once a first prompt was submitted).
    fn conversation_of<'a>(
        &self,
        session: &'a Session,
        verb: &str,
    ) -> Result<&'a str, EngineError> {
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
}

impl LlmEngine for CodexAdapter {
    fn id(&self) -> &str {
        "codex"
    }

    fn ext_key(&self) -> &'static str {
        EXT_KEY
    }

    fn launch(&self, req: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        let model = req
            .model
            .filter(|model| !model.is_empty())
            .unwrap_or(CODEX_MODEL);
        let effort = req.effort.unwrap_or(DEFAULT_EFFORT);
        let mut args = Vec::new();
        match req.mode {
            LaunchMode::Start => {}
            LaunchMode::Resume => args.extend([
                RESUME_SUBCOMMAND.to_owned(),
                self.conversation_of(req.session, "resume")?.to_owned(),
            ]),
            LaunchMode::Fork { source } => args.extend([
                FORK_SUBCOMMAND.to_owned(),
                self.conversation_of(source, "fork")?.to_owned(),
            ]),
        }
        args.extend([
            "-m".to_owned(),
            model.to_owned(),
            "-c".to_owned(),
            format!("{REASONING_EFFORT_KEY}={}", effort.as_str()),
        ]);
        if !req.profile.text().is_empty() {
            args.extend([
                "-c".to_owned(),
                format!("{DEVELOPER_INSTRUCTIONS_KEY}={}", req.profile.text()),
            ]);
        }
        if !req.hook_command.is_empty() {
            for event in HOOK_EVENTS {
                args.extend(["-c".to_owned(), hook_override(req.hook_command, event)]);
            }
        }
        args.extend([
            BYPASS_APPROVALS_FLAG.to_owned(),
            BYPASS_HOOK_TRUST_FLAG.to_owned(),
        ]);
        if let Some(prompt) = req.initial_prompt.filter(|prompt| !prompt.is_empty()) {
            args.push(prompt.to_owned());
        }
        let mut plan = LaunchPlan::new(CODEX_BIN).args(args);
        for link in skill_links(SKILLS_DIR, req.profile.skill_dirs()) {
            plan = plan.workspace(link);
        }
        Ok(plan)
    }

    /// An approval stops the turn on the operator (`Idle`); the next `PostToolUse` means they
    /// answered. `SessionEnd` changes nothing: Codex also fires it after 30 min idle while the
    /// process lives, and its payload cannot tell the two apart; core ends the session when the
    /// process is gone.
    fn on_hook(&self, session: &Session, hook: &LlmEngineHook) -> LlmEngineChange {
        let state = match hook.name.as_str() {
            "SessionStart" | "Stop" | "Interrupt" | "PermissionRequest" => {
                Some(LlmEngineState::Idle)
            }
            "UserPromptSubmit" | "PostToolUse" => Some(LlmEngineState::Working),
            _ => None,
        };
        let engine_data = match hook.name.as_str() {
            "SessionStart" => conversation_started(engine_data(self, session), &hook.payload),
            _ => None,
        };
        LlmEngineChange { state, engine_data }
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

/// `hooks.<event>=[{matcher="",hooks=[{type="command",command="…"}]}]`, TOML inline. The
/// command is a TOML basic string; JSON string escapes are valid TOML escapes.
pub fn hook_override(hook_command: &[String], event: &str) -> String {
    let command = Value::from(hook_shell_command(hook_command, event)).to_string();
    format!("hooks.{event}=[{{matcher=\"\",hooks=[{{type=\"command\",command={command}}}]}}]")
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
    fn record(data: serde_json::Value) -> Session {
        session(LlmEngineState::Starting, data)
    }

    /// An agent record in `state` whose `ext.CodexAdapter` is `data` (absent when `Null`).
    fn session(state: LlmEngineState, data: serde_json::Value) -> Session {
        use tx_core::{Launch, Lifecycle, SessionId};
        let mut ext = std::collections::BTreeMap::new();
        if !data.is_null() {
            ext.insert(EXT_KEY.to_owned(), data);
        }
        Session {
            id: SessionId::mint(),
            name: "agent".into(),
            parent: None,
            tags: Vec::new(),
            cwd: "/tmp".into(),
            created_at: 1.0,
            ended_at: None,
            last_modified_at: 1.0,
            lifecycle: Lifecycle::Running,
            launch: Launch::Agent {
                engine: adapter().id().to_owned(),
                profile: Vec::new(),
                model: None,
                effort: None,
                resume: None,
            },
            state: Some(state),
            ext,
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
                to(Idle),
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
            (Working, null.clone(), "Stop", none.clone(), to(Idle)),
            (Working, null.clone(), "Interrupt", none.clone(), to(Idle)),
            (
                Idle,
                null.clone(),
                "SessionEnd",
                json!({"reason": "other"}),
                LlmEngineChange::NONE,
            ),
            (
                Idle,
                null.clone(),
                "PreToolUse",
                none.clone(),
                LlmEngineChange::NONE,
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
                to(Idle).engine_data(first.clone()),
            ),
            // The same id again: state only.
            (
                Starting,
                first.clone(),
                "SessionStart",
                started("id-1"),
                to(Idle),
            ),
            // A resume launched with only the id gains the transcript path.
            (
                Starting,
                json!({"conversation_id": "id-1"}),
                "SessionStart",
                started("id-1"),
                to(Idle).engine_data(first.clone()),
            ),
            // A payload without both strings changes no data.
            (Starting, null.clone(), "SessionStart", json!({}), to(Idle)),
            (
                Starting,
                null.clone(),
                "SessionStart",
                json!({"session_id": "id-1"}),
                to(Idle),
            ),
            (
                Starting,
                null.clone(),
                "SessionStart",
                json!({"session_id": "id-1", "transcript_path": null}),
                to(Idle),
            ),
            // Hand-edited non-object data counts as absent.
            (
                Idle,
                json!(["junk"]),
                "SessionStart",
                started("id-1"),
                to(Idle).engine_data(first.clone()),
            ),
            // Other keys of the object survive a new conversation.
            (
                Idle,
                json!({"conversation_id": "id-1", "transcript_path": "/old", "effort": "high"}),
                "SessionStart",
                started("id-2"),
                to(Idle).engine_data(json!({
                    "conversation_id": "id-2",
                    "transcript_path": path.replace("id-1", "id-2"),
                    "effort": "high",
                })),
            ),
            // Only SessionStart carries data.
            (Working, null.clone(), "Stop", started("id-3"), to(Idle)),
        ]);
    }

    #[test]
    fn ext_key_is_codex() {
        assert_eq!(adapter().ext_key(), "codex");
        assert_eq!(adapter().ext_key(), EXT_KEY);
    }

    #[test]
    fn id_is_codex() {
        assert_eq!(adapter().id(), "codex");
    }

    #[test]
    fn defaults() {
        let (_dir, profile) = profile("", &[]);
        let session = record(serde_json::Value::Null);
        let plan = adapter()
            .launch(&LaunchRequest::new(&session, &profile))
            .unwrap();
        assert_eq!(plan.program, "codex");
        assert_eq!(
            plan.args,
            argv(&[
                "-m",
                "gpt-5.6-sol",
                "-c",
                "model_reasoning_effort=high",
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
            ])
        );
        assert!(plan.workspace.is_empty());
    }

    #[test]
    fn every_option() {
        let (_dir, profile) = profile("be good", &["/skills/one"]);
        let session = record(serde_json::Value::Null);
        let plan = adapter()
            .launch(
                &LaunchRequest::new(&session, &profile)
                    .model("o3")
                    .effort(Effort::Low)
                    .initial_prompt("go"),
            )
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "-m",
                "o3",
                "-c",
                "model_reasoning_effort=low",
                "-c",
                "developer_instructions=be good",
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
            .launch(&LaunchRequest::new(&session, &profile).initial_prompt("hi"))
            .unwrap();
        assert_eq!(plan.args[0], "-m");
        assert_eq!(plan.args.last().map(String::as_str), Some("hi"));
        assert_eq!(plan.engine_data, None, "Codex reports its id by hook");
    }

    #[test]
    fn resume_is_the_subcommand_on_the_recorded_conversation() {
        let (_dir, profile) = profile("", &[]);
        let session = record(serde_json::json!({"conversation_id": "sess-7"}));
        let plan = adapter()
            .launch(
                &LaunchRequest::new(&session, &profile)
                    .mode(LaunchMode::Resume)
                    .model("o3")
                    .initial_prompt("more"),
            )
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
                "--dangerously-bypass-approvals-and-sandbox",
                "--dangerously-bypass-hook-trust",
                "more",
            ])
        );
        assert_eq!(plan.engine_data, None);
    }

    #[test]
    fn fork_is_the_subcommand_on_the_source_conversation() {
        let (_dir, profile) = profile("", &[]);
        let source = record(serde_json::json!({"conversation_id": "sess-7"}));
        let session = record(serde_json::Value::Null);
        let plan = adapter()
            .launch(
                &LaunchRequest::new(&session, &profile)
                    .mode(LaunchMode::Fork { source: &source })
                    .initial_prompt("go"),
            )
            .unwrap();
        assert_eq!(plan.args[..2], argv(&["fork", "sess-7"]));
        assert_eq!(plan.args.last().map(String::as_str), Some("go"));
        assert_eq!(plan.engine_data, None);
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
                .launch(&LaunchRequest::new(&session, &profile).mode(LaunchMode::Resume))
                .unwrap_err();
            assert!(
                matches!(&error, EngineError::Refused(text) if text.contains("cannot resume agent")),
                "{error:?}"
            );
            let target = record(serde_json::Value::Null);
            let error = adapter()
                .launch(
                    &LaunchRequest::new(&target, &profile)
                        .mode(LaunchMode::Fork { source: &session }),
                )
                .unwrap_err();
            assert!(
                matches!(&error, EngineError::Refused(text) if text.contains("cannot fork agent")),
                "{error:?}"
            );
        }
    }

    #[test]
    fn empty_model_and_prompt_fall_back() {
        let (_dir, profile) = profile("", &[]);
        let session = record(serde_json::Value::Null);
        let plan = adapter()
            .launch(
                &LaunchRequest::new(&session, &profile)
                    .model("")
                    .initial_prompt(""),
            )
            .unwrap();
        assert_eq!(plan.args[1], "gpt-5.6-sol");
        assert_eq!(plan.args.len(), 6);
    }

    #[test]
    fn hook_command_wires_every_event_as_a_config_override() {
        let (_dir, profile) = profile("", &[]);
        let hook_command = ["/x\"y/txc".to_owned(), "chat".to_owned(), "hook".to_owned()];
        let session = record(serde_json::Value::Null);
        let plan = adapter()
            .launch(&LaunchRequest::new(&session, &profile).hook_command(&hook_command))
            .unwrap();
        let overrides: Vec<&str> = plan
            .args
            .windows(2)
            .filter(|pair| pair[0] == "-c" && pair[1].starts_with("hooks."))
            .map(|pair| pair[1].as_str())
            .collect();
        assert_eq!(overrides.len(), HOOK_EVENTS.len());
        assert_eq!(
            overrides[4],
            r#"hooks.Stop=[{matcher="",hooks=[{type="command",command="'/x\"y/txc' 'chat' 'hook' 'Stop' || exit 1"}]}]"#
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
            .prefix("txc")
            .tempdir_in("/tmp")
            .unwrap();
        let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
            dir.path().join("state"),
        )))
        .unwrap();
        assert_eq!(CodexPlugin.id(), "engine-codex");
        core.plug(vec![Box::new(CodexPlugin)], &PluginConfig::default())
            .unwrap();
        assert_eq!(core.engine_ids(), vec!["codex"]);
    }
}
