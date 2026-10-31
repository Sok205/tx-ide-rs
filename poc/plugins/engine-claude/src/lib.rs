//! The Claude Code engine for tx-core: `claude [--resume ID] [--model M] --effort E --no-chrome
//! --append-system-prompt <profile text> --dangerously-skip-permissions [prompt]`, with every
//! granted skill linked under `.claude/skills/<name>` in the working directory.
//!
//! Engine data, under `ext.claude` of the record, is this adapter's alone to read:
//!
//! ```json
//! {"conversation_id": "5f0c…", "transcript_path": "/Users/w/.claude/projects/-Users-w-…/5f0c….jsonl"}
//! ```
//!
//! Both come from the `SessionStart` payload. Claude reports them on every start (`startup`,
//! `resume`, `clear`); the data is rewritten only when the id differs, so a `/clear` (a new
//! conversation inside the same process) is what changes it. Other keys of the object survive.

use std::path::PathBuf;

use serde_json::Value;
use tx_core::{
    Effort, EngineError, LaunchPlan, LaunchRequest, LlmEngine, LlmEngineChange, LlmEngineHook,
    LlmEngineState, Session, WorkspaceAction, engine_data, hook_shell_command,
};

pub const CLAUDE_BIN: &str = "claude";
pub const SKILLS_DIR: &str = ".claude/skills";
pub const MODEL_FLAG: &str = "--model";
pub const RESUME_FLAG: &str = "--resume";
pub const EFFORT_FLAG: &str = "--effort";
pub const NO_CHROME_FLAG: &str = "--no-chrome";
pub const APPEND_SYSTEM_PROMPT_FLAG: &str = "--append-system-prompt";
pub const SKIP_PERMISSIONS_FLAG: &str = "--dangerously-skip-permissions";
pub const SETTINGS_FLAG: &str = "--settings";
/// The hooks wired per launch; each runs `<hook_command> <Event>`, so `on_hook` sees Claude's
/// own event names.
pub const HOOK_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];
/// Seconds Claude waits for one hook before moving on.
pub const HOOK_TIMEOUT_SECONDS: u64 = 10;
pub const DEFAULT_EFFORT: Effort = Effort::High;
/// `Notification` types that stop a turn on the operator: `Idle`. Not `idle_prompt`: Claude
/// sends it about 60 s after every finished turn, which is already `Idle`.
pub const MID_TURN_NOTIFICATIONS: [&str; 2] = ["permission_prompt", "elicitation_dialog"];
/// `SessionEnd` reasons that replace the conversation inside a process that keeps running
/// (`SessionStart` follows at once); they end nothing.
pub const CONTINUING_END_REASONS: [&str; 2] = ["clear", "resume"];
/// The `ext` key this engine owns.
pub const EXT_KEY: &str = "claude";
pub const CONVERSATION_ID_KEY: &str = "conversation_id";
pub const TRANSCRIPT_PATH_KEY: &str = "transcript_path";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClaudeAdapter;

impl LlmEngine for ClaudeAdapter {
    fn id(&self) -> &str {
        "claude"
    }

