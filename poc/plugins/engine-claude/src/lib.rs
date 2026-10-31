//! The Claude Code engine for tx-core: `claude <conversation> [--model M] --effort E --no-chrome
//! --append-system-prompt <profile text> --dangerously-skip-permissions [prompt]`, with every
//! granted skill linked under `.claude/skills/<name>` in the working directory. The conversation
//! flags depend on the launch mode:
//!
//! | mode   | flags                                                                   |
//! |--------|-------------------------------------------------------------------------|
//! | Start  | `--session-id <session id>`                                             |
//! | Resume | `--resume <conversation_id, else session id>`                           |
//! | Fork   | `--resume <source's conversation_id, else its id> --fork-session --session-id <session id>` |
//!
//! Engine data, under `ext.claude` of the record, is this adapter's alone to read:
//!
//! ```json
//! {"conversation_id": "5f0c…", "transcript_path": "/Users/w/.claude/projects/-Users-w-…/5f0c….jsonl"}
//! ```
//!
//! A start or a fork names its conversation after the tx session id, so the launch plan carries
//! both up front. After that, `SessionStart` reports them on every start (`startup`, `resume`,
//! `clear`); the data is rewritten only when they differ, so a `/clear` (a new conversation
//! inside the same process) is what changes it. Other keys of the object survive.

use std::path::{Path, PathBuf};

use serde_json::Value;
use tx_core::{
    Effort, EngineError, LaunchMode, LaunchPlan, LaunchRequest, LlmEngine, LlmEngineChange,
    LlmEngineHook, LlmEngineState, Session, WorkspaceAction, engine_data, hook_shell_command,
};

pub const CLAUDE_BIN: &str = "claude";
pub const SKILLS_DIR: &str = ".claude/skills";
pub const MODEL_FLAG: &str = "--model";
pub const RESUME_FLAG: &str = "--resume";
pub const SESSION_ID_FLAG: &str = "--session-id";
pub const FORK_SESSION_FLAG: &str = "--fork-session";
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

/// Claude's own directory: `CLAUDE_CONFIG_DIR`, else `$HOME/.claude`. Transcripts live under
/// `<claude home>/projects/<munge(realpath(cwd))>/<conversation id>.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeAdapter {
    pub claude_home: PathBuf,
}

impl ClaudeAdapter {
    pub fn new(claude_home: impl Into<PathBuf>) -> Self {
        Self {
            claude_home: claude_home.into(),
        }
    }

    /// Where Claude writes conversation `id` of a process started in `cwd`.
    pub fn transcript_path(&self, cwd: &Path, id: &str) -> PathBuf {
        self.claude_home
            .join("projects")
            .join(munge(&realpath(cwd).to_string_lossy()))
            .join(format!("{id}.jsonl"))
    }

    /// The engine data of a conversation named after `session`: its id and transcript path.
    fn named_after(&self, session: &Session) -> Value {
        let id = session.id.as_str();
        serde_json::json!({
            CONVERSATION_ID_KEY: id,
            TRANSCRIPT_PATH_KEY: self.transcript_path(&session.cwd, id).to_string_lossy(),
        })
    }

    /// The conversation recorded for `session`, else the one named after it.
    fn conversation_of<'a>(&self, session: &'a Session) -> &'a str {
        engine_data(self, session)
            .get(CONVERSATION_ID_KEY)
            .and_then(Value::as_str)
            .unwrap_or(session.id.as_str())
    }
}

impl LlmEngine for ClaudeAdapter {
    fn id(&self) -> &str {
        "claude"
    }

