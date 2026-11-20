//! The engine protocol on the wire, plugin api 1: what core writes to an engine that runs as
//! its own program, and what the engine answers. One request per line on the engine's standard
//! input, one response per line on its standard output, in order:
//!
//! ```text
//! -> {"method": "start", "params": {"request": {…}, "prompt": "hi"}}
//! <- {"ok": {"program": "claude", "args": ["--session-id", "…"]}}
//! <- {"error": {"kind": "refused", "message": "claude options: model must not be empty"}}
//! ```
//!
//! | method | params | ok |
//! |---|---|---|
//! | `configure` | `session`, `options` | the new engine data |
//! | `start` | `request`, `prompt` | a [`LaunchPlan`](super::LaunchPlan) |
//! | `resume` | `request` | a launch plan |
//! | `fork` | `request`, `source`, `prompt` | a launch plan |
//! | `on_hook` | `session`, `hook` | an [`LlmEngineChange`](crate::LlmEngineChange) |
//! | `prepare` | `session` | `null` |
//! | `archive` | `session`, `archive_directory` | `null` |
//! | `restore` | `session`, `archive_directory` | `true` or `false` |
//! | `receive_message` | `sender`, `text` | an [`Input`] |
//! | `interrupt` | `session` | `null`, or an [`Interrupted`] |
//! | `warnings` | `agents`, `skills`, `cache` ([`Definitions`]) | a list of [`Finding`](super::Finding)s |
//! | `check` | `agents`, `skills`, `cache` | an [`EngineCheck`](super::EngineCheck) |
//! | `tools` | `cache`, `mcp_servers` | a [`ToolsReport`] |
//!
//! Every shape is what serde writes for the type core already has. The two types that only
//! borrow what they name ([`LaunchRequest`], [`EnvelopeSender`]) have a twin here that owns it,
//! for the side that reads. A reader ignores a key it does not know.

use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use super::{AgentSummary, DefinitionsRequest, EngineError, EngineTools, Input, LaunchRequest};
use crate::agent::{Agent, RecordOut};
use crate::harness::McpServer;
use crate::messaging::EnvelopeSender;
use crate::profile::{Profile, SkillProvider};
use crate::state::{LlmEngineHook, LlmEngineState};

/// One request: `{"method": …, "params": {…}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Call {
    Configure {
        session: Record,
        options: Value,
    },
    Start {
        request: Box<Launch>,
        prompt: String,
    },
    Resume {
        request: Box<Launch>,
    },
    Fork {
        request: Box<Launch>,
        source: Record,
        prompt: String,
    },
    OnHook {
        session: Record,
        hook: LlmEngineHook,
    },
    Prepare {
        session: Record,
    },
    Archive {
        session: Record,
        archive_directory: PathBuf,
    },
    Restore {
        session: Record,
        archive_directory: PathBuf,
    },
    ReceiveMessage {
        sender: Sender,
        text: String,
    },
    Interrupt {
        session: Record,
    },
    Warnings(Definitions),
    Check(Definitions),
    Tools {
        cache: PathBuf,
        mcp_servers: BTreeMap<String, McpServer>,
    },
}

impl Call {
    /// The method, as the request spells it.
    pub fn method(&self) -> &'static str {
        match self {
            Call::Configure { .. } => "configure",
            Call::Start { .. } => "start",
            Call::Resume { .. } => "resume",
            Call::Fork { .. } => "fork",
            Call::OnHook { .. } => "on_hook",
            Call::Prepare { .. } => "prepare",
            Call::Archive { .. } => "archive",
            Call::Restore { .. } => "restore",
            Call::ReceiveMessage { .. } => "receive_message",
            Call::Interrupt { .. } => "interrupt",
            Call::Warnings(_) => "warnings",
            Call::Check(_) => "check",
            Call::Tools { .. } => "tools",
        }
    }
}

/// One response: `{"ok": …}` or `{"error": {…}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Reply<T> {
    Ok(T),
    Error(Failure),
}

impl<T> From<Result<T, EngineError>> for Reply<T> {
    fn from(result: Result<T, EngineError>) -> Self {
        match result {
            Ok(value) => Reply::Ok(value),
            Err(error) => Reply::Error(Failure::from(&error)),
        }
    }
}

