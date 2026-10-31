//! The Codex engine for tx-core: `codex [resume ID] -m <model> -c model_reasoning_effort=<effort>
//! [-c developer_instructions=<profile text>] --dangerously-bypass-approvals-and-sandbox
//! --dangerously-bypass-hook-trust [prompt]`, with granted skills linked under `.agents/skills`.
//!
//! Engine data, under `ext.codex` of the record, is this adapter's alone to read:
//!
//! ```json
//! {"conversation_id": "01a19173-…", "transcript_path": "/Users/w/.codex/sessions/2026/10/31/rollout-…-01a19173-….jsonl"}
//! ```
//!
//! Codex picks its own id; both come from the `SessionStart` payload, which Codex fires once
//! the first prompt is submitted. The data is rewritten only when the id differs; other keys of
//! the object survive. Paths Codex reports are resolved (`/private/tmp/…` for `/tmp/…`).

use std::path::PathBuf;

use serde_json::Value;
use tx_core::{
    Effort, EngineError, LaunchPlan, LaunchRequest, LlmEngine, LlmEngineChange, LlmEngineHook,
    LlmEngineState, Session, WorkspaceAction, engine_data, hook_shell_command,
};

pub const CODEX_BIN: &str = "codex";
pub const SKILLS_DIR: &str = ".agents/skills";
pub const CODEX_MODEL: &str = "gpt-5.6-sol";
pub const RESUME_SUBCOMMAND: &str = "resume";
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodexAdapter;

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
        if let Some(resume) = req.resume.filter(|resume| !resume.is_empty()) {
            args.extend([RESUME_SUBCOMMAND.to_owned(), resume.to_owned()]);
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
/// the id is already the current one.
pub fn conversation_started(current: &Value, payload: &Value) -> Option<Value> {
    let conversation_id = payload.get("session_id")?.as_str()?;
    let transcript_path = payload.get(TRANSCRIPT_PATH_KEY)?.as_str()?;
    if current.get(CONVERSATION_ID_KEY).and_then(Value::as_str) == Some(conversation_id) {
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

/// The plugin wrapper: registers [`CodexAdapter`] under the plugin id `engine-codex`.
#[derive(Clone, Copy, Debug, Default)]
pub struct CodexPlugin;

impl tx_core::Plugin for CodexPlugin {
    fn id(&self) -> &'static str {
        "engine-codex"
    }

    fn register(&self, r: &mut tx_core::Registry) -> Result<(), tx_core::PluginError> {
        r.engine(Box::new(CodexAdapter));
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
                engine: CodexAdapter.id().to_owned(),
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
                CodexAdapter.on_hook(
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
        assert_eq!(CodexAdapter.ext_key(), "codex");
        assert_eq!(CodexAdapter.ext_key(), EXT_KEY);
    }

    #[test]
    fn id_is_codex() {
        assert_eq!(CodexAdapter.id(), "codex");
    }

    #[test]
    fn defaults() {
        let (dir, profile) = profile("", &[]);
        let plan = CodexAdapter
            .launch(&LaunchRequest::new(dir.path(), &profile))
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
        let (dir, profile) = profile("be good", &["/skills/one"]);
        let plan = CodexAdapter
            .launch(
                &LaunchRequest::new(dir.path(), &profile)
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
    fn resume_is_the_subcommand() {
        let (dir, profile) = profile("", &[]);
        let plan = CodexAdapter
            .launch(
                &LaunchRequest::new(dir.path(), &profile)
                    .resume("sess-7")
                    .model("o3"),
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
            ])
        );
        let plan = CodexAdapter
            .launch(&LaunchRequest::new(dir.path(), &profile).resume(""))
            .unwrap();
        assert_eq!(plan.args[0], "-m", "empty resume is absent");
    }

    #[test]
    fn empty_model_and_prompt_fall_back() {
        let (dir, profile) = profile("", &[]);
        let plan = CodexAdapter
            .launch(
                &LaunchRequest::new(dir.path(), &profile)
                    .model("")
                    .initial_prompt(""),
            )
            .unwrap();
        assert_eq!(plan.args[1], "gpt-5.6-sol");
        assert_eq!(plan.args.len(), 6);
    }

    #[test]
    fn hook_command_wires_every_event_as_a_config_override() {
        let (dir, profile) = profile("", &[]);
        let hook_command = ["/x\"y/txc".to_owned(), "chat".to_owned(), "hook".to_owned()];
        let plan = CodexAdapter
            .launch(&LaunchRequest::new(dir.path(), &profile).hook_command(&hook_command))
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
