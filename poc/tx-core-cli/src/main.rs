//! `txc`: the thin CLI over tx-core. Exit 0 on success, 1 on a core error (`txc: <error>` on
//! stderr), 2 on misuse (the usage line on stderr). `attach` replaces the process with tmux.

mod args;
mod plugins;

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;

use args::{Invocation, LaunchSpec, Verb};
use tx_core::{
    CoreConfig, CoreError, CreateOptions, LaunchOptions, ListFilter, LlmEngineHook, PluginConfig,
    RefreshResult, SendOptions, SessionId, TxCore,
};

const HOME_ENV: &str = "TX_CORE_HOME";
const DEFAULT_HOME: &str = ".tx-core";
/// Under the state dir: the roles plugins ship (written by `ui install`) and the user's own.
const AGENTS_DIR: &str = "agents";
const USER_AGENTS_DIR: &str = "user-agents";
const SKILLS_DIR: &str = "skills";

fn main() -> ExitCode {
    let args: Result<Vec<String>, args::UsageError> = std::env::args_os()
        .skip(1)
        .map(|arg| {
            arg.into_string()
                .map_err(|bad| args::UsageError(format!("argument is not UTF-8: {bad:?}")))
        })
        .collect();
    let invocation = match args.and_then(|args| args::parse(&args)) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("txc: {error}\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };
    match run(invocation) {
        Ok(code) => code,
        Err(Failure::Usage(error)) => {
            eprintln!("txc: {error}\n{}", args::USAGE);
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("txc: {error}");
            ExitCode::from(1)
        }
    }
}

/// Why `run` stopped: usage is exit 2 with the usage text, everything else exit 1.
#[derive(Debug)]
enum Failure {
    Usage(args::UsageError),
    Core(CoreError),
    Plugin(tx_core::PluginError),
    Verb(tx_core::VerbError),
}

impl From<CoreError> for Failure {
    fn from(error: CoreError) -> Self {
        Failure::Core(error)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Usage(error) => error.fmt(f),
            Failure::Core(error) => error.fmt(f),
            Failure::Plugin(error) => error.fmt(f),
            Failure::Verb(error) => error.fmt(f),
        }
    }
}

/// `--state-dir`, else `$TX_CORE_HOME` (which every session carries), else `~/.tx-core`; made
/// absolute so the socket path exported into sessions does not depend on the caller's cwd.
fn state_dir(explicit: Option<PathBuf>) -> PathBuf {
    let dir = explicit
        .or_else(|| {
            std::env::var_os(HOME_ENV)
                .filter(|home| !home.is_empty())
                .map(PathBuf::from)
        })
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(DEFAULT_HOME)
        });
    absolute(dir)
}

fn absolute(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        return path;
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(&path))
        .unwrap_or(path)
}

/// Without `--profile-dir`: the roles `ui install` materialises, then the user's own.
fn default_profile_dirs(state: &Path) -> Vec<PathBuf> {
    vec![state.join(AGENTS_DIR), state.join(USER_AGENTS_DIR)]
}

/// Without `--skill-dir`: `skills/` under each default profile dir.
fn default_skill_dirs(state: &Path) -> Vec<PathBuf> {
    default_profile_dirs(state)
        .into_iter()
        .map(|dir| dir.join(SKILLS_DIR))
        .collect()
}

/// The core config from the globals; pure, touches no disk.
fn core_config(invocation: &Invocation) -> CoreConfig {
    let global = &invocation.global;
    let state = state_dir(global.state_dir.clone());
    let profile_dirs = if global.profile_dirs.is_empty() {
        default_profile_dirs(&state)
    } else {
        global.profile_dirs.clone()
    };
    let skill_dirs = if global.skill_dirs.is_empty() {
        default_skill_dirs(&state)
    } else {
        global.skill_dirs.clone()
    };
    let mut config = CoreConfig::new(state)
        .profile_dirs(profile_dirs)
        .skill_dirs(skill_dirs);
    // The hooks a session's engine wires call back into this txc. Without a path to it the
    // session still launches; it just stays `starting`.
    if let Ok(exe) = std::env::current_exe() {
        config = config.hook_command([
            exe.to_string_lossy().into_owned(),
            "chat".into(),
            "hook".into(),
        ]);
    }
    for (key, value) in &global.env {
        config = config.env(key, value);
    }
    // An explicit socket wins; inside a session the exported one applies when no state dir was
    // named on the command line (the two were exported together).
    let socket = global.socket.clone().map(absolute).or_else(|| {
        if global.state_dir.is_some() {
            return None;
        }
        std::env::var_os(tx_core::core::TMUX_SOCKET_ENV)
            .filter(|socket| !socket.is_empty())
            .map(PathBuf::from)
    });
    if let Some(socket) = socket {
        config = config.tmux_socket(socket);
    }
    config
}