impl<T> From<Reply<T>> for Result<T, EngineError> {
    fn from(reply: Reply<T>) -> Self {
        match reply {
            Reply::Ok(value) => Ok(value),
            Reply::Error(failure) => Err(failure.into()),
        }
    }
}

/// An [`EngineError`] on the wire: `{"kind", "message", "path"}`, where `kind` is `refused`,
/// `workspace` or `io`, and the last two name the `path` they are about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Failure {
    Refused { message: String },
    Workspace { message: String, path: PathBuf },
    Io { message: String, path: PathBuf },
}

impl From<&EngineError> for Failure {
    fn from(error: &EngineError) -> Self {
        match error {
            EngineError::Refused(message) => Failure::Refused {
                message: message.clone(),
            },
            EngineError::Workspace { path, reason } => Failure::Workspace {
                message: reason.clone(),
                path: path.clone(),
            },
            EngineError::Io { path, source } => Failure::Io {
                message: source.to_string(),
                path: path.clone(),
            },
        }
    }
}

/// The error core raises for a failure an engine answered: the same variant, and for `io` the
/// message as its source, so it reads as it did in the engine.
impl From<Failure> for EngineError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Refused { message } => EngineError::Refused(message),
            Failure::Workspace { message, path } => EngineError::Workspace {
                path,
                reason: message,
            },
            Failure::Io { message, path } => EngineError::Io {
                path,
                source: io::Error::other(message),
            },
        }
    }
}

/// An agent record as `tx show` prints it: `version` first, then the record's own keys. Read
/// back as a record is read from disk, the version check included.
#[derive(Clone, Debug, PartialEq)]
pub struct Record(pub Agent);

impl Serialize for Record {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        RecordOut::new(&self.0).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Record {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = Map::<String, Value>::deserialize(deserializer)?;
        Agent::from_map(record)
            .map(Record)
            .map_err(serde::de::Error::custom)
    }
}

/// A [`LaunchRequest`] that owns what it names: `{"session", "profile", "hook_command"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Launch {
    pub session: Record,
    pub profile: Profile,
    pub hook_command: Vec<String>,
}

impl Launch {
    /// The request, as an engine's launch methods take it.
    pub fn request(&self) -> LaunchRequest<'_> {
        LaunchRequest::new(&self.session.0, &self.profile).hook_command(&self.hook_command)
    }
}

impl From<&LaunchRequest<'_>> for Launch {
    fn from(request: &LaunchRequest<'_>) -> Self {
        Self {
            session: Record(request.session.clone()),
            profile: request.profile.clone(),
            hook_command: request.hook_command.to_vec(),
        }
    }
}

/// An [`EnvelopeSender`] that owns its strings: `{"user": {"via": …}}` or
/// `{"agent": {"name": …}}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sender {
    User { via: Option<String> },
    Agent { name: String },
}

impl Sender {
    /// The sender, as an engine's `receive_message` takes it.
    pub fn envelope(&self) -> EnvelopeSender<'_> {
        match self {
            Sender::User { via } => EnvelopeSender::User {
                via: via.as_deref(),
            },
            Sender::Agent { name } => EnvelopeSender::Agent { name },
        }
    }
}

impl From<&EnvelopeSender<'_>> for Sender {
    fn from(sender: &EnvelopeSender<'_>) -> Self {
        match *sender {
            EnvelopeSender::User { via } => Sender::User {
                via: via.map(str::to_owned),
            },
            EnvelopeSender::Agent { name } => Sender::Agent {
                name: name.to_owned(),
            },
        }
    }
}

/// What an engine that can abort a turn answers to `interrupt`: what to type, and the state the
/// agent is in once it landed (`null`: say nothing, a hook will).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interrupted {
    pub input: Input,
    #[serde(default)]
    pub state: Option<LlmEngineState>,
}

impl From<(Input, Option<LlmEngineState>)> for Interrupted {
    fn from((input, state): (Input, Option<LlmEngineState>)) -> Self {
        Self { input, state }
    }
}

impl From<Interrupted> for (Input, Option<LlmEngineState>) {
    fn from(interrupted: Interrupted) -> Self {
        (interrupted.input, interrupted.state)
    }
}