    fn ext_key(&self) -> &'static str {
        EXT_KEY
    }

    fn launch(&self, req: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        let session_id = req.session.id.to_string();
        let (mut args, engine_data) = match req.mode {
            LaunchMode::Start => (
                vec![SESSION_ID_FLAG.to_owned(), session_id],
                Some(self.named_after(req.session)),
            ),
            LaunchMode::Resume => (
                vec![
                    RESUME_FLAG.to_owned(),
                    self.conversation_of(req.session).to_owned(),
                ],
                None,
            ),
            LaunchMode::Fork { source } => (
                vec![
                    RESUME_FLAG.to_owned(),
                    self.conversation_of(source).to_owned(),
                    FORK_SESSION_FLAG.to_owned(),
                    SESSION_ID_FLAG.to_owned(),
                    session_id,
                ],
                Some(self.named_after(req.session)),
            ),
        };
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
        plan.engine_data = engine_data;
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

/// Claude's project-dir name for an absolute path: every `/` and `.` becomes `-`.
pub fn munge(cwd: &str) -> String {
    cwd.replace(['/', '.'], "-")
}

/// Symlinks resolved along the existing prefix, the missing tail kept.
pub fn realpath(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut head = path;
    loop {
        if let Ok(real) = std::fs::canonicalize(head) {
            return tail.iter().rev().fold(real, |acc, part| acc.join(part));
        }
        match (head.parent(), head.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name);
                head = parent;
            }
            _ => return path.to_owned(),
        }
    }
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

/// The plugin wrapper: registers [`ClaudeAdapter`] under the plugin id `engine-claude`, with the
/// Claude home resolved once from the environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClaudePlugin;

/// `CLAUDE_CONFIG_DIR`, else `$HOME/.claude`.
pub fn claude_home_from_env() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(directory) if !directory.is_empty() => PathBuf::from(directory),
        _ => std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".claude"),
    }
}

