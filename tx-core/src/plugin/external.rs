//! Plugins that run as their own programs, seen from a core: how a program of a plugin is
//! started ([`ExecContext`], [`Launcher`]) and the proxies that stand, in this process, for a
//! verb and for the engine of a plugin that is not hosted in it.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde_json::Value;

use super::index::{Activation, Index, Installed, PluginState};
use super::manifest::{BIN_DIR, EngineMethod, ManifestEngine, ManifestInterrupt, PLUGIN_API};
use super::{PluginError, Verb, VerbError, VerbIo};
use crate::agent::Agent;
use crate::config::CoreConfig;
use crate::core::TxCore;
use crate::engine::wire::{Call, Interrupted, Launch, Record, Reply, Sender};
use crate::engine::{EngineError, EngineKind, Input, LaunchPlan, LaunchRequest, LlmEngine};
use crate::messaging::{EnvelopeSender, build_envelope};
use crate::state::{LlmEngineChange, LlmEngineHook, LlmEngineState};

/// The role of a traced start: a verb of a plugin, run by `tx` or by a proxy.
pub const TRACE_VERB: &str = "verb";
/// The role of a traced start: the engine of a plugin, started by its proxy.
pub const TRACE_ENGINE: &str = "engine";
/// The role of a traced start: an engine's own hook entry. Nothing of tx starts it (the
/// engine's CLI does), so the entry writes the line itself.
pub const TRACE_HOOK: &str = "hook";

/// What every plugin program is started with, whichever plugin it belongs to. Built by the front
/// end, since core never reads the environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExecContext {
    /// The variables a plugin program gets on top of the caller's environment, apart from the
    /// ones that name its plugin: the `tx` to run, the application name, the five directories,
    /// the socket, and the variables that stand for a global flag.
    pub environment: Vec<(String, String)>,
    /// The file that gets one line `<role>\t<plugin id>\t<program>` for every plugin program
    /// started; `None`: no trace.
    pub trace: Option<PathBuf>,
}

impl ExecContext {
    /// Notes that `program` of the plugin `plugin` is about to start in the role `role`: one
    /// line `<role>\t<plugin id>\t<program>` appended to the trace file, when there is one. One
    /// append, so lines of processes tracing into the same file never mix. A trace that cannot
    /// be written never fails what it traces.
    pub fn trace(&self, role: &str, plugin: &str, program: &Path) {
        let Some(file) = &self.trace else {
            return;
        };
        let line = format!("{role}\t{plugin}\t{}\n", program.display());
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// One active plugin, as its programs are started: the context, plus the variables that name the
/// plugin, its directories and its settings.
#[derive(Clone, Debug)]
pub struct Launcher {
    context: Arc<ExecContext>,
    id: String,
    directory: PathBuf,
    data: PathBuf,
    /// `plugins.<id>.settings` as JSON; `null` without one.
    settings: String,
}

impl Launcher {
    pub fn new(
        context: &Arc<ExecContext>,
        config: &CoreConfig,
        state: &PluginState,
        installed: &Installed,
    ) -> Self {
        Self {
            context: Arc::clone(context),
            id: state.id.clone(),
            directory: installed.directory.clone(),
            data: config.plugin_data_dir(&state.id),
            settings: state.settings.to_string(),
        }
    }

    /// The id of the plugin.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// `<plugin>/bin/<bin>`: the program a `bin` of the manifest names.
    pub fn program(&self, bin: &str) -> PathBuf {
        self.directory.join(BIN_DIR).join(bin)
    }

    /// The command that runs `program` with `args`, for a caller about to start it in the role
    /// `role`: the trace line is appended here. The program inherits the caller's environment
    /// with the context's variables and its plugin's on top; standard input, output and error
    /// are the caller's to set.
    pub fn command(&self, role: &str, program: &Path, args: &[String]) -> Command {
        self.context.trace(role, &self.id, program);
        let mut command = Command::new(program);
        command
            .args(args)
            .envs(
                self.context
                    .environment
                    .iter()
                    .map(|(key, value)| (key, value)),
            )
            .env("TX_PLUGIN_API", PLUGIN_API.to_string())
            .env("TX_PLUGIN_ID", &self.id)
            .env("TX_PLUGIN_DIR", &self.directory)
            .env("TX_PLUGIN_DATA_DIR", &self.data)
            .env("TX_PLUGIN_SETTINGS", &self.settings);
        command
    }
}

/// The exit code of a program that ended; one killed by a signal counts as `128 + signal`, the
/// way a shell reports it.
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or_default())
}