/// `--plugins`, else `<state_dir>/plugins.yaml`; reads the file, creates nothing.
fn plugin_config(invocation: &Invocation, config: &CoreConfig) -> Result<PluginConfig, Failure> {
    let yaml = invocation
        .global
        .plugins
        .clone()
        .map(absolute)
        .unwrap_or_else(|| config.state_dir.join(plugins::FILE_NAME));
    plugins::load(&yaml, &plugins::known_ids(), plugins::RESERVED).map_err(Failure::Usage)
}

/// The plugin verbs this invocation would see, without opening the core (so `help` and an
/// unknown verb never create the state dir).
fn preview_verbs(invocation: &Invocation) -> Result<Vec<Arc<dyn tx_core::Verb>>, Failure> {
    let config = core_config(invocation);
    let cfg = plugin_config(invocation, &config)?;
    tx_core::plugin::preview_verbs(&plugins::manifest(), &cfg, &config).map_err(Failure::Plugin)
}

fn open(invocation: &Invocation) -> Result<TxCore, Failure> {
    let config = core_config(invocation);
    let cfg = plugin_config(invocation, &config)?;
    let mut core = TxCore::open(config)?;
    core.plug(plugins::manifest(), &cfg)
        .map_err(Failure::Plugin)?;
    Ok(core)
}

/// The caller's own session id, only when this process runs inside tx's tmux server: `$TMUX`
/// names the server socket, and `$TX_SESSION_ID` was set by `create_session`. Anywhere else the
/// sender is the user.
fn sender_from_env(core: &TxCore) -> Option<SessionId> {
    let tmux = std::env::var("TMUX").ok()?;
    let socket = tmux.split(',').next()?;
    if std::path::Path::new(socket) != core.socket_path() {
        return None;
    }
    std::env::var("TX_SESSION_ID").ok()?.parse().ok()
}

/// A hook's payload, tolerantly: `{}` for none, unreadable, bad JSON or a non-object. The hook
/// must never fail the agent over its own input.
fn read_payload(stdin: &mut dyn std::io::Read) -> serde_json::Value {
    let mut text = String::new();
    if stdin.read_to_string(&mut text).is_err() {
        return serde_json::json!({});
    }
    match serde_json::from_str(&text) {
        Ok(value @ serde_json::Value::Object(_)) => value,
        _ => serde_json::json!({}),
    }
}