impl tx_core::Plugin for ClaudePlugin {
    fn id(&self) -> &'static str {
        "engine-claude"
    }

    fn register(&self, r: &mut tx_core::Registry) -> Result<(), tx_core::PluginError> {
        r.engine(Box::new(ClaudeAdapter::new(claude_home_from_env())));
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
            // A resume launched with only the id gains the transcript path.
            (
                Starting,
                serde_json::json!({"conversation_id": "id-1"}),
                "SessionStart",
                started("id-1", "resume"),
                to(Idle).engine_data(first.clone()),
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
            adapter().on_hook(&session, &hook("SessionEnd", json!({"reason": "clear"}))),
            LlmEngineChange::NONE
        );
        let change = adapter().on_hook(
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
        assert_eq!(adapter().ext_key(), "claude");
        assert_eq!(adapter().ext_key(), EXT_KEY);
    }

    #[test]
    fn id_is_claude() {
        assert_eq!(adapter().id(), "claude");
    }

    /// The adapter under test, with a fixed Claude home.
    fn adapter() -> ClaudeAdapter {
        ClaudeAdapter::new("/h")
    }

    /// An agent record in `cwd` with this engine's data `data` (absent when `Null`).
    fn record(cwd: &Path, data: serde_json::Value) -> Session {
        Session {
            cwd: cwd.to_owned(),
            ..session(LlmEngineState::Starting, data)
        }
    }

    /// What every launch ends with when no option is set.
    const TAIL: [&str; 4] = [
        "--effort",
        "high",
        "--no-chrome",
        "--dangerously-skip-permissions",
    ];

    fn with_tail(head: &[&str]) -> Vec<String> {
        argv(&[head, &TAIL[..]].concat())
    }

    #[test]
    fn start_names_the_conversation_after_the_session() {
        let (dir, profile) = profile("", &[]);
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        let session = record(&cwd, serde_json::Value::Null);
        let id = session.id.to_string();
        let plan = adapter()
            .launch(&LaunchRequest::new(&session, &profile))
            .unwrap();
        assert_eq!(plan.program, "claude");
        assert_eq!(plan.args, with_tail(&["--session-id", &id]));
        assert!(plan.env.is_empty());
        assert!(plan.workspace.is_empty());
        let transcript = format!("/h/projects/{}/{id}.jsonl", munge(&cwd.to_string_lossy()));
        assert_eq!(
            plan.engine_data,
            Some(serde_json::json!({"conversation_id": id, "transcript_path": transcript}))
        );
    }

    #[test]
    fn transcript_path_uses_the_resolved_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap();
        std::os::unix::fs::symlink(&real, real.join("link")).unwrap();
        assert_eq!(
            adapter().transcript_path(&real.join("link"), "abc"),
            PathBuf::from(format!(
                "/h/projects/{}/abc.jsonl",
                munge(&real.to_string_lossy())
            ))
        );
        // A missing tail is kept as it is.
        assert_eq!(
            adapter().transcript_path(Path::new("/nonexistent/a.b"), "abc"),
            PathBuf::from("/h/projects/-nonexistent-a-b/abc.jsonl")
        );
    }

    #[test]
    fn resume_continues_the_recorded_conversation_else_the_session_id() {
        let (dir, profile) = profile("role", &[]);
        let session = record(
            dir.path(),
            serde_json::json!({"conversation_id": "chat-42", "transcript_path": "/x"}),
        );
        let plan = adapter()
            .launch(
                &LaunchRequest::new(&session, &profile)
                    .mode(LaunchMode::Resume)
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
        assert_eq!(plan.engine_data, None, "a resume keeps the data as it is");
        // No data yet (or hand-edited junk): the conversation is the one named after the session.
        for data in [serde_json::Value::Null, serde_json::json!("junk")] {
            let session = record(dir.path(), data);
            let plan = adapter()
                .launch(&LaunchRequest::new(&session, &profile).mode(LaunchMode::Resume))
                .unwrap();
            assert_eq!(plan.args[..2], argv(&["--resume", session.id.as_str()]));
            assert_eq!(plan.engine_data, None);
        }
    }

    #[test]
    fn fork_branches_the_source_into_the_session_id() {
        let (dir, profile) = profile("", &[]);
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        let source = record(&cwd, serde_json::json!({"conversation_id": "after-clear"}));
        let session = record(&cwd, serde_json::Value::Null);
        let id = session.id.to_string();
        let plan = adapter()
            .launch(
                &LaunchRequest::new(&session, &profile)
                    .mode(LaunchMode::Fork { source: &source })
                    .initial_prompt("go"),
            )
            .unwrap();
        let mut expected = with_tail(&[
            "--resume",
            "after-clear",
            "--fork-session",
            "--session-id",
            &id,
        ]);
        expected.push("go".into());
        assert_eq!(plan.args, expected);
        assert_eq!(
            plan.engine_data,
            Some(serde_json::json!({
                "conversation_id": id,
                "transcript_path": format!("/h/projects/{}/{id}.jsonl", munge(&cwd.to_string_lossy())),
            }))
        );
        // A source without data: its conversation is the one named after it.
        let source = record(&cwd, serde_json::Value::Null);
        let plan = adapter()
            .launch(
                &LaunchRequest::new(&session, &profile).mode(LaunchMode::Fork { source: &source }),
            )
            .unwrap();
        assert_eq!(plan.args[..2], argv(&["--resume", source.id.as_str()]));
    }

    #[test]
    fn every_option() {
        let (dir, profile) = profile("# Role\nbe good", &["/skills/one", "/other/two"]);
        let session = record(dir.path(), serde_json::Value::Null);
        let plan = adapter()
            .launch(
                &LaunchRequest::new(&session, &profile)
                    .model("opus")
                    .effort(Effort::Medium)
                    .initial_prompt("go"),
            )
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "--session-id",
                session.id.as_str(),
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
        let session = record(dir.path(), serde_json::Value::Null);
        let plan = adapter()
            .launch(
                &LaunchRequest::new(&session, &profile)
                    .model("")
                    .effort(Effort::Max)
                    .initial_prompt(""),
            )
            .unwrap();
        assert_eq!(
            plan.args,
            argv(&[
                "--session-id",
                session.id.as_str(),
                "--effort",
                "max",
                "--no-chrome",
                "--dangerously-skip-permissions"
            ])
        );
    }

    #[test]
    fn hook_command_wires_every_event_through_settings() {
        let (dir, profile) = profile("", &[]);
        let session = record(dir.path(), serde_json::Value::Null);
        let hook_command = ["/x y/txc".to_owned(), "chat".to_owned(), "hook".to_owned()];
        let plan = adapter()
            .launch(&LaunchRequest::new(&session, &profile).hook_command(&hook_command))
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
    fn munge_paths() {
        assert_eq!(munge("/a/b.c/.d"), "-a-b-c--d");
        assert_eq!(munge("/nope"), "-nope");
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