/// A verb of an active plugin that is not hosted in this process. Running it starts the plugin's
/// program directly, not through `tx`, with standard input closed, and hands its output, its
/// error and its exit code to the caller once it ended.
struct ProxyVerb {
    name: String,
    usage: String,
    program: PathBuf,
    args: Vec<String>,
    launcher: Launcher,
}

impl Verb for ProxyVerb {
    fn name(&self) -> &str {
        &self.name
    }

    fn usage(&self) -> &str {
        &self.usage
    }

    fn run(&self, _core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let output = self
            .launcher
            .command(TRACE_VERB, &self.program, &self.args)
            .args(argv)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| {
                VerbError::Failed(format!("cannot run {}: {error}", self.program.display()))
            })?;
        let written = |error: std::io::Error| VerbError::Failed(format!("write: {error}"));
        io.stdout.write_all(&output.stdout).map_err(written)?;
        io.stderr.write_all(&output.stderr).map_err(written)?;
        Ok(exit_code(output.status))
    }
}

/// The installed plugins that are not hosted here join the core's plugin listing with the status
/// the activation gives them; the second directory of an id is left to the first.
pub(super) fn list(core: &mut TxCore, activation: &Activation, hosted_ids: &[&str]) {
    for state in &activation.plugins {
        let listed = core.host.status.iter().any(|(id, _)| *id == state.id);
        if state.installed.is_some() && !listed && !hosted_ids.contains(&state.id.as_str()) {
            core.host
                .status
                .push((state.id.clone(), state.status.clone()));
        }
    }
}

/// The verb step of [`TxCore::plug_external`]: a proxy for every verb the activation gives a
/// plugin that is not hosted here. The activation decided whose a verb is, so a proxy takes the
/// place of a verb of the same name registered in-process.
pub(super) fn plug_verbs(
    core: &mut TxCore,
    index: &Index,
    activation: &Activation,
    context: &Arc<ExecContext>,
    hosted_ids: &[&str],
) {
    let launchers: BTreeMap<&str, Launcher> = activation
        .active(index)
        .filter(|(state, _, _)| !hosted_ids.contains(&state.id.as_str()))
        .map(|(state, installed, _)| {
            (
                state.id.as_str(),
                Launcher::new(context, core.config(), state, installed),
            )
        })
        .collect();
    for (name, verb) in &activation.verbs {
        let Some(launcher) = launchers.get(verb.plugin.as_str()) else {
            continue;
        };
        let proxy = ProxyVerb {
            name: name.clone(),
            usage: verb.usage.clone(),
            program: verb.program.clone(),
            args: verb.args.clone(),
            launcher: launcher.clone(),
        };
        core.host
            .verbs
            .insert(name.clone(), (verb.plugin.clone(), Arc::new(proxy)));
    }
}

/// How long the program of an engine has to end by itself once its input is closed.
const ENGINE_GRACE: Duration = Duration::from_secs(1);

/// The program of an engine while it runs: the child, the pipe its requests go into and the
/// pipe its responses come out of.
struct Running {
    child: Child,
    requests: ChildStdin,
    responses: BufReader<ChildStdout>,
}

impl Running {
    /// Closes the program's input, gives it `grace` to end by itself, then kills it. Either way
    /// it is reaped before this returns.
    fn stop(self, grace: Duration) {
        let Running {
            mut child,
            requests,
            responses: _responses,
        } = self;
        drop(requests);
        let deadline = Instant::now() + grace;
        let mut pause = Duration::from_micros(200);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(pause);
                    pause = (pause * 2).min(Duration::from_millis(5));
                }
                _ => break,
            }
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The engine of an active plugin that is not hosted in this process: an [`LlmEngine`] whose
/// answers come from the plugin's program, over the protocol of [`crate::engine::wire`].
///
/// The program starts on the first call that needs it, or on [`LlmEngine::warm`], and is kept
/// for every later call: one request line in, one response line out. It ends when the proxy is
/// dropped: its input is closed, it has a second to exit, then it is killed. A method the
/// manifest does not list is answered here as the trait's default answers it, without the
/// program; so is `interrupt` when the manifest gives the fixed answer, and `conversation`
/// always (the two reserved keys of the engine's data).
///
/// An exchange that breaks (the program cannot start, ends without answering, or says
/// something that is not a response) is `EngineError::Refused("engine <kind>: <what
/// happened>")`; the program is then discarded and the next call starts a new one. The three
/// methods that cannot fail (`on_hook`, `receive_message`, `interrupt`) fall back to the
/// trait's default and say why in one line on standard error.
pub struct ProcessEngine {
    kind: EngineKind,
    ext_key: String,
    program: PathBuf,
    args: Vec<String>,
    methods: Vec<EngineMethod>,
    interrupt: Option<ManifestInterrupt>,
    /// The engine's own hook entry, `[<plugin>/bin/<hook.bin>, hook.args…]`, sent in every
    /// launch request instead of core's; `None`: the engine has none, and core's stands.
    hook_command: Option<Vec<String>>,
    launcher: Launcher,
    running: Mutex<Option<Running>>,
}