fn run(invocation: Invocation) -> Result<ExitCode, Failure> {
    if invocation.verb == Verb::Help {
        print!("{}", args::USAGE);
        // A broken plugins.yaml still gets the built-in usage.
        if let Ok(verbs) = preview_verbs(&invocation)
            && !verbs.is_empty()
        {
            println!("\nplugin verbs:");
            for verb in verbs {
                println!("  {}", verb.usage());
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    if let Verb::Plugin { name, .. } = &invocation.verb
        && !preview_verbs(&invocation)?
            .iter()
            .any(|verb| verb.name() == name)
    {
        return Err(Failure::Usage(args::UsageError(format!(
            "unknown verb {name:?}"
        ))));
    }
    let core = open(&invocation)?;
    let mut out = std::io::stdout().lock();
    match invocation.verb {
        Verb::Help => unreachable!(),
        Verb::Engines => {
            for id in core.engine_ids() {
                writeln!(out, "{id}").ok();
            }
        }
        Verb::Plugins => {
            for (id, status) in core.plugin_status() {
                writeln!(out, "{id}\t{}", status.as_str()).ok();
            }
        }
        Verb::Plugin { name, argv } => {
            let verb = core.verb(&name).expect("checked by preview_verbs");
            drop(out);
            let (mut stdin, mut stdout, mut stderr) = (
                std::io::stdin().lock(),
                std::io::stdout().lock(),
                std::io::stderr().lock(),
            );
            let mut io = tx_core::VerbIo {
                stdin: &mut stdin,
                stdout: &mut stdout,
                stderr: &mut stderr,
            };
            let code = verb.run(&core, &mut io, &argv).map_err(Failure::Verb)?;
            return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
        }
        Verb::ChatHook { name } => {
            // Not ours (outside a session, a malformed id, no such record): a silent success.
            let Some(id) = std::env::var(tx_core::core::SESSION_ID_ENV)
                .ok()
                .and_then(|raw| raw.parse::<SessionId>().ok())
            else {
                return Ok(ExitCode::SUCCESS);
            };
            let hook = LlmEngineHook {
                name,
                payload: read_payload(&mut std::io::stdin().lock()),
                at: tx_core::events::now(),
            };
            match core.hook(&id, hook) {
                Ok(_) | Err(CoreError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Verb::Create {
            name,
            cwd,
            parent,
            tags,
            env,
            launch,
        } => {
            let parent = match parent {
                Some(parent) => Some(core.resolve(&parent)?),
                None => None,
            };
            let launch = match launch {
                LaunchSpec::Agent {
                    engine,
                    roles,
                    model,
                    effort,
                    prompt,
                    resume,
                } => {
                    let mut launch = LaunchOptions::agent(engine).profile(roles);
                    if let Some(resume) = resume {
                        launch = launch.resume(resume);
                    }
                    if let Some(model) = model {
                        launch = launch.model(model);
                    }
                    if let Some(effort) = effort {
                        launch = launch.effort(effort);
                    }
                    if let Some(prompt) = prompt {
                        launch = launch.initial_prompt(prompt);
                    }
                    launch
                }
                LaunchSpec::Process { program, args } => LaunchOptions::process(program, args),
            };
            let mut opts = CreateOptions::new(name, cwd, launch).tags(tags);
            if let Some(parent) = parent {
                opts = opts.parent(parent);
            }
            for (key, value) in env {
                opts = opts.env(key, value);
            }
            let id = core.create_session(opts)?;
            writeln!(out, "{id}").ok();
        }
        Verb::Ls {
            parent,
            tag,
            lifecycle,
        } => {
            let parent = match parent {
                Some(parent) => Some(core.resolve(&parent)?),
                None => None,
            };
            let mut filter = ListFilter::new();
            if let Some(parent) = parent {
                filter = filter.parent(parent);
            }
            if let Some(tag) = tag {
                filter = filter.tag(tag);
            }
            if let Some(lifecycle) = lifecycle {
                filter = filter.lifecycle(lifecycle);
            }
            for id in core.list_sessions(&filter)? {
                let session = core.get_session(&id)?;
                writeln!(
                    out,
                    "{}\t{}\t{}\t{}",
                    session.id,
                    session.name,
                    session.lifecycle.as_str(),
                    session.cwd.display()
                )
                .ok();
            }
        }
        Verb::Show { target } => {
            let session = core.get_session(&core.resolve(&target)?)?;
            let text = session.to_json().map_err(|error| {
                CoreError::Store(tx_core::StoreError::Encode {
                    id: session.id.clone(),
                    source: error,
                })
            })?;
            write!(out, "{text}").ok();
        }
        Verb::Rename { target, name } => {
            let session = core.rename_session(&core.resolve(&target)?, &name)?;
            writeln!(out, "{}\t{}", session.id, session.name).ok();
        }
        Verb::Tag { target, tags } => {
            let session = core.set_session_tags(&core.resolve(&target)?, tags)?;
            writeln!(out, "{}\t{}", session.id, session.tags.join(",")).ok();
        }
        Verb::Send { target, text } => {
            let sender = sender_from_env(&core);
            let options = sender.map_or_else(SendOptions::default, SendOptions::from_agent);
            let receipt = core.send_message(&core.resolve(&target)?, &text, options)?;
            writeln!(out, "{}\t{}", receipt.recipient, receipt.pane).ok();
        }
        Verb::Refresh { target } => {
            let changed = match target {
                Some(target) => match core.refresh_session(&core.resolve(&target)?)? {
                    RefreshResult::Exited(session) => vec![session],
                    _ => vec![],
                },
                None => core.refresh_all()?,
            };
            for session in changed {
                writeln!(
                    out,
                    "{}\t{}\t{}",
                    session.id,
                    session.name,
                    session.lifecycle.as_str()
                )
                .ok();
            }
        }
        Verb::Where { target } => {
            let location = core.get_execution_location(&core.resolve(&target)?)?;
            writeln!(
                out,
                "{}\t{}\t{}",
                location.socket.display(),
                location.session_target,
                location.pane_target
            )
            .ok();
        }
        Verb::Attach { target } => {
            let location = core.get_execution_location(&core.resolve(&target)?)?;
            drop(out);
            let error = Command::new("tmux")
                .arg("-S")
                .arg(&location.socket)
                .arg("-f")
                .arg(core.config().tmux_conf_path())
                .args(["attach-session", "-t", &location.session_target])
                .env_remove("TMUX")
                .exec();
            return Err(Failure::Core(CoreError::Tmux(tx_core::TmuxError::Spawn {
                binary: "tmux".into(),
                source: error,
            })));
        }
        Verb::Stop { target } => {
            let session = core.stop_session(&core.resolve(&target)?)?;
            writeln!(out, "{}\t{}", session.id, session.lifecycle.as_str()).ok();
        }
        Verb::Forget { target } => {
            let id = core.resolve(&target)?;
            let existed = core.forget_session(&id)?;
            writeln!(
                out,
                "{}\t{}",
                id,
                if existed { "forgotten" } else { "absent" }
            )
            .ok();
        }
    }
    Ok(ExitCode::SUCCESS)
}
