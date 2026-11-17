//! Engines in a plugin program: serving the engine a hosted plugin registers to core, over
//! standard input and output (`--tx-engine`), and the hook entry, what the hooks an engine
//! wires run (`--tx-hook <Event>` in the engine's own program, `txi hook <Event>` otherwise).

use std::ffi::OsString;
use std::io::{BufRead, Read, Write};

use serde::Serialize;
use tx_core::core::SESSION_ID_ENV;
use tx_core::engine::wire::{Call, Failure as Refusal, Interrupted, Reply};
use tx_core::{AgentId, CoreError, LlmEngine, LlmEngineHook, LlmEngineState, Plugin, TxCore};

use crate::args::Global;
use crate::setup::{Failure, Installation, process_environment};

/// Answers the engine protocol for `engine` (see [`tx_core::engine::wire`]): one request per
/// line of `requests`, one response per line of `responses`, in order, until the requests end.
/// A line that is not a request, or that names a method this server does not have, is answered
/// with a refusal: the server never stops over what it is asked, only when it can no longer
/// read or answer.
pub fn serve(
    engine: &dyn LlmEngine,
    requests: &mut dyn BufRead,
    responses: &mut dyn Write,
) -> std::io::Result<()> {
    let mut request = String::new();
    loop {
        request.clear();
        if requests.read_line(&mut request)? == 0 {
            return Ok(());
        }
        let mut response = answer(engine, &request);
        response.push('\n');
        responses.write_all(response.as_bytes())?;
        responses.flush()?;
    }
}

/// The response to one request line, without its newline.
fn answer(engine: &dyn LlmEngine, request: &str) -> String {
    let call: Call = match serde_json::from_str(request) {
        Ok(call) => call,
        Err(error) => {
            return written(&Reply::<()>::Error(Refusal::Refused {
                message: format!("not a request: {error}"),
            }));
        }
    };
    match call {
        Call::Configure { session, options } => {
            written(&Reply::from(engine.configure(&session.0, &options)))
        }
        Call::Start { request, prompt } => {
            written(&Reply::from(engine.start(&request.request(), &prompt)))
        }
        Call::Resume { request } => written(&Reply::from(engine.resume(&request.request()))),
        Call::Fork {
            request,
            source,
            prompt,
        } => written(&Reply::from(engine.fork(
            &request.request(),
            &source.0,
            &prompt,
        ))),
        Call::OnHook { session, hook } => written(&Reply::Ok(engine.on_hook(&session.0, &hook))),
        Call::Prepare { session } => written(&Reply::from(engine.prepare(&session.0))),
        Call::Archive {
            session,
            archive_directory,
        } => written(&Reply::from(engine.archive(&session.0, &archive_directory))),
        Call::Restore {
            session,
            archive_directory,
        } => written(&Reply::from(engine.restore(&session.0, &archive_directory))),
        Call::ReceiveMessage { sender, text } => written(&Reply::Ok(
            engine.receive_message(&sender.envelope(), &text),
        )),
        Call::Interrupt { session } => written(&Reply::Ok(
            engine.interrupt(&session.0).map(Interrupted::from),
        )),
    }
}

fn written<T: Serialize>(reply: &Reply<T>) -> String {
    serde_json::to_string(reply).expect("a response is plain data")
}

/// `--tx-engine`: serves the engine an active hosted plugin registers, on the process's
/// standard input and output. The plugins are registered as they are for a verb (the active
/// ones, each with the settings `config.yaml` gives its id), and no core is opened.
pub(crate) fn serve_hosted(
    program: &str,
    global: &Global,
    hosted: &[Box<dyn Plugin>],
) -> Result<u8, Failure> {
    let Some(registry) = Installation::read(global)?.engine_registry(hosted)? else {
        eprintln!("{program}: no hosted plugin registers an engine");
        return Ok(2);
    };
    let engine = registry.engines()[0].as_ref();
    let served = serve(
        engine,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout().lock(),
    );
    if let Err(error) = served {
        eprintln!("tx: engine {}: {error}", engine.kind());
        return Ok(1);
    }
    Ok(0)
}