impl ProcessEngine {
    /// The proxy of the plugin behind `launcher`, for the `engine` section of its manifest.
    pub fn new(launcher: Launcher, engine: &ManifestEngine) -> Self {
        Self {
            kind: engine
                .kind
                .parse()
                .expect("the engine kind of a manifest that loaded"),
            ext_key: engine.ext_key.clone(),
            program: launcher.program(&engine.bin),
            args: engine.args.clone(),
            methods: engine.methods.clone(),
            interrupt: engine.interrupt.clone(),
            hook_command: engine.hook.as_ref().map(|hook| {
                std::iter::once(launcher.program(&hook.bin).to_string_lossy().into_owned())
                    .chain(hook.args.iter().cloned())
                    .collect()
            }),
            launcher,
            running: Mutex::new(None),
        }
    }

    /// Starts the program, with its trace line.
    fn spawn(&self) -> Result<Running, String> {
        let mut child = self
            .launcher
            .command(TRACE_ENGINE, &self.program, &self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("cannot run {}: {error}", self.program.display()))?;
        let requests = child.stdin.take().expect("the child's piped input");
        let responses = BufReader::new(child.stdout.take().expect("the child's piped output"));
        Ok(Running {
            child,
            requests,
            responses,
        })
    }

    /// One call: the request written, the response read and taken apart.
    fn ask<T: DeserializeOwned>(&self, call: &Call) -> Result<T, EngineError> {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        match self.exchange(&mut running, call) {
            Ok(reply) => reply.into(),
            Err(what) => {
                // The program is no longer in step with its caller: the next call starts anew.
                if let Some(broken) = running.take() {
                    broken.stop(Duration::ZERO);
                }
                Err(EngineError::Refused(format!(
                    "engine {}: {what}",
                    self.kind
                )))
            }
        }
    }

    /// The exchange itself; what happened, when it broke.
    fn exchange<T: DeserializeOwned>(
        &self,
        running: &mut Option<Running>,
        call: &Call,
    ) -> Result<Reply<T>, String> {
        let method = call.method();
        let program = match running {
            Some(program) => program,
            None => running.insert(self.spawn()?),
        };
        let mut request = serde_json::to_string(call)
            .map_err(|error| format!("cannot write {method}: {error}"))?;
        request.push('\n');
        let gone = || format!("the engine exited without answering {method}");
        program
            .requests
            .write_all(request.as_bytes())
            .and_then(|()| program.requests.flush())
            .map_err(|_| gone())?;
        let mut response = String::new();
        match program.responses.read_line(&mut response) {
            Ok(0) => return Err(gone()),
            Ok(_) => {}
            Err(error) => return Err(format!("not a response to {method}: {error}")),
        }
        let reply: Reply<Value> = serde_json::from_str(&response).map_err(|_| {
            format!(
                "not a response to {method}: {}",
                excerpt(response.trim_end())
            )
        })?;
        Ok(match reply {
            Reply::Ok(value) => Reply::Ok(
                serde_json::from_value(value)
                    .map_err(|error| format!("bad answer to {method}: {error}"))?,
            ),
            Reply::Error(failure) => Reply::Error(failure),
        })
    }

    fn implements(&self, method: EngineMethod) -> bool {
        self.methods.contains(&method)
    }

    /// The launch request as the program gets it: with the engine's own hook entry, when its
    /// manifest names one.
    fn launch(&self, request: &LaunchRequest<'_>) -> Box<Launch> {
        let mut launch = Launch::from(request);
        if let Some(hook_command) = &self.hook_command {
            launch.hook_command.clone_from(hook_command);
        }
        Box::new(launch)
    }
}