/// A [`DefinitionsRequest`] that owns what it names, the params of `warnings` and of `check`:
/// `{"agents", "skills", "cache"}`, `skills` being every provided skill by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Definitions {
    pub agents: Vec<AgentSummary>,
    pub skills: BTreeMap<String, SkillProvider>,
    pub cache: PathBuf,
}

impl Definitions {
    /// The request, as an engine's `warnings` and `check` take it.
    pub fn request(&self) -> DefinitionsRequest<'_> {
        DefinitionsRequest {
            agents: &self.agents,
            provided_skills: &self.skills,
            cache: &self.cache,
        }
    }
}

impl From<&DefinitionsRequest<'_>> for Definitions {
    fn from(request: &DefinitionsRequest<'_>) -> Self {
        Self {
            agents: request.agents.to_vec(),
            skills: request.provided_skills.clone(),
            cache: request.cache.to_owned(),
        }
    }
}

/// One of the two levels of a tools report's `mcp` on the wire: `{"ok": …}` or
/// `{"error": "<why>"}`, lowercase as every answer's envelope is, for engines in any language.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Listed<T> {
    Ok(T),
    Error(String),
}

impl<T> From<Result<T, String>> for Listed<T> {
    fn from(result: Result<T, String>) -> Self {
        match result {
            Ok(value) => Listed::Ok(value),
            Err(why) => Listed::Error(why),
        }
    }
}

impl<T> From<Listed<T>> for Result<T, String> {
    fn from(listed: Listed<T>) -> Self {
        match listed {
            Listed::Ok(value) => Ok(value),
            Listed::Error(why) => Err(why),
        }
    }
}

/// The MCP servers' tools as a tools report carries them: the whole listing, or why it failed;
/// per server its tools as (name, description), or why there are none.
pub type McpListing = Listed<BTreeMap<String, Listed<Vec<(String, String)>>>>;

/// [`EngineTools`] as the answer to `tools`: `{"heading", "rows": [[name, note]], "footer",
/// "mcp": null | {"ok": {<server>: {"ok": [[tool, description]]} | {"error": why}}} |
/// {"error": why}}`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsReport {
    #[serde(default)]
    pub heading: String,
    #[serde(default)]
    pub rows: Vec<(String, String)>,
    #[serde(default)]
    pub footer: Vec<String>,
    #[serde(default)]
    pub mcp: Option<McpListing>,
}

impl From<EngineTools> for ToolsReport {
    fn from(tools: EngineTools) -> Self {
        Self {
            heading: tools.heading,
            rows: tools.rows,
            footer: tools.footer,
            mcp: tools.mcp.map(|listing| {
                Listed::from(listing.map(|servers| {
                    servers
                        .into_iter()
                        .map(|(server, tools)| (server, Listed::from(tools)))
                        .collect()
                }))
            }),
        }
    }
}