/// What a hook an engine wired runs: feeds the hook `event`, with the payload on standard
/// input, to the agent `$TX_SESSION_ID` names, on the core `open` opens.
///
/// A hook must never fail the agent. One that is not tx's (no `$TX_SESSION_ID`, or not an id)
/// is `Ok` before anything is opened, and so is one for a record this core does not hold; the
/// caller exits 0 for those and 1, never more, for any error.
///
/// Before the session lock is taken, the engine of the agent is asked to start what its next
/// call needs ([`LlmEngine::warm`]): an engine that runs as its own program is then running
/// when the lock is held, and one linked into this process has nothing to start.
pub fn hook(event: String, open: impl FnOnce() -> Result<TxCore, Failure>) -> Result<(), Failure> {
    hook_in(
        &process_environment,
        event,
        &mut std::io::stdin().lock(),
        open,
    )
}

fn hook_in(
    environment: &dyn Fn(&str) -> Option<OsString>,
    event: String,
    payload: &mut dyn Read,
    open: impl FnOnce() -> Result<TxCore, Failure>,
) -> Result<(), Failure> {
    let Some(id) = environment(SESSION_ID_ENV)
        .and_then(|raw| raw.into_string().ok())
        .and_then(|raw| raw.parse::<AgentId>().ok())
    else {
        return Ok(());
    };
    let core = open()?;
    // Read without the lock: a record that ended meanwhile only costs a start. One that has
    // ended already gets none, since core returns before it asks the engine.
    if let Ok(session) = core.get_agent(&id)
        && session.state != LlmEngineState::Ended
        && let Some(engine) = core.engine(session.engine)
    {
        engine.warm();
    }
    let hook = LlmEngineHook {
        name: event,
        payload: read_payload(payload),
        at: tx_core::events::now(),
    };
    match core.hook(&id, hook) {
        Ok(_) | Err(CoreError::NotFound(_)) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// `--tx-hook <Event>`: [`hook`] on the core of a plugin program, where the engine the hosted
/// plugin registers is in the process. The program notes itself in the trace.
pub(crate) fn hook_hosted(
    global: &Global,
    hosted: Vec<Box<dyn Plugin>>,
    event: String,
) -> Result<(), Failure> {
    hook(event, || {
        let installation = Installation::read(global)?;
        installation.trace_hook(global, &hosted);
        installation.open_hosted(global, hosted)
    })
}

/// A hook's payload, tolerantly: `{}` for none, unreadable, bad JSON or a non-object. The hook
/// must never fail the agent over its own input.
fn read_payload(payload: &mut dyn Read) -> serde_json::Value {
    let mut text = String::new();
    if payload.read_to_string(&mut text).is_err() {
        return serde_json::json!({});
    }
    match serde_json::from_str(&text) {
        Ok(value @ serde_json::Value::Object(_)) => value,
        _ => serde_json::json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use tx_core::engine::wire::{Launch, Record, Sender};
    use tx_core::{
        Agent, EngineError, EngineKind, IdleReason, Input, Key, LaunchPlan, LaunchRequest,
        LlmEngineChange, PluginError, Registry, WorkspaceAction,
    };

    use crate::args::parse_global;

    /// What the engine of these tests was asked, in order.
    type Seen = Arc<Mutex<Vec<String>>>;

    /// An engine of kind `test` that implements every method, notes each call and its settings,
    /// and refuses the model `none`.
    struct TestEngine {
        seen: Seen,
        settings: Value,
    }

    impl TestEngine {
        fn note(&self, call: impl Into<String>) {
            self.seen.lock().unwrap().push(call.into());
        }
    }

    impl LlmEngine for TestEngine {
        fn kind(&self) -> EngineKind {
            "test".parse().unwrap()
        }

        fn ext_key(&self) -> &str {
            "test"
        }

        fn configure(&self, session: &Agent, options: &Value) -> Result<Value, EngineError> {
            self.note(format!("configure {}", session.name));
            if options["model"] == "none" {
                return Err(EngineError::Refused("test options: no such model".into()));
            }
            Ok(json!({"options": options, "settings": self.settings}))
        }

        fn start(
            &self,
            request: &LaunchRequest<'_>,
            prompt: &str,
        ) -> Result<LaunchPlan, EngineError> {
            self.note(format!("start {} {prompt}", request.session.name));
            Ok(LaunchPlan::new("test-cli")
                .arg(request.profile.file().display().to_string())
                .args(request.hook_command.iter().cloned())
                .env("K", "V")
                .workspace(WorkspaceAction::Write {
                    path: "note.txt".into(),
                    content: prompt.to_owned(),
                })
                .engine_data(json!({"started": true})))
        }

        fn resume(&self, request: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
            self.note(format!("resume {}", request.session.name));
            Err(EngineError::Workspace {
                path: "/w/x".into(),
                reason: "escapes the working directory".into(),
            })
        }

        fn fork(
            &self,
            request: &LaunchRequest<'_>,
            source: &Agent,
            prompt: &str,
        ) -> Result<LaunchPlan, EngineError> {
            self.note(format!(
                "fork {} of {} {prompt}",
                request.session.name, source.name
            ));
            Ok(LaunchPlan::new("test-cli").arg("--fork"))
        }

        fn receive_message(&self, sender: &tx_core::EnvelopeSender<'_>, text: &str) -> Input {
            self.note(format!("receive_message {sender:?}"));
            Input::Text {
                text: format!("[{text}]"),
                submit: false,
            }
        }

        fn interrupt(&self, session: &Agent) -> Option<(Input, Option<LlmEngineState>)> {
            self.note(format!("interrupt {}", session.name));
            (session.name != "deaf")
                .then_some((Input::Key(Key::Escape), Some(LlmEngineState::Idle)))
        }

        fn on_hook(&self, session: &Agent, hook: &LlmEngineHook) -> LlmEngineChange {
            self.note(format!("on_hook {} {}", session.name, hook.name));
            match hook.name.as_str() {
                "Stop" => LlmEngineChange::idle(IdleReason::Finished)
                    .engine_data(json!({"payload": hook.payload})),
                "UserPromptSubmit" => LlmEngineChange::to(LlmEngineState::Working),
                _ => LlmEngineChange::NONE,
            }
        }

        fn archive(&self, session: &Agent, archive_directory: &Path) -> Result<(), EngineError> {
            self.note(format!(
                "archive {} {}",
                session.name,
                archive_directory.display()
            ));
            Ok(())
        }

        fn prepare(&self, session: &Agent) -> Result<(), EngineError> {
            self.note(format!("prepare {}", session.name));
            Err(EngineError::Io {
                path: "/h/.config.json".into(),
                source: std::io::Error::other("denied"),
            })
        }

        fn restore(&self, session: &Agent, _: &Path) -> Result<bool, EngineError> {
            self.note(format!("restore {}", session.name));
            Ok(true)
        }
    }

    /// The plugin `engine-test`, which registers [`TestEngine`] with its settings.
    struct TestPlugin(Seen);

    impl Plugin for TestPlugin {
        fn id(&self) -> &'static str {
            "engine-test"
        }

        fn register(&self, registry: &mut Registry) -> Result<(), PluginError> {
            registry.engine(Box::new(TestEngine {
                seen: Arc::clone(&self.0),
                settings: registry.settings().clone(),
            }));
            Ok(())
        }
    }

    /// A plugin that registers nothing.
    struct Empty;

    impl Plugin for Empty {
        fn id(&self) -> &'static str {
            "empty"
        }

        fn register(&self, _: &mut Registry) -> Result<(), PluginError> {
            Ok(())
        }
    }

    fn engine() -> (TestEngine, Seen) {
        let seen = Seen::default();
        let engine = TestEngine {
            seen: Arc::clone(&seen),
            settings: Value::Null,
        };
        (engine, seen)
    }

    fn record(name: &str, state: &str) -> Record {
        serde_json::from_value(json!({
            "version": 2, "id": "a1", "name": name, "engine": "test", "working_directory": "/w",
            "roles": [], "state": state, "created_at": 1.0, "last_modified_at": 1.0, "tags": [],
        }))
        .unwrap()
    }

    fn launch(name: &str) -> Box<Launch> {
        Box::new(Launch {
            session: record(name, "starting"),
            profile: serde_json::from_value(json!({
                "text": "t", "file": "/s/profiles/a1.md", "skill_dirs": [], "tools": null,
                "disallowed_tools": [], "mcp_servers": {}, "provided_skills": [],
            }))
            .unwrap(),
            hook_command: vec!["/bin/txi".to_owned(), "hook".to_owned()],
        })
    }

    /// The responses of a server of `engine` to `requests`, one line each.
    fn served(engine: &dyn LlmEngine, requests: &[String]) -> Vec<String> {
        let input = requests
            .iter()
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let mut output = Vec::new();
        serve(engine, &mut input.as_bytes(), &mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.is_empty() || output.ends_with('\n'), "{output:?}");
        output.lines().map(str::to_owned).collect()
    }

    /// The response to `call`, as the side that asked reads it.
    fn asked<T: serde::de::DeserializeOwned>(
        engine: &dyn LlmEngine,
        call: &Call,
    ) -> Result<T, EngineError> {
        let responses = served(engine, &[serde_json::to_string(call).unwrap()]);
        assert_eq!(responses.len(), 1);
        serde_json::from_str::<Reply<T>>(&responses[0])
            .unwrap_or_else(|error| panic!("{}: {error}", responses[0]))
            .into()
    }

    #[test]
    fn every_method_is_answered_in_the_shape_its_caller_reads() {
        let (engine, seen) = engine();
        let configured: Value = asked(
            &engine,
            &Call::Configure {
                session: record("a", "starting"),
                options: json!({"model": "m"}),
            },
        )
        .unwrap();
        assert_eq!(
            configured,
            json!({"options": {"model": "m"}, "settings": null})
        );
        let plan: LaunchPlan = asked(
            &engine,
            &Call::Start {
                request: launch("a"),
                prompt: "hi".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(plan.program, "test-cli");
        assert_eq!(plan.args, ["/s/profiles/a1.md", "/bin/txi", "hook"]);
        assert_eq!(plan.env, [("K".to_owned(), "V".to_owned())]);
        assert_eq!(
            plan.workspace,
            [WorkspaceAction::Write {
                path: "note.txt".into(),
                content: "hi".into()
            }]
        );
        assert_eq!(plan.engine_data, Some(json!({"started": true})));
        let plan: LaunchPlan = asked(
            &engine,
            &Call::Fork {
                request: launch("b"),
                source: record("a", "idle"),
                prompt: "branch".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(plan.args, ["--fork"]);
        let change: LlmEngineChange = asked(
            &engine,
            &Call::OnHook {
                session: record("a", "working"),
                hook: LlmEngineHook {
                    name: "Stop".to_owned(),
                    payload: json!({"k": 1}),
                    at: 2.0,
                },
            },
        )
        .unwrap();
        assert_eq!(
            change,
            LlmEngineChange::idle(IdleReason::Finished).engine_data(json!({"payload": {"k": 1}}))
        );
        asked::<()>(
            &engine,
            &Call::Archive {
                session: record("a", "ended"),
                archive_directory: "/d/archive".into(),
            },
        )
        .unwrap();
        assert!(
            asked::<bool>(
                &engine,
                &Call::Restore {
                    session: record("a", "ended"),
                    archive_directory: "/d/archive".into(),
                },
            )
            .unwrap()
        );
        let input: Input = asked(
            &engine,
            &Call::ReceiveMessage {
                sender: Sender::User {
                    via: Some("plan.md".to_owned()),
                },
                text: "hi".to_owned(),
            },
        )
        .unwrap();
        assert_eq!(
            input,
            Input::Text {
                text: "[hi]".to_owned(),
                submit: false
            }
        );
        let interrupted: Option<Interrupted> = asked(
            &engine,
            &Call::Interrupt {
                session: record("a", "working"),
            },
        )
        .unwrap();
        assert_eq!(
            interrupted.map(<(Input, Option<LlmEngineState>)>::from),
            Some((Input::Key(Key::Escape), Some(LlmEngineState::Idle)))
        );
        let deaf: Option<Interrupted> = asked(
            &engine,
            &Call::Interrupt {
                session: record("deaf", "working"),
            },
        )
        .unwrap();
        assert_eq!(deaf, None);
        assert_eq!(
            *seen.lock().unwrap(),
            [
                "configure a",
                "start a hi",
                "fork b of a branch",
                "on_hook a Stop",
                "archive a /d/archive",
                "restore a",
                "receive_message User { via: Some(\"plan.md\") }",
                "interrupt a",
                "interrupt deaf",
            ]
        );
    }

    #[test]
    fn what_the_engine_refuses_comes_back_as_the_error_it_was() {
        let (engine, _) = engine();
        let refused = asked::<Value>(
            &engine,
            &Call::Configure {
                session: record("a", "starting"),
                options: json!({"model": "none"}),
            },
        )
        .unwrap_err();
        assert!(
            matches!(&refused, EngineError::Refused(message) if message == "test options: no such model"),
            "{refused:?}"
        );
        let workspace = asked::<LaunchPlan>(
            &engine,
            &Call::Resume {
                request: launch("a"),
            },
        )
        .unwrap_err();
        assert_eq!(
            workspace.to_string(),
            "workspace action on /w/x: escapes the working directory"
        );
        let io = asked::<()>(
            &engine,
            &Call::Prepare {
                session: record("a", "starting"),
            },
        )
        .unwrap_err();
        assert!(
            matches!(&io, EngineError::Io { path, .. } if path == Path::new("/h/.config.json")),
            "{io:?}"
        );
        assert_eq!(io.to_string(), "/h/.config.json: denied");
    }

    #[test]
    fn the_server_answers_every_line_and_stops_only_when_its_input_ends() {
        let (engine, seen) = engine();
        let prepare = serde_json::to_string(&Call::Archive {
            session: record("a", "ended"),
            archive_directory: "/d".into(),
        })
        .unwrap();
        let responses = served(
            &engine,
            &[
                "oops".to_owned(),
                String::new(),
                r#"{"method":"tools","params":{}}"#.to_owned(),
                r#"{"method":"archive","params":{}}"#.to_owned(),
                prepare,
            ],
        );
        assert_eq!(responses.len(), 5);
        for (response, start) in responses.iter().zip([
            "not a request: expected value",
            "not a request: EOF while parsing",
            "not a request: unknown variant `tools`",
            "not a request: missing field `session`",
        ]) {
            let reply: Reply<()> = serde_json::from_str(response).unwrap();
            let Reply::Error(Refusal::Refused { message }) = reply else {
                panic!("{response}")
            };
            assert!(message.starts_with(start), "{message}");
        }
        assert_eq!(responses[4], r#"{"ok":null}"#);
        assert_eq!(*seen.lock().unwrap(), ["archive a /d"]);
        assert!(served(&engine, &[]).is_empty());
    }

    /// An installation in a temporary directory: the five directories in `home`, one plugin
    /// root holding `engine-test`, whose manifest declares the engine of kind `test`.
    struct Sandbox {
        dir: tempfile::TempDir,
        seen: Seen,
    }

    impl Sandbox {
        fn new() -> Self {
            let dir = tempfile::Builder::new()
                .prefix("txengine")
                .tempdir_in("/tmp")
                .unwrap();
            let sandbox = Self {
                dir,
                seen: Seen::default(),
            };
            let plugin = sandbox.dir.path().join("root/engine-test");
            std::fs::create_dir_all(&plugin).unwrap();
            std::fs::write(
                plugin.join("plugin.yaml"),
                "api: 1\nid: engine-test\ndescription: An engine.\nengine:\n  kind: test\n  \
                 bin: tx-engine-test\n  args: [--tx-engine]\n  \
                 hook: { bin: tx-engine-test, args: [--tx-hook] }\n",
            )
            .unwrap();
            sandbox
        }

        fn home(&self) -> PathBuf {
            self.dir.path().join("home")
        }

        fn write_yaml(&self, text: &str) {
            std::fs::create_dir_all(self.home()).unwrap();
            std::fs::write(self.home().join("config.yaml"), text).unwrap();
        }

        fn arguments(&self, words: &[&str]) -> Vec<String> {
            let mut arguments = Vec::new();
            for flag in [
                "--config-dir",
                "--data-dir",
                "--state-dir",
                "--cache-dir",
                "--run-dir",
            ] {
                arguments.push(flag.to_owned());
                arguments.push(self.home().to_string_lossy().into_owned());
            }
            arguments.push("--plugin-dir".to_owned());
            arguments.push(self.dir.path().join("root").to_string_lossy().into_owned());
            arguments.extend(words.iter().map(|word| (*word).to_owned()));
            arguments
        }

        fn global(&self) -> Global {
            parse_global(&self.arguments(&[])).unwrap().0
        }

        fn hosted(&self) -> Vec<Box<dyn Plugin>> {
            vec![
                Box::new(Empty),
                Box::new(TestPlugin(Arc::clone(&self.seen))),
            ]
        }

        fn seen(&self) -> Vec<String> {
            std::mem::take(&mut *self.seen.lock().unwrap())
        }

        /// Plants the record of an agent `worker` of the engine `test`: its id.
        fn plant(&self, state: &str) -> String {
            let id = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
            let directory = self.home().join("agents").join(id);
            std::fs::create_dir_all(&directory).unwrap();
            let record = json!({
                "version": 2, "id": id, "name": "worker", "engine": "test",
                "working_directory": "/w", "roles": [], "state": state, "created_at": 1.0,
                "last_modified_at": 1.0, "tags": [],
            });
            std::fs::write(directory.join("agent.json"), record.to_string()).unwrap();
            id.to_owned()
        }

        fn record(&self, id: &str) -> Value {
            let file = self.home().join("agents").join(id).join("agent.json");
            serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
        }

        /// The hook entry of the program hosting [`Self::hosted`], inside the session `id`.
        fn hook(&self, id: Option<&str>, event: &str, payload: &str) -> Result<(), Failure> {
            let environment = |key: &str| {
                (key == SESSION_ID_ENV)
                    .then(|| id.map(OsString::from))
                    .flatten()
            };
            let global = self.global();
            hook_in(
                &environment,
                event.to_owned(),
                &mut payload.as_bytes(),
                || Installation::read(&global)?.open_hosted(&global, self.hosted()),
            )
        }
    }

    #[test]
    fn the_engine_served_is_the_one_an_active_hosted_plugin_registers_with_its_settings() {
        let sandbox = Sandbox::new();
        sandbox.write_yaml("plugins:\n  engine-test:\n    settings: { flavour: plain }\n");
        let registry = Installation::read(&sandbox.global())
            .unwrap()
            .engine_registry(&sandbox.hosted())
            .unwrap()
            .expect("the engine");
        let engine = registry.engines()[0].as_ref();
        assert_eq!(engine.kind().as_str(), "test");
        let configured: Value = asked(
            engine,
            &Call::Configure {
                session: record("a", "starting"),
                options: json!({}),
            },
        )
        .unwrap();
        assert_eq!(configured["settings"], json!({"flavour": "plain"}));
        assert!(
            !sandbox.home().join("agents").exists(),
            "no core was opened"
        );

        // Switched off, or hosting no engine at all: nothing to serve, and that is misuse.
        sandbox.write_yaml("plugins:\n  engine-test: { enabled: false }\n");
        let installation = Installation::read(&sandbox.global()).unwrap();
        assert!(
            installation
                .engine_registry(&sandbox.hosted())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            serve_hosted("tx-engine-test", &sandbox.global(), &sandbox.hosted()).unwrap(),
            2
        );
        let none: Vec<Box<dyn Plugin>> = vec![Box::new(Empty)];
        sandbox.write_yaml("");
        assert_eq!(
            serve_hosted("tx-empty", &sandbox.global(), &none).unwrap(),
            2
        );
    }

    #[test]
    fn the_hook_entry_moves_the_agent_of_its_session() {
        let sandbox = Sandbox::new();
        let id = sandbox.plant("starting");
        sandbox.hook(Some(&id), "Stop", r#"{"k": 1}"#).unwrap();
        let record = sandbox.record(&id);
        assert_eq!(record["state"], "idle");
        assert_eq!(record["idle_reason"], "finished");
        assert_eq!(record["ext"]["test"], json!({"payload": {"k": 1}}));
        // The hosted engine is in the process: it is asked, then archives, with no program.
        let archive = sandbox.home().join("agents").join(&id).join("archive");
        assert_eq!(
            sandbox.seen(),
            [
                "on_hook worker Stop".to_owned(),
                format!("archive worker {}", archive.display())
            ]
        );
        // A payload that is no object is no payload; an event the engine does not know changes
        // nothing; neither fails.
        sandbox
            .hook(Some(&id), "UserPromptSubmit", "not json")
            .unwrap();
        assert_eq!(sandbox.record(&id)["state"], "working");
        sandbox.hook(Some(&id), "NoSuchEvent", "[1, 2]").unwrap();
        assert_eq!(sandbox.record(&id)["state"], "working");
    }

    #[test]
    fn a_hook_that_is_not_txs_opens_nothing_and_one_for_no_record_changes_nothing() {
        let sandbox = Sandbox::new();
        for not_ours in [None, Some("not an id"), Some("")] {
            sandbox.hook(not_ours, "Stop", "{}").unwrap();
        }
        assert!(!sandbox.home().exists(), "nothing was opened");
        // An id this core holds no record of: opened, and nothing to do.
        sandbox
            .hook(Some("0f1e2d3c-0000-4000-8000-000000000000"), "Stop", "{}")
            .unwrap();
        assert!(sandbox.seen().is_empty());
        // A hook of an agent that has ended is stale: the engine is not asked.
        let id = sandbox.plant("ended");
        sandbox.hook(Some(&id), "Stop", "{}").unwrap();
        assert_eq!(sandbox.record(&id)["state"], "ended");
        assert!(sandbox.seen().is_empty());
        // With its plugin switched off the engine is not there: still no failure.
        let id = sandbox.plant("working");
        sandbox.write_yaml("plugins:\n  engine-test: { enabled: false }\n");
        sandbox.hook(Some(&id), "Stop", "{}").unwrap();
        assert_eq!(sandbox.record(&id)["state"], "working");
        assert!(sandbox.seen().is_empty());
    }

    #[test]
    fn a_hook_that_cannot_open_the_core_is_an_error_for_its_caller_to_cap() {
        let sandbox = Sandbox::new();
        let id = sandbox.plant("working");
        sandbox.write_yaml("plugins: [1, 2\n");
        let error = sandbox.hook(Some(&id), "Stop", "{}").unwrap_err();
        assert!(matches!(error, Failure::Usage(_)), "{error}");
    }

    #[test]
    fn a_payload_is_an_object_or_nothing() {
        for (text, expected) in [
            (r#"{"a": 1}"#, json!({"a": 1})),
            ("", json!({})),
            ("not json", json!({})),
            ("[1, 2]", json!({})),
            ("7", json!({})),
        ] {
            assert_eq!(read_payload(&mut text.as_bytes()), expected, "{text:?}");
        }
        assert_eq!(read_payload(&mut &b"\xff\xfe"[..]), json!({}));
    }
}