/// At most 200 bytes of a line, for an error message.
fn excerpt(line: &str) -> String {
    const MAX_BYTES: usize = 200;
    if line.len() <= MAX_BYTES {
        return line.to_owned();
    }
    let mut end = MAX_BYTES;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

/// What a method that cannot fail says when its program let it down, before it answers as the
/// trait's default does.
fn complain(error: &EngineError) {
    eprintln!("tx: {error}");
}

impl LlmEngine for ProcessEngine {
    fn kind(&self) -> EngineKind {
        self.kind
    }

    fn ext_key(&self) -> &str {
        &self.ext_key
    }

    fn configure(&self, session: &Agent, options: &Value) -> Result<Value, EngineError> {
        self.ask(&Call::Configure {
            session: Record(session.clone()),
            options: options.clone(),
        })
    }

    fn start(&self, request: &LaunchRequest<'_>, prompt: &str) -> Result<LaunchPlan, EngineError> {
        self.ask(&Call::Start {
            request: self.launch(request),
            prompt: prompt.to_owned(),
        })
    }

    fn resume(&self, request: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        self.ask(&Call::Resume {
            request: self.launch(request),
        })
    }

    fn fork(
        &self,
        request: &LaunchRequest<'_>,
        source: &Agent,
        prompt: &str,
    ) -> Result<LaunchPlan, EngineError> {
        self.ask(&Call::Fork {
            request: self.launch(request),
            source: Record(source.clone()),
            prompt: prompt.to_owned(),
        })
    }

    fn receive_message(&self, sender: &EnvelopeSender<'_>, text: &str) -> Input {
        let default = || Input::line(build_envelope(sender, text));
        if !self.implements(EngineMethod::ReceiveMessage) {
            return default();
        }
        self.ask(&Call::ReceiveMessage {
            sender: Sender::from(sender),
            text: text.to_owned(),
        })
        .unwrap_or_else(|error| {
            complain(&error);
            default()
        })
    }

    fn interrupt(&self, session: &Agent) -> Option<(Input, Option<LlmEngineState>)> {
        if let Some(fixed) = &self.interrupt {
            return Some((Input::Key(fixed.key), fixed.state));
        }
        if !self.implements(EngineMethod::Interrupt) {
            return None;
        }
        let answer: Result<Option<Interrupted>, EngineError> = self.ask(&Call::Interrupt {
            session: Record(session.clone()),
        });
        match answer {
            Ok(interrupted) => interrupted.map(Into::into),
            Err(error) => {
                complain(&error);
                None
            }
        }
    }

    fn on_hook(&self, session: &Agent, hook: &LlmEngineHook) -> LlmEngineChange {
        self.ask(&Call::OnHook {
            session: Record(session.clone()),
            hook: hook.clone(),
        })
        .unwrap_or_else(|error| {
            complain(&error);
            LlmEngineChange::NONE
        })
    }

    fn archive(&self, session: &Agent, archive_directory: &Path) -> Result<(), EngineError> {
        if !self.implements(EngineMethod::Archive) {
            return Ok(());
        }
        self.ask(&Call::Archive {
            session: Record(session.clone()),
            archive_directory: archive_directory.to_owned(),
        })
    }

    fn prepare(&self, session: &Agent) -> Result<(), EngineError> {
        if !self.implements(EngineMethod::Prepare) {
            return Ok(());
        }
        self.ask(&Call::Prepare {
            session: Record(session.clone()),
        })
    }

    fn restore(&self, session: &Agent, archive_directory: &Path) -> Result<bool, EngineError> {
        if !self.implements(EngineMethod::Restore) {
            return Ok(false);
        }
        self.ask(&Call::Restore {
            session: Record(session.clone()),
            archive_directory: archive_directory.to_owned(),
        })
    }

    /// Starts the program, unless it runs. One that cannot start is the business of the call
    /// that then needs it.
    fn warm(&self) {
        let mut running = self.running.lock().unwrap_or_else(PoisonError::into_inner);
        if running.is_none() {
            *running = self.spawn().ok();
        }
    }
}

impl Drop for ProcessEngine {
    fn drop(&mut self) {
        let running = self
            .running
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(running) = running.take() {
            running.stop(ENGINE_GRACE);
        }
    }
}

/// The engine step of [`TxCore::plug_external`]: a proxy for the engine of every plugin that
/// is not hosted here and that the activation gave its engine kind. Clashes between installed
/// plugins were settled there. An engine linked into this process that already has the kind or
/// the `ext` key is not settled here: the registry refuses the proxy, as it refuses the second
/// of two linked engines.
pub(super) fn plug_engines(
    core: &mut TxCore,
    index: &Index,
    activation: &Activation,
    context: &Arc<ExecContext>,
    hosted_ids: &[&str],
) -> Result<(), PluginError> {
    for (state, installed, manifest) in activation.active(index) {
        let Some(engine) = &manifest.engine else {
            continue;
        };
        let owns = activation
            .engines
            .values()
            .any(|active| active.plugin == state.id);
        if !owns || hosted_ids.contains(&state.id.as_str()) {
            continue;
        }
        let launcher = Launcher::new(context, core.config(), state, installed);
        core.register_engine(Box::new(ProcessEngine::new(launcher, engine)))
            .map_err(|source| PluginError::Engine {
                plugin: state.id.clone(),
                source,
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Directories;
    use crate::engine::Key;
    use crate::plugin::PluginStatus;
    use crate::plugin::manifest::ManifestProgram;
    use crate::profile::Profile;
    use crate::state::IdleReason;
    use serde_json::json;

    /// An engine that answers every method, notes each start in `starts` (its pid) and each
    /// request in `calls`, both in the plugin's directory.
    const ANSWERING: &str = r#"echo "$$" >> "$TX_PLUGIN_DIR/starts"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$TX_PLUGIN_DIR/calls"
  case "$line" in
    '{"method":"configure"'*) echo '{"ok":{"configured":true}}' ;;
    '{"method":"start"'*|'{"method":"resume"'*|'{"method":"fork"'*) echo '{"ok":{"program":"sleep","args":["600"]}}' ;;
    '{"method":"on_hook"'*) echo '{"ok":{"state":"idle","idle_reason":"finished"}}' ;;
    '{"method":"restore"'*) echo '{"ok":true}' ;;
    '{"method":"receive_message"'*) echo '{"ok":{"text":{"text":"framed","submit":false}}}' ;;
    '{"method":"interrupt"'*) echo '{"ok":{"input":{"key":"C-c"}}}' ;;
    *) echo '{"ok":null}' ;;
  esac