impl From<ToolsReport> for EngineTools {
    fn from(tools: ToolsReport) -> Self {
        Self {
            heading: tools.heading,
            rows: tools.rows,
            footer: tools.footer,
            mcp: tools.mcp.map(|listing| {
                Result::from(listing).map(|servers| {
                    servers
                        .into_iter()
                        .map(|(server, tools)| (server, Result::from(tools)))
                        .collect()
                })
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineCheck, Finding, Key, LaunchPlan, WorkspaceAction};
    use crate::state::{IdleReason, LlmEngineChange};
    use serde_json::json;

    const SESSION: &str = r#"{"version":2,"id":"a1","name":"worker","engine":"claude","working_directory":"/w","roles":["dev"],"state":"starting","created_at":1.5,"last_modified_at":2.5,"last_activity_at":null,"tags":["t"],"parent_id":null,"ext":{"claude":{"model":"opus"}}}"#;
    const PROFILE: &str = r#"{"text":"be kind","file":"/s/profiles/a1.md","skill_dirs":["/skills/one"],"tools":null,"disallowed_tools":["Bash"],"mcp_servers":{"docs":{"type":"stdio","command":"docs-mcp"}},"provided_skills":[["pdf","claude-bundled"]]}"#;

    fn session() -> Record {
        serde_json::from_str(SESSION).unwrap()
    }

    fn launch() -> Launch {
        Launch {
            session: session(),
            profile: serde_json::from_str(PROFILE).unwrap(),
            hook_command: vec!["/bin/txi".to_owned(), "hook".to_owned()],
        }
    }

    fn written<T: Serialize>(value: &T) -> String {
        serde_json::to_string(value).unwrap()
    }

    /// Every request there is, one of each.
    fn calls() -> Vec<Call> {
        vec![
            Call::Configure {
                session: session(),
                options: json!({"model": "fable"}),
            },
            Call::Start {
                request: Box::new(launch()),
                prompt: "hi".to_owned(),
            },
            Call::Resume {
                request: Box::new(launch()),
            },
            Call::Fork {
                request: Box::new(launch()),
                source: session(),
                prompt: "branch".to_owned(),
            },
            Call::OnHook {
                session: session(),
                hook: LlmEngineHook {
                    name: "Stop".to_owned(),
                    payload: json!({"session_id": "c"}),
                    at: 3.5,
                },
            },
            Call::Prepare { session: session() },
            Call::Archive {
                session: session(),
                archive_directory: "/d/agents/a1/archive".into(),
            },
            Call::Restore {
                session: session(),
                archive_directory: "/d/agents/a1/archive".into(),
            },
            Call::ReceiveMessage {
                sender: Sender::Agent {
                    name: "lead".to_owned(),
                },
                text: "status?".to_owned(),
            },
            Call::Interrupt { session: session() },
            Call::Warnings(definitions()),
            Call::Check(definitions()),
            Call::Tools {
                cache: "/c".into(),
                mcp_servers: serde_json::from_value(
                    json!({"docs": {"type": "stdio", "command": "docs-mcp"}}),
                )
                .unwrap(),
            },
        ]
    }

    fn definitions() -> Definitions {
        Definitions {
            agents: vec![AgentSummary {
                name: "mine".to_owned(),
                path: "/c/definitions/agents/mine.md".into(),
                tools: vec!["Read".to_owned()],
                disallowed_tools: Vec::new(),
                skills: vec!["pdf".to_owned()],
                lines: [("skills".to_owned(), 5)].into_iter().collect(),
            }],
            skills: [("pdf".to_owned(), SkillProvider::ClaudeBundled)]
                .into_iter()
                .collect(),
            cache: "/c".into(),
        }
    }

    #[test]
    fn a_record_is_written_as_tx_show_prints_it_and_read_back_whole() {
        let record = session();
        assert_eq!(record.0.name, "worker");
        assert_eq!(record.0.ext["claude"], json!({"model": "opus"}));
        assert!(
            record.0.other.is_empty(),
            "the version is not a leftover key"
        );
        assert_eq!(written(&record), SESSION);
        let pretty: Value = serde_json::from_str(&record.0.to_json().unwrap()).unwrap();
        assert_eq!(serde_json::to_value(&record).unwrap(), pretty);
    }

    #[test]
    fn a_record_of_another_version_is_refused_as_it_is_on_disk() {
        let error =
            serde_json::from_str::<Record>(&SESSION.replace("\"version\":2", "\"version\":3"))
                .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("record version 3 is not supported"),
            "{error}"
        );
        assert!(serde_json::from_str::<Record>(&SESSION.replace("\"version\":2,", "")).is_err());
    }

    #[test]
    fn a_request_is_a_method_and_its_params() {
        let start = Call::Start {
            request: Box::new(launch()),
            prompt: "hi".to_owned(),
        };
        assert_eq!(
            written(&start),
            format!(
                r#"{{"method":"start","params":{{"request":{{"session":{SESSION},"profile":{PROFILE},"hook_command":["/bin/txi","hook"]}},"prompt":"hi"}}}}"#
            )
        );
        assert_eq!(
            written(&Call::Configure {
                session: session(),
                options: json!({"model": "fable"}),
            }),
            format!(
                r#"{{"method":"configure","params":{{"session":{SESSION},"options":{{"model":"fable"}}}}}}"#
            )
        );
        assert_eq!(
            written(&Call::Archive {
                session: session(),
                archive_directory: "/d/archive".into(),
            }),
            format!(
                r#"{{"method":"archive","params":{{"session":{SESSION},"archive_directory":"/d/archive"}}}}"#
            )
        );
        assert_eq!(
            written(&Call::ReceiveMessage {
                sender: Sender::User { via: None },
                text: "hi".to_owned(),
            }),
            r#"{"method":"receive_message","params":{"sender":{"user":{"via":null}},"text":"hi"}}"#
        );
    }

    #[test]
    fn every_request_names_its_method_and_reads_back_as_it_was_written() {
        let calls = calls();
        let methods: Vec<&str> = calls.iter().map(Call::method).collect();
        assert_eq!(
            methods,
            [
                "configure",
                "start",
                "resume",
                "fork",
                "on_hook",
                "prepare",
                "archive",
                "restore",
                "receive_message",
                "interrupt",
                "warnings",
                "check",
                "tools"
            ]
        );
        for call in calls {
            let text = written(&call);
            assert!(!text.contains('\n'), "{text}");
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["method"], call.method());
            assert_eq!(serde_json::from_str::<Call>(&text).unwrap(), call);
        }
    }

    #[test]
    fn a_request_reads_with_a_key_it_does_not_know_and_in_any_order() {
        let text =
            format!(r#"{{"params":{{"later":1,"session":{SESSION}}},"method":"prepare","id":7}}"#);
        assert_eq!(
            serde_json::from_str::<Call>(&text).unwrap(),
            Call::Prepare { session: session() }
        );
        let error = serde_json::from_str::<Call>(r#"{"method":"reload","params":{}}"#).unwrap_err();
        assert!(
            error.to_string().starts_with("unknown variant `reload`"),
            "{error}"
        );
    }

    #[test]
    fn a_launch_is_the_request_an_engine_takes() {
        let launch = launch();
        let request = launch.request();
        assert_eq!(request.session, &launch.session.0);
        assert_eq!(request.profile.text(), "be kind");
        assert_eq!(
            request.profile.file(),
            std::path::Path::new("/s/profiles/a1.md")
        );
        assert_eq!(
            request.profile.provided_skills(),
            [(
                "pdf".to_owned(),
                crate::profile::SkillProvider::ClaudeBundled
            )]
        );
        assert_eq!(request.hook_command, ["/bin/txi", "hook"]);
        assert_eq!(Launch::from(&request), launch);
    }

    #[test]
    fn a_plan_is_written_whole_and_read_from_its_program_alone() {
        let plan = LaunchPlan::new("claude")
            .args(["--session-id", "x"])
            .env("K", "V")
            .workspace(WorkspaceAction::Write {
                path: "a".into(),
                content: "b".into(),
            })
            .workspace(WorkspaceAction::Symlink {
                src: "s".into(),
                dst: "d".into(),
            })
            .engine_data(json!({"k": 1}));
        let text = r#"{"program":"claude","args":["--session-id","x"],"env":[["K","V"]],"workspace":[{"write":{"path":"a","content":"b"}},{"symlink":{"src":"s","dst":"d"}}],"engine_data":{"k":1}}"#;
        assert_eq!(written(&plan), text);
        assert_eq!(serde_json::from_str::<LaunchPlan>(text).unwrap(), plan);
        assert_eq!(
            written(&LaunchPlan::new("sleep")),
            r#"{"program":"sleep","args":[],"env":[],"workspace":[],"engine_data":null}"#
        );
        assert_eq!(
            serde_json::from_str::<LaunchPlan>(r#"{"program": "sleep", "args": ["600"]}"#).unwrap(),
            LaunchPlan::new("sleep").arg("600")
        );
        assert!(serde_json::from_str::<LaunchPlan>(r#"{"args": ["600"]}"#).is_err());
    }

    #[test]
    fn a_hook_and_the_change_it_makes() {
        let hook = LlmEngineHook {
            name: "Stop".to_owned(),
            payload: json!({}),
            at: 1.5,
        };
        let text = r#"{"name":"Stop","payload":{},"at":1.5}"#;
        assert_eq!(written(&hook), text);
        assert_eq!(serde_json::from_str::<LlmEngineHook>(text).unwrap(), hook);
        assert_eq!(
            written(&LlmEngineChange::idle(IdleReason::Finished)),
            r#"{"state":"idle","idle_reason":"finished","engine_data":null}"#
        );
        assert_eq!(
            written(&LlmEngineChange::NONE.engine_data(json!({"k": 1}))),
            r#"{"state":null,"idle_reason":null,"engine_data":{"k":1}}"#
        );
        assert_eq!(
            serde_json::from_str::<LlmEngineChange>("{}").unwrap(),
            LlmEngineChange::NONE
        );
        assert_eq!(
            serde_json::from_str::<LlmEngineChange>(r#"{"state": "working"}"#).unwrap(),
            LlmEngineChange::to(LlmEngineState::Working)
        );
        assert!(serde_json::from_str::<LlmEngineChange>(r#"{"state": "busy"}"#).is_err());
    }

    #[test]
    fn an_input_is_a_line_of_text_or_a_key_by_its_tmux_name() {
        assert_eq!(
            written(&Input::line("hi")),
            r#"{"text":{"text":"hi","submit":true}}"#
        );
        for key in [Key::Escape, Key::Enter, Key::ControlC, Key::ControlD] {
            let text = written(&Input::Key(key));
            assert_eq!(text, format!(r#"{{"key":"{}"}}"#, key.tmux_name()));
            assert_eq!(
                serde_json::from_str::<Input>(&text).unwrap(),
                Input::Key(key)
            );
        }
        assert!(serde_json::from_str::<Input>(r#"{"key": "escape"}"#).is_err());
        let interrupted = Interrupted::from((Input::Key(Key::Escape), Some(LlmEngineState::Idle)));
        assert_eq!(
            written(&interrupted),
            r#"{"input":{"key":"Escape"},"state":"idle"}"#
        );
        assert_eq!(
            serde_json::from_str::<Option<Interrupted>>(r#"{"input": {"key": "C-c"}}"#).unwrap(),
            Some(Interrupted {
                input: Input::Key(Key::ControlC),
                state: None
            })
        );
        assert_eq!(
            serde_json::from_str::<Option<Interrupted>>("null").unwrap(),
            None
        );
    }

    #[test]
    fn a_sender_is_the_user_or_an_agent() {
        for (sender, text) in [
            (
                EnvelopeSender::User { via: None },
                r#"{"user":{"via":null}}"#,
            ),
            (
                EnvelopeSender::User {
                    via: Some("plan.md"),
                },
                r#"{"user":{"via":"plan.md"}}"#,
            ),
            (
                EnvelopeSender::Agent { name: "lead" },
                r#"{"agent":{"name":"lead"}}"#,
            ),
        ] {
            let owned = Sender::from(&sender);
            assert_eq!(written(&owned), text);
            let read: Sender = serde_json::from_str(text).unwrap();
            assert_eq!(read.envelope(), sender);
        }
        assert_eq!(
            serde_json::from_str::<Sender>(r#"{"user": {}}"#).unwrap(),
            Sender::User { via: None }
        );
    }

    #[test]
    fn a_response_is_ok_or_an_error_of_one_of_three_kinds() {
        assert_eq!(written(&Reply::Ok(())), r#"{"ok":null}"#);
        assert_eq!(written(&Reply::Ok(true)), r#"{"ok":true}"#);
        assert_eq!(
            serde_json::from_str::<Reply<()>>(r#"{"ok": null}"#).unwrap(),
            Reply::Ok(())
        );
        for (error, text, shown) in [
            (
                EngineError::Refused("no effort support".to_owned()),
                r#"{"error":{"kind":"refused","message":"no effort support"}}"#,
                "no effort support",
            ),
            (
                EngineError::Workspace {
                    path: "/w/x".into(),
                    reason: "escapes the working directory".to_owned(),
                },
                r#"{"error":{"kind":"workspace","message":"escapes the working directory","path":"/w/x"}}"#,
                "workspace action on /w/x: escapes the working directory",
            ),
            (
                EngineError::Io {
                    path: "/a/t.jsonl".into(),
                    source: io::Error::other("disk full"),
                },
                r#"{"error":{"kind":"io","message":"disk full","path":"/a/t.jsonl"}}"#,
                "/a/t.jsonl: disk full",
            ),
        ] {
            assert_eq!(error.to_string(), shown);
            let reply: Reply<()> = Err(error).into();
            assert_eq!(written(&reply), text);
            let read: Result<(), EngineError> =
                serde_json::from_str::<Reply<()>>(text).unwrap().into();
            assert_eq!(read.unwrap_err().to_string(), shown);
        }
        // The path is part of the two kinds that are about a file.
        assert!(
            serde_json::from_str::<Reply<()>>(r#"{"error": {"kind": "io", "message": "m"}}"#)
                .is_err()
        );
        for not_a_response in [
            "oops",
            "{}",
            r#"{"ok": 1}"#,
            r#"{"error": {"kind": "odd"}}"#,
        ] {
            assert!(
                serde_json::from_str::<Reply<()>>(not_a_response).is_err(),
                "{not_a_response}"
            );
        }
    }

    #[test]
    fn warnings_and_check_carry_the_agents_the_provided_skills_and_the_cache() {
        let params = r#"{"agents":[{"name":"mine","path":"/c/definitions/agents/mine.md","tools":["Read"],"disallowed_tools":[],"skills":["pdf"],"lines":{"skills":5}}],"skills":{"pdf":"claude-bundled"},"cache":"/c"}"#;
        assert_eq!(
            written(&Call::Warnings(definitions())),
            format!(r#"{{"method":"warnings","params":{params}}}"#)
        );
        assert_eq!(
            written(&Call::Check(definitions())),
            format!(r#"{{"method":"check","params":{params}}}"#)
        );
        let request = definitions();
        assert_eq!(Definitions::from(&request.request()), request);
    }

    #[test]
    fn findings_and_a_check_are_written_as_their_fields() {
        let check = EngineCheck {
            always_present: vec!["a baseline".to_owned()],
            problems: vec![Finding {
                path: "/a.md".into(),
                line: Some(4),
                message: "not a tool".to_owned(),
            }],
            notes: vec![Finding {
                path: "/a.md".into(),
                line: None,
                message: "gets more".to_owned(),
            }],
        };
        let text = r#"{"ok":{"always_present":["a baseline"],"problems":[{"path":"/a.md","line":4,"message":"not a tool"}],"notes":[{"path":"/a.md","line":null,"message":"gets more"}]}}"#;
        assert_eq!(written(&Reply::Ok(check.clone())), text);
        assert_eq!(
            serde_json::from_str::<Reply<EngineCheck>>(text).unwrap(),
            Reply::Ok(check)
        );
    }

    #[test]
    fn both_levels_of_a_tools_reports_mcp_are_ok_or_error_in_lowercase() {
        let listed = EngineTools {
            heading: "codex:".to_owned(),
            rows: vec![("Bash".to_owned(), "--disable shell_tool".to_owned())],
            footer: Vec::new(),
            mcp: Some(Ok([
                (
                    "docs".to_owned(),
                    Ok(vec![("mcp__docs__find".to_owned(), "Finds.".to_owned())]),
                ),
                ("linear".to_owned(), Err("auth required".to_owned())),
            ]
            .into_iter()
            .collect())),
        };
        let text = r#"{"heading":"codex:","rows":[["Bash","--disable shell_tool"]],"footer":[],"mcp":{"ok":{"docs":{"ok":[["mcp__docs__find","Finds."]]},"linear":{"error":"auth required"}}}}"#;
        assert_eq!(written(&ToolsReport::from(listed.clone())), text);
        let read: ToolsReport = serde_json::from_str(text).unwrap();
        assert_eq!(EngineTools::from(read), listed);

        let failed = EngineTools {
            mcp: Some(Err("codex app-server: no answer".to_owned())),
            ..EngineTools::default()
        };
        let text =
            r#"{"heading":"","rows":[],"footer":[],"mcp":{"error":"codex app-server: no answer"}}"#;
        assert_eq!(written(&ToolsReport::from(failed.clone())), text);
        let read: ToolsReport = serde_json::from_str(text).unwrap();
        assert_eq!(EngineTools::from(read), failed);

        let none: ToolsReport = serde_json::from_str(r#"{"heading":"claude:"}"#).unwrap();
        assert_eq!(
            EngineTools::from(none),
            EngineTools {
                heading: "claude:".to_owned(),
                ..EngineTools::default()
            }
        );
    }
}