    fn ext_key(&self) -> &'static str {
        EXT_KEY
    }

    fn launch(&self, req: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        let mut args = Vec::new();
        if let Some(resume) = req.resume.filter(|resume| !resume.is_empty()) {
            args.extend([RESUME_FLAG.to_owned(), resume.to_owned()]);
        }
        if let Some(model) = req.model.filter(|model| !model.is_empty()) {
            args.extend([MODEL_FLAG.to_owned(), model.to_owned()]);
        }
        args.extend([
            EFFORT_FLAG.to_owned(),
            req.effort.unwrap_or(DEFAULT_EFFORT).as_str().to_owned(),
            NO_CHROME_FLAG.to_owned(),
        ]);
        if !req.profile.text().is_empty() {
            args.extend([
                APPEND_SYSTEM_PROMPT_FLAG.to_owned(),
                req.profile.text().to_owned(),
            ]);
        }
        if !req.hook_command.is_empty() {
            args.extend([SETTINGS_FLAG.to_owned(), hook_settings(req.hook_command)]);
        }
        args.push(SKIP_PERMISSIONS_FLAG.to_owned());
        if let Some(prompt) = req.initial_prompt.filter(|prompt| !prompt.is_empty()) {
            args.push(prompt.to_owned());
        }
        let mut plan = LaunchPlan::new(CLAUDE_BIN).args(args);
        for link in skill_links(SKILLS_DIR, req.profile.skill_dirs()) {
            plan = plan.workspace(link);
        }
        Ok(plan)
    }

    fn on_hook(&self, session: &Session, hook: &LlmEngineHook) -> LlmEngineChange {
        let field = |key: &str| hook.payload.get(key).and_then(Value::as_str);
        let state = match hook.name.as_str() {
            "SessionStart" | "Stop" | "StopFailure" => Some(LlmEngineState::Idle),
            // Tool hooks fire only inside a turn: after a stop on the operator they mean the
            // operator answered.
            "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(LlmEngineState::Working),
            "Notification"
                if field("notification_type")
                    .is_some_and(|kind| MID_TURN_NOTIFICATIONS.contains(&kind)) =>
            {
                Some(LlmEngineState::Idle)
            }
            "SessionEnd"
                if !field("reason")
                    .is_some_and(|reason| CONTINUING_END_REASONS.contains(&reason)) =>
            {
                Some(LlmEngineState::Ended)
            }
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

/// `--settings` JSON wiring every [`HOOK_EVENTS`] entry to `hook_command`. Inline settings
/// add to the user's own hooks rather than replacing them.
pub fn hook_settings(hook_command: &[String]) -> String {
    let hooks: serde_json::Map<String, Value> = HOOK_EVENTS
        .iter()
        .map(|event| {
            let entry = serde_json::json!([{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": hook_shell_command(hook_command, event),
                    "timeout": HOOK_TIMEOUT_SECONDS,
                }],
            }]);
            ((*event).to_owned(), entry)
        })
        .collect();
    serde_json::json!({ "hooks": hooks }).to_string()
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

/// The plugin wrapper: registers [`ClaudeAdapter`] under the plugin id `engine-claude`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudePlugin;

impl tx_core::Plugin for ClaudePlugin {
    fn id(&self) -> &'static str {
        "engine-claude"
    }

    fn register(&self, r: &mut tx_core::Registry) -> Result<(), tx_core::PluginError> {
        r.engine(Box::new(ClaudeAdapter));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
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

    /// An agent record in `state` whose `ext.ClaudeAdapter` is `data` (absent when `Null`).
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
                engine: ClaudeAdapter.id().to_owned(),
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
                ClaudeAdapter.on_hook(
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
            // Tool hooks fire only inside a turn: Working, whatever came before.
            (
                Working,
                null.clone(),
                "PreToolUse",
                none.clone(),
                to(Working),
            ),
            (Idle, null.clone(), "PreToolUse", none.clone(), to(Working)),
            (Idle, null.clone(), "PostToolUse", none.clone(), to(Working)),
            (
                Working,
                null.clone(),
                "Notification",
                json!({"notification_type": "permission_prompt", "message": "Claude needs your permission to use Bash"}),
                to(Idle),
            ),
            (
                Working,
                null.clone(),
                "Notification",
                json!({"notification_type": "elicitation_dialog"}),
                to(Idle),
            ),
            (
                Idle,
                null.clone(),
                "Notification",
                json!({"notification_type": "idle_prompt", "message": "your turn"}),
                LlmEngineChange::NONE,
            ),
            (
                Working,
                null.clone(),
                "Notification",
                none.clone(),
                LlmEngineChange::NONE,
            ),
            (Working, null.clone(), "Stop", none.clone(), to(Idle)),
            (
                Working,
                null.clone(),
                "StopFailure",
                json!({"error": "rate_limit"}),
                to(Idle),
            ),
            (
                Idle,
                null.clone(),
                "SessionEnd",
                json!({"reason": "logout"}),
                to(Ended),
            ),
            (
                Idle,
                null.clone(),
                "SessionEnd",
                json!({"reason": "prompt_input_exit"}),
                to(Ended),
            ),
            (Idle, null.clone(), "SessionEnd", none.clone(), to(Ended)),
            (
                Idle,
                null.clone(),
                "SessionEnd",
                json!({"reason": "clear"}),
                LlmEngineChange::NONE,
            ),
            (
                Idle,
                null.clone(),
                "SessionEnd",
                json!({"reason": "resume"}),
                LlmEngineChange::NONE,
            ),
            (
                Idle,
                null.clone(),
                "PreCompact",
                none.clone(),
                LlmEngineChange::NONE,
            ),
            (
                Idle,
                null.clone(),
                "stop",
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
        let first =
            json!({"conversation_id": "id-1", "transcript_path": "/h/projects/-w/id-1.jsonl"});
        let started = |id: &str, source: &str| {
            json!({
                "session_id": id,
                "transcript_path": format!("/h/projects/-w/{id}.jsonl"),
                "cwd": "/w",
                "hook_event_name": "SessionStart",
                "source": source,
            })
        };
        check(vec![
            // The first start, before any data: id and path land.
            (
                Starting,
                null.clone(),
                "SessionStart",
                started("id-1", "startup"),
                to(Idle).engine_data(first.clone()),
            ),
            // The same id again (a restart with the data already written): state only.
            (
                Starting,
                first.clone(),
                "SessionStart",
                started("id-1", "resume"),
                to(Idle),
            ),
            // A payload without the strings changes no data.
            (Starting, null.clone(), "SessionStart", json!({}), to(Idle)),
            (
                Starting,
                null.clone(),
                "SessionStart",
                json!({"session_id": 7, "transcript_path": "/x"}),
                to(Idle),
            ),
            (
                Starting,
                null.clone(),
                "SessionStart",
                json!({"session_id": "id-1"}),
                to(Idle),
            ),
            // Hand-edited non-object data counts as absent.
            (
                Idle,
                json!("junk"),
                "SessionStart",
                started("id-1", "startup"),
                to(Idle).engine_data(first.clone()),
            ),
            // Other keys of the object survive a new conversation.
            (
                Idle,
                json!({"conversation_id": "id-1", "transcript_path": "/old", "model": "sonnet"}),
                "SessionStart",
                started("id-2", "clear"),
                to(Idle).engine_data(json!({
                    "conversation_id": "id-2",
                    "transcript_path": "/h/projects/-w/id-2.jsonl",
                    "model": "sonnet",
                })),
            ),
        ]);
    }

    /// `/clear`: `SessionEnd{reason: clear}` ends nothing, then `SessionStart{source: clear}`
    /// brings a new id.
    #[test]
    fn clear_sequence_keeps_the_agent_and_changes_the_conversation() {
        use serde_json::json;
        let data = json!({"conversation_id": "id-1", "transcript_path": "/h/id-1.jsonl"});
        let session = session(LlmEngineState::Idle, data.clone());
        assert_eq!(
            ClaudeAdapter.on_hook(&session, &hook("SessionEnd", json!({"reason": "clear"}))),
            LlmEngineChange::NONE
        );
        let change = ClaudeAdapter.on_hook(
            &session,
            &hook(
                "SessionStart",
                json!({"session_id": "id-2", "transcript_path": "/h/id-2.jsonl", "source": "clear"}),
            ),
        );
        assert_eq!(
            change,
            to(LlmEngineState::Idle).engine_data(
                json!({"conversation_id": "id-2", "transcript_path": "/h/id-2.jsonl"})
            )
        );
    }

    #[test]
    fn ext_key_is_claude() {
        assert_eq!(ClaudeAdapter.ext_key(), "claude");
        assert_eq!(ClaudeAdapter.ext_key(), EXT_KEY);
    }

    #[test]
    fn id_is_claude() {
        assert_eq!(ClaudeAdapter.id(), "claude");
    }

    #[test]
    fn defaults() {
        let (dir, profile) = profile("", &[]);
        let plan = ClaudeAdapter
            .launch(&LaunchRequest::new(dir.path(), &profile))
            .unwrap();
        assert_eq!(plan.program, "claude");
        assert_eq!(
            plan.args,
            argv(&[
                "--effort",
                "high",
                "--no-chrome",
                "--dangerously-skip-permissions"
            ])
        );
        assert!(plan.env.is_empty());
        assert!(plan.workspace.is_empty());
    }

    #[test]
    fn every_option() {
        let (dir, profile) = profile("# Role\nbe good", &["/skills/one", "/other/two"]);
        let plan = ClaudeAdapter
            .launch(
                &LaunchRequest::new(dir.path(), &profile)
                    .model("opus")
                    .effort(Effort::Medium)
                    .initial_prompt("go"),
            )
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "--model",
                "opus",
                "--effort",
                "medium",
                "--no-chrome",
                "--append-system-prompt",
                "# Role\nbe good",
                "--dangerously-skip-permissions",
                "go",
            ])
        );
        assert_eq!(
            plan.workspace,
            vec![
                WorkspaceAction::Symlink {
                    src: "/skills/one".into(),
                    dst: ".claude/skills/one".into()
                },
                WorkspaceAction::Symlink {
                    src: "/other/two".into(),
                    dst: ".claude/skills/two".into()
                },
            ]
        );
    }

    #[test]
    fn empty_model_and_prompt_are_absent() {
        let (dir, profile) = profile("", &[]);
        let plan = ClaudeAdapter
            .launch(
                &LaunchRequest::new(dir.path(), &profile)
                    .model("")
                    .effort(Effort::Max)
                    .initial_prompt(""),
            )
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "--effort",
                "max",
                "--no-chrome",
                "--dangerously-skip-permissions"
            ])
        );
    }

    #[test]
    fn resume_continues_a_conversation() {
        let (dir, profile) = profile("role", &[]);
        let plan = ClaudeAdapter
            .launch(
                &LaunchRequest::new(dir.path(), &profile)
                    .resume("chat-42")
                    .initial_prompt("more"),
            )
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "--resume",
                "chat-42",
                "--effort",
                "high",
                "--no-chrome",
                "--append-system-prompt",
                "role",
                "--dangerously-skip-permissions",
                "more",
            ])
        );
        let plan = ClaudeAdapter
            .launch(&LaunchRequest::new(dir.path(), &profile).resume(""))
            .unwrap();
        assert!(
            !plan.args.iter().any(|a| a == "--resume"),
            "empty resume is absent"
        );
    }

    #[test]
    fn hook_command_wires_every_event_through_settings() {
        let (dir, profile) = profile("", &[]);
        let hook_command = ["/x y/txc".to_owned(), "chat".to_owned(), "hook".to_owned()];
        let plan = ClaudeAdapter
            .launch(&LaunchRequest::new(dir.path(), &profile).hook_command(&hook_command))
            .unwrap();
        let at = plan
            .args
            .iter()
            .position(|arg| arg == "--settings")
            .unwrap();
        assert_eq!(plan.args[at + 2], "--dangerously-skip-permissions");
        let settings: Value = serde_json::from_str(&plan.args[at + 1]).unwrap();
        let hooks = settings["hooks"].as_object().unwrap();
        assert_eq!(
            hooks.keys().map(String::as_str).collect::<Vec<_>>(),
            HOOK_EVENTS
        );
        assert_eq!(
            settings["hooks"]["Stop"],
            serde_json::json!([{"matcher": "", "hooks": [{
                "type": "command",
                "command": "'/x y/txc' 'chat' 'hook' 'Stop' || exit 1",
                "timeout": 10,
            }]}])
        );
    }

    #[test]
    fn a_skill_dir_without_a_name_is_skipped() {
        assert!(skill_links(".x", &[PathBuf::from("/")]).is_empty());
        assert_eq!(skill_links(".x", &[PathBuf::from("/a/b/")]).len(), 1);
        let _ = Path::new("");
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
        assert_eq!(ClaudePlugin.id(), "engine-claude");
        core.plug(vec![Box::new(ClaudePlugin)], &PluginConfig::default())
            .unwrap();
        assert_eq!(core.engine_ids(), vec!["claude"]);
    }
}