done"#;

    const EVERY_METHOD: [EngineMethod; 5] = [
        EngineMethod::Prepare,
        EngineMethod::ReceiveMessage,
        EngineMethod::Interrupt,
        EngineMethod::Archive,
        EngineMethod::Restore,
    ];

    /// A plugin `engine-test` in a temporary directory whose engine is the shell script
    /// `bin/engine`.
    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new(script: &str) -> Self {
            let fixture = Self {
                dir: tempfile::tempdir().unwrap(),
            };
            let program = fixture.plugin().join("bin/engine");
            std::fs::create_dir_all(program.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink("/bin/sh", &program).unwrap();
            std::fs::write(fixture.dir.path().join("engine.sh"), format!("{script}\n")).unwrap();
            fixture
        }

        fn plugin(&self) -> PathBuf {
            self.dir.path().join("engine-test")
        }

        fn trace(&self) -> PathBuf {
            self.dir.path().join("trace")
        }

        fn section(methods: &[EngineMethod]) -> ManifestEngine {
            ManifestEngine {
                kind: "fake".to_owned(),
                ext_key: "fake".to_owned(),
                bin: "engine".to_owned(),
                args: Vec::new(),
                hook: None,
                methods: methods.to_vec(),
                interrupt: None,
                notes: Vec::new(),
            }
        }

        /// The proxy for `section`, tracing into [`Self::trace`].
        fn engine(&self, section: &ManifestEngine) -> ProcessEngine {
            let mut section = section.clone();
            section
                .args
                .insert(0, self.dir.path().join("engine.sh").display().to_string());
            let context = Arc::new(ExecContext {
                environment: Vec::new(),
                trace: Some(self.trace()),
            });
            let config = CoreConfig::new(Directories::under(self.dir.path().join("home")));
            let state = PluginState {
                id: "engine-test".to_owned(),
                status: PluginStatus::Active,
                settings: Value::Null,
                installed: Some(0),
            };
            let installed = Installed {
                id: "engine-test".to_owned(),
                directory: self.plugin(),
                root: self.dir.path().to_owned(),
                manifest: Err("not looked at".to_owned()),
            };
            ProcessEngine::new(
                Launcher::new(&context, &config, &state, &installed),
                &section,
            )
        }

        fn lines(&self, file: &str) -> Vec<String> {
            std::fs::read_to_string(self.plugin().join(file))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        /// How often the program was started.
        fn starts(&self) -> usize {
            self.lines("starts").len()
        }

        /// The requests the program got, in order.
        fn calls(&self) -> Vec<Value> {
            self.lines("calls")
                .iter()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }

        fn methods_called(&self) -> Vec<String> {
            self.calls()
                .iter()
                .map(|call| call["method"].as_str().unwrap().to_owned())
                .collect()
        }

        /// Whether the program started last is still a process.
        fn alive(&self) -> bool {
            let pid = self.lines("starts").pop().expect("a start");
            Command::new("kill")
                .args(["-0", &pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        }
    }

    fn session() -> Agent {
        Agent::from_json(
            r#"{"version":2,"id":"a1","name":"worker","engine":"fake","working_directory":"/w","roles":[],"state":"working","created_at":1.0,"last_modified_at":1.0,"tags":[]}"#,
        )
        .unwrap()
    }

    fn profile() -> Profile {
        serde_json::from_value(json!({
            "text": "t", "file": "/p.md", "skill_dirs": [], "tools": null,
            "disallowed_tools": [], "mcp_servers": {}, "provided_skills": [],
        }))
        .unwrap()
    }

    fn hook(name: &str) -> LlmEngineHook {
        LlmEngineHook {
            name: name.to_owned(),
            payload: json!({}),
            at: 2.0,
        }
    }

    fn refusal<T: std::fmt::Debug>(result: Result<T, EngineError>) -> String {
        match result.unwrap_err() {
            EngineError::Refused(message) => message,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn one_program_answers_every_call_and_ends_with_the_proxy() {
        let fixture = Fixture::new(ANSWERING);
        let engine = fixture.engine(&Fixture::section(&EVERY_METHOD));
        assert_eq!(engine.kind().as_str(), "fake");
        assert_eq!(engine.ext_key(), "fake");
        assert_eq!(fixture.starts(), 0, "nothing starts before a call needs it");
        let (session, profile) = (session(), profile());
        let hooks = ["/bin/txi".to_owned(), "hook".to_owned()];
        let request = LaunchRequest::new(&session, &profile).hook_command(&hooks);
        let plan = LaunchPlan::new("sleep").arg("600");

        assert_eq!(
            engine.configure(&session, &json!({"model": "m"})).unwrap(),
            json!({"configured": true})
        );
        engine.prepare(&session).unwrap();
        assert_eq!(engine.start(&request, "hi").unwrap(), plan);
        assert_eq!(engine.resume(&request).unwrap(), plan);
        assert_eq!(engine.fork(&request, &session, "branch").unwrap(), plan);
        assert_eq!(
            engine.on_hook(&session, &hook("Stop")),
            LlmEngineChange::idle(IdleReason::Finished)
        );
        engine.archive(&session, Path::new("/archive")).unwrap();
        assert!(engine.restore(&session, Path::new("/archive")).unwrap());
        assert_eq!(
            engine.receive_message(&EnvelopeSender::Agent { name: "lead" }, "status?"),
            Input::Text {
                text: "framed".to_owned(),
                submit: false
            }
        );
        assert_eq!(
            engine.interrupt(&session),
            Some((Input::Key(Key::ControlC), None))
        );

        assert_eq!(fixture.starts(), 1, "one program for every call");
        assert_eq!(
            fixture.methods_called(),
            [
                "configure",
                "prepare",
                "start",
                "resume",
                "fork",
                "on_hook",
                "archive",
                "restore",
                "receive_message",
                "interrupt"
            ]
        );
        let calls = fixture.calls();
        assert_eq!(calls[0]["params"]["options"], json!({"model": "m"}));
        assert_eq!(calls[0]["params"]["session"]["id"], "a1");
        assert_eq!(calls[0]["params"]["session"]["version"], 2);
        assert_eq!(calls[2]["params"]["prompt"], "hi");
        assert_eq!(
            calls[2]["params"]["request"]["hook_command"],
            json!(["/bin/txi", "hook"]),
            "an engine without a hook entry of its own wires core's"
        );
        assert_eq!(calls[4]["params"]["source"]["name"], "worker");
        assert_eq!(calls[5]["params"]["hook"]["name"], "Stop");
        assert_eq!(calls[6]["params"]["archive_directory"], "/archive");
        assert_eq!(
            calls[8]["params"],
            json!({"sender": {"agent": {"name": "lead"}}, "text": "status?"})
        );
        assert_eq!(
            std::fs::read_to_string(fixture.trace()).unwrap(),
            format!(
                "engine\tengine-test\t{}\n",
                fixture.plugin().join("bin/engine").display()
            )
        );

        assert!(fixture.alive());
        drop(engine);
        assert!(!fixture.alive(), "the program ended with its proxy");
    }

    #[test]
    fn a_method_the_manifest_does_not_list_is_answered_without_the_program() {
        let fixture = Fixture::new(ANSWERING);
        let engine = fixture.engine(&Fixture::section(&[]));
        let mut session = session();
        engine.prepare(&session).unwrap();
        engine.archive(&session, Path::new("/archive")).unwrap();
        assert!(!engine.restore(&session, Path::new("/archive")).unwrap());
        assert_eq!(
            engine.receive_message(&EnvelopeSender::User { via: None }, "hi"),
            Input::line("<from-user>hi</from-user>")
        );
        assert_eq!(engine.interrupt(&session), None);
        // The conversation is the two reserved keys of the engine's data.
        assert_eq!(engine.conversation(&session), None);
        session.ext.insert(
            "fake".to_owned(),
            json!({"conversation_id": "c", "transcript_path": "/t.jsonl"}),
        );
        assert_eq!(engine.conversation(&session).unwrap().id, "c");
        assert_eq!(fixture.starts(), 0);
        assert!(!fixture.trace().exists());

        // The manifest's fixed answer to `interrupt`, still without the program.
        let fixed = ManifestEngine {
            interrupt: Some(ManifestInterrupt {
                key: Key::Escape,
                state: Some(LlmEngineState::Idle),
            }),
            ..Fixture::section(&[])
        };
        assert_eq!(
            fixture.engine(&fixed).interrupt(&session),
            Some((Input::Key(Key::Escape), Some(LlmEngineState::Idle)))
        );
        assert_eq!(fixture.starts(), 0);
    }

    #[test]
    fn warm_starts_the_program_and_the_next_call_finds_it_running() {
        let fixture = Fixture::new(ANSWERING);
        let engine = fixture.engine(&Fixture::section(&[]));
        engine.warm();
        engine.warm();
        // The program notes its start itself, a moment after it was started.
        for _ in 0..200 {
            if fixture.starts() == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(fixture.starts(), 1);
        assert!(fixture.calls().is_empty());
        assert_eq!(
            engine.on_hook(&session(), &hook("Stop")),
            LlmEngineChange::idle(IdleReason::Finished)
        );
        assert_eq!(fixture.starts(), 1);
        assert_eq!(
            std::fs::read_to_string(fixture.trace())
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn an_engine_with_a_hook_entry_of_its_own_gets_it_as_the_hook_command() {
        let fixture = Fixture::new(ANSWERING);
        let section = ManifestEngine {
            hook: Some(ManifestProgram {
                bin: "hook".to_owned(),
                args: vec!["--tx-hook".to_owned()],
            }),
            ..Fixture::section(&[])
        };
        let engine = fixture.engine(&section);
        let (session, profile) = (session(), profile());
        let hooks = ["/bin/txi".to_owned(), "hook".to_owned()];
        let request = LaunchRequest::new(&session, &profile).hook_command(&hooks);
        engine.start(&request, "hi").unwrap();
        engine.resume(&request).unwrap();
        engine.fork(&request, &session, "branch").unwrap();
        // And when core has none to offer (no `tx` is known), the engine's own still stands.
        engine
            .start(&LaunchRequest::new(&session, &profile), "hi")
            .unwrap();
        let own = json!([
            fixture.plugin().join("bin/hook").to_string_lossy(),
            "--tx-hook"
        ]);
        let calls = fixture.calls();
        assert_eq!(calls.len(), 4);
        for call in calls {
            assert_eq!(call["params"]["request"]["hook_command"], own, "{call}");
        }
    }

    #[test]
    fn a_program_that_cannot_start_is_a_refusal_that_names_it() {
        let fixture = Fixture::new(ANSWERING);
        std::fs::remove_file(fixture.plugin().join("bin/engine")).unwrap();
        let engine = fixture.engine(&Fixture::section(&[]));
        let message = refusal(engine.configure(&session(), &json!({})));
        assert!(
            message.starts_with(&format!(
                "engine fake: cannot run {}: ",
                fixture.plugin().join("bin/engine").display()
            )),
            "{message}"
        );
        // Warming it fails without a word; the call that needs it says so.
        engine.warm();
        assert_eq!(
            engine.on_hook(&session(), &hook("Stop")),
            LlmEngineChange::NONE
        );
    }

    #[test]
    fn a_line_that_is_not_a_response_is_a_refusal_and_the_next_call_starts_anew() {
        let fixture = Fixture::new(
            r#"echo "$$" >> "$TX_PLUGIN_DIR/starts"
echo oops
while IFS= read -r line; do echo '{"ok":{}}'; done"#,
        );
        let engine = fixture.engine(&Fixture::section(&[]));
        assert_eq!(
            refusal(engine.configure(&session(), &json!({}))),
            "engine fake: not a response to configure: oops"
        );
        // The program that was out of step is gone; another one is asked, and is as wrong.
        assert!(!fixture.alive());
        assert_eq!(
            refusal(engine.configure(&session(), &json!({}))),
            "engine fake: not a response to configure: oops"
        );
        assert_eq!(fixture.starts(), 2);
        assert_eq!(
            std::fs::read_to_string(fixture.trace())
                .unwrap()
                .lines()
                .count(),
            2
        );
        // A long line is cut.
        let long = Fixture::new(&format!("echo {}\ncat > /dev/null", "x".repeat(300)));
        let message = refusal(
            long.engine(&Fixture::section(&[]))
                .configure(&session(), &json!({})),
        );
        assert_eq!(
            message,
            format!(
                "engine fake: not a response to configure: {}…",
                "x".repeat(200)
            )
        );
    }

    #[test]
    fn a_program_that_ends_without_answering_is_a_refusal() {
        let fixture = Fixture::new("exit 0");
        let engine = fixture.engine(&Fixture::section(&[EngineMethod::Archive]));
        assert_eq!(
            refusal(engine.configure(&session(), &json!({}))),
            "engine fake: the engine exited without answering configure"
        );
        assert_eq!(
            refusal(engine.archive(&session(), Path::new("/archive"))),
            "engine fake: the engine exited without answering archive"
        );
        // The methods that cannot fail answer as the trait's default does.
        assert_eq!(
            engine.on_hook(&session(), &hook("Stop")),
            LlmEngineChange::NONE
        );
        let every = fixture.engine(&Fixture::section(&EVERY_METHOD));
        assert_eq!(
            every.receive_message(&EnvelopeSender::User { via: None }, "hi"),
            Input::line("<from-user>hi</from-user>")
        );
        assert_eq!(every.interrupt(&session()), None);
    }

    #[test]
    fn an_answer_of_the_wrong_shape_and_an_error_the_engine_answers() {
        let fixture = Fixture::new(
            r#"while IFS= read -r line; do
  case "$line" in
    '{"method":"configure"'*) echo '{"error":{"kind":"refused","message":"no such model"}}' ;;
    '{"method":"prepare"'*) echo '{"error":{"kind":"io","message":"denied","path":"/h/.claude.json"}}' ;;
    *) echo '{"ok":7}' ;;
  esac
done"#,
        );
        let engine = fixture.engine(&Fixture::section(&[EngineMethod::Prepare]));
        let (session, profile) = (session(), profile());
        // What the engine refuses is its own word, and the program stays.
        assert_eq!(
            refusal(engine.configure(&session, &json!({"model": "x"}))),
            "no such model"
        );
        let error = engine.prepare(&session).unwrap_err();
        assert!(
            matches!(&error, EngineError::Io { path, .. } if path == Path::new("/h/.claude.json")),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "/h/.claude.json: denied");
        let message = refusal(engine.start(&LaunchRequest::new(&session, &profile), "hi"));
        assert!(
            message.starts_with("engine fake: bad answer to start: "),
            "{message}"
        );
    }

    #[test]
    fn a_program_that_ignores_the_end_of_its_input_is_killed_after_a_second() {
        let fixture = Fixture::new(
            r#"echo "$$" >> "$TX_PLUGIN_DIR/starts"
while IFS= read -r line; do echo '{"ok":{}}'; done
exec sleep 30"#,
        );
        let engine = fixture.engine(&Fixture::section(&[]));
        engine.configure(&session(), &json!({})).unwrap();
        assert!(fixture.alive());
        let dropped = Instant::now();
        drop(engine);
        let waited = dropped.elapsed();
        assert!(
            waited >= ENGINE_GRACE && waited < ENGINE_GRACE + Duration::from_secs(4),
            "{waited:?}"
        );
        assert!(!fixture.alive());
    }
}
