//! The Claude Code engine for tx-core: `claude [--resume ID] [--model M] --effort E --no-chrome
//! --append-system-prompt <profile text> --dangerously-skip-permissions [prompt]`, with every
//! granted skill linked under `.claude/skills/<name>` in the working directory.

use std::path::PathBuf;

use serde_json::Value;
use tx_core::{
    Effort, EngineError, LaunchPlan, LaunchRequest, LlmEngine, LlmEngineHook, LlmEngineState,
    LlmEngineStateTransition, LlmEngineStatus, WorkspaceAction, hook_shell_command,
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
/// `Notification` types that stop a turn on the operator: `Idle` with the message as the note.
/// Not `idle_prompt`: Claude sends it about 60 s after every finished turn, already `Idle`.
pub const MID_TURN_NOTIFICATIONS: [&str; 2] = ["permission_prompt", "elicitation_dialog"];
/// `SessionEnd` reasons that replace the conversation inside a process that keeps running
/// (`SessionStart` follows at once); they end nothing.
pub const CONTINUING_END_REASONS: [&str; 2] = ["clear", "resume"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClaudeAdapter;

impl LlmEngine for ClaudeAdapter {
    fn id(&self) -> &str {
        "claude"
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

    fn on_hook(
        &self,
        current: &LlmEngineStatus,
        hook: &LlmEngineHook,
    ) -> Option<LlmEngineStateTransition> {
        let field = |key: &str| hook.payload.get(key).and_then(Value::as_str);
        let to = |state| Some(LlmEngineStateTransition::to(state));
        match hook.name.as_str() {
            "SessionStart" | "Stop" => to(LlmEngineState::Idle),
            "UserPromptSubmit" => to(LlmEngineState::Working),
            // Idle with a note is a turn stopped on the operator; a tool running means they
            // answered.
            "PreToolUse" | "PostToolUse"
                if current.state == LlmEngineState::Idle && current.note.is_some() =>
            {
                to(LlmEngineState::Working)
            }
            "Notification"
                if field("notification_type")
                    .is_some_and(|kind| MID_TURN_NOTIFICATIONS.contains(&kind)) =>
            {
                Some(LlmEngineStateTransition {
                    to: LlmEngineState::Idle,
                    note: field("message").map(str::to_owned),
                })
            }
            "StopFailure" => Some(LlmEngineStateTransition {
                to: LlmEngineState::Idle,
                note: field("error").map(|error| format!("error: {error}")),
            }),
            "SessionEnd"
                if !field("reason")
                    .is_some_and(|reason| CONTINUING_END_REASONS.contains(&reason)) =>
            {
                to(LlmEngineState::Ended)
            }
            _ => None,
        }
    }
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

    /// (current state, current note, hook name, payload) → the adapter's transition.
    type Row = (
        LlmEngineState,
        Option<&'static str>,
        &'static str,
        serde_json::Value,
        Option<LlmEngineStateTransition>,
    );

    fn check(adapter: &dyn LlmEngine, table: Vec<Row>) {
        for (state, note, name, payload, expected) in table {
            let current = LlmEngineStatus {
                note: note.map(str::to_owned),
                ..LlmEngineStatus::new(state, 1.0)
            };
            assert_eq!(
                adapter.on_hook(&current, &hook(name, payload.clone())),
                expected,
                "{state:?} {note:?} {name} {payload}"
            );
        }
    }

    fn to(state: LlmEngineState) -> Option<LlmEngineStateTransition> {
        Some(LlmEngineStateTransition::to(state))
    }

    fn to_noted(state: LlmEngineState, note: &str) -> Option<LlmEngineStateTransition> {
        Some(LlmEngineStateTransition::to(state).note(note))
    }

    #[test]
    fn hooks_to_transitions() {
        use LlmEngineState::*;
        use serde_json::json;
        let none = json!({});
        let asked = "Claude needs your permission to use Bash";
        check(
            &ClaudeAdapter,
            vec![
                (Starting, None, "SessionStart", none.clone(), to(Idle)),
                (Idle, None, "UserPromptSubmit", none.clone(), to(Working)),
                (Working, None, "PreToolUse", none.clone(), None),
                (Working, None, "PostToolUse", none.clone(), None),
                (
                    Working,
                    None,
                    "Notification",
                    json!({"notification_type": "permission_prompt", "message": asked}),
                    to_noted(Idle, asked),
                ),
                (
                    Working,
                    None,
                    "Notification",
                    json!({"notification_type": "elicitation_dialog"}),
                    to(Idle),
                ),
                // The operator answered: a tool runs again.
                (Idle, Some(asked), "PreToolUse", none.clone(), to(Working)),
                (Idle, Some(asked), "PostToolUse", none.clone(), to(Working)),
                // A plain finished turn has no note: a stray tool hook changes nothing.
                (Idle, None, "PostToolUse", none.clone(), None),
                (
                    Idle,
                    None,
                    "Notification",
                    json!({"notification_type": "idle_prompt", "message": "your turn"}),
                    None,
                ),
                (Working, None, "Notification", none.clone(), None),
                (Working, None, "Stop", none.clone(), to(Idle)),
                (
                    Working,
                    None,
                    "StopFailure",
                    json!({"error": "rate_limit"}),
                    to_noted(Idle, "error: rate_limit"),
                ),
                (Working, None, "StopFailure", none.clone(), to(Idle)),
                (
                    Idle,
                    None,
                    "SessionEnd",
                    json!({"reason": "logout"}),
                    to(Ended),
                ),
                (
                    Idle,
                    None,
                    "SessionEnd",
                    json!({"reason": "prompt_input_exit"}),
                    to(Ended),
                ),
                (Idle, None, "SessionEnd", none.clone(), to(Ended)),
                (Idle, None, "SessionEnd", json!({"reason": "clear"}), None),
                (Idle, None, "SessionEnd", json!({"reason": "resume"}), None),
                (Idle, None, "PreCompact", none.clone(), None),
                (Idle, None, "stop", none.clone(), None),
            ],
        );
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
        let mut core = TxCore::open(CoreConfig::new(dir.path().join("state"))).unwrap();
        assert_eq!(ClaudePlugin.id(), "engine-claude");
        core.plug(vec![Box::new(ClaudePlugin)], &PluginConfig::default())
            .unwrap();
        assert_eq!(core.engine_ids(), vec!["claude"]);
    }
}
