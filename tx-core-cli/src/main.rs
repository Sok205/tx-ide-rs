//! `tx`: the thin CLI over tx-core. Exit 0 on success, 1 on a core error (`tx: <error>` on
//! stderr), 2 on misuse (the usage line on stderr). `attach` replaces the process with tmux.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use tx_client::tmux::Ui;
use tx_core::{
    AgentId, CoreError, CreateOptions, ForkOptions, LaunchOptions, ListFilter, SendOptions, Sender,
    TxCore,
};
use tx_core_cli::args::{self, Invocation, Verb};

use tx_core_cli::setup::{Failure, config_file, core_config, load_config, open, preview_verbs};

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
            eprintln!("tx: {error}\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };
    match run(invocation) {
        Ok(code) => code,
        Err(Failure::Usage(error)) => {
            eprintln!("tx: {error}\n{}", args::USAGE);
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("tx: {error}");
            ExitCode::from(1)
        }
    }
}

/// Who `send` speaks as. Inside tx's tmux server (`$TMUX` names its socket): the agent whose
/// `$TX_SESSION_ID` `create_agent` set, when core knows it; else the user from the view this
/// pane belongs to. Otherwise the user, from `--via` when given.
fn sender_from_env(core: &TxCore, via: Option<String>) -> Result<Sender, CoreError> {
    let ui = Ui::from_core(core);
    if !ui.inside() {
        return Ok(Sender::User { via });
    }
    if let Some(id) = std::env::var("TX_SESSION_ID")
        .ok()
        .and_then(|id| id.parse::<AgentId>().ok())
    {
        match core.get_agent(&id) {
            Ok(_) => return Ok(Sender::Agent { id }),
            Err(CoreError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
    }
    let view = std::env::var("TMUX_PANE")
        .ok()
        .and_then(|pane| ui.display_message("#S", Some(&pane)))
        .filter(|session| ui.is_view(session));
    Ok(Sender::User { via: view.or(via) })
}

fn run(invocation: Invocation) -> Result<ExitCode, Failure> {
    if invocation.verb == Verb::Help {
        print!("{}", args::USAGE);
        if let Ok(config) = core_config(&invocation.global) {
            println!(
                "Config file here: {}",
                config_file(&invocation.global, &config).display()
            );
        }
        // A broken config.yaml still gets the built-in usage.
        if let Ok(verbs) = preview_verbs(&invocation.global)
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
        && !preview_verbs(&invocation.global)?
            .iter()
            .any(|verb| verb.name() == name)
    {
        return Err(Failure::Usage(args::UsageError(format!(
            "unknown verb {name:?}"
        ))));
    }
    let core = open(&invocation.global)?;
    let mut out = std::io::stdout().lock();
    match invocation.verb {
        Verb::Help => unreachable!(),
        Verb::Engines => {
            for kind in core.engine_kinds() {
                writeln!(out, "{kind}").ok();
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
        Verb::Create {
            name,
            cwd,
            parent,
            tags,
            env,
            engine,
            roles,
            options,
            prompt,
        } => {
            // Records whose session vanished without a hook (a crash, a `kill -9`, a reboot)
            // read `ended` before they are listed.
            core.refresh_all()?;
            let defaults = load_config(&invocation.global, core.config())?.agent;
            let engine = engine.unwrap_or(defaults.engine);
            let roles = if roles.is_empty() {
                defaults.roles
            } else {
                roles
            };
            // The file's model and effort go first, so a flag for the same key wins.
            let file_options = [("model", defaults.model), ("effort", defaults.effort)]
                .into_iter()
                .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)));
            let options: Vec<(String, String)> = file_options.chain(options).collect();
            let parent = match parent {
                Some(parent) => Some(core.resolve(&parent)?),
                None => None,
            };
            let mut launch = LaunchOptions::agent(engine)
                .profile(roles)
                .initial_prompt(prompt);
            for (key, value) in options {
                launch = launch.option(key, value);
            }
            let mut opts = CreateOptions::new(name, cwd, launch).tags(tags);
            if let Some(parent) = parent {
                opts = opts.parent(parent);
            }
            for (key, value) in env {
                opts = opts.env(key, value);
            }
            let id = core.create_agent(opts)?;
            writeln!(out, "{id}").ok();
        }
        Verb::Fork {
            source,
            name,
            prompt,
            tags,
            options: engine_options,
        } => {
            let mut options = ForkOptions::new(prompt);
            if let Some(name) = name {
                options = options.name(name);
            }
            if !tags.is_empty() {
                options = options.tags(tags);
            }
            for (key, value) in engine_options {
                options = options.option(key, value);
            }
            let id = core.fork_agent(&core.resolve(&source)?, options)?;
            writeln!(out, "{id}").ok();
        }
        Verb::Ls {
            parent,
            tag,
            ended,
            json,
        } => {
            // Records whose session vanished without a hook (a crash, a `kill -9`, a reboot)
            // read `ended` before they are listed.
            core.refresh_all()?;
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
            filter = if ended {
                filter.ended()
            } else {
                filter.running()
            };
            let ids = core.list_agents(&filter)?;
            if json {
                let mut entries = Vec::with_capacity(ids.len());
                for id in ids {
                    let session = core.get_agent(&id)?;
                    entries.push(serde_json::json!({
                        "id": session.id.as_str(),
                        "name": session.name,
                        "state": session.state.as_str(),
                        "working_directory": session.working_directory,
                        "tags": session.tags,
                    }));
                }
                writeln!(out, "{}", serde_json::Value::Array(entries)).ok();
            } else {
                for id in ids {
                    let session = core.get_agent(&id)?;
                    writeln!(
                        out,
                        "{}\t{}\t{}\t{}",
                        session.id,
                        session.name,
                        session.state.as_str(),
                        session.working_directory.display()
                    )
                    .ok();
                }
            }
        }
        Verb::Show { target } => {
            let session = core.refresh_agent(&core.resolve(&target)?)?.agent().clone();
            let text = session.to_json().map_err(|error| {
                CoreError::Store(tx_core::StoreError::Encode {
                    id: session.id.clone(),
                    source: error,
                })
            })?;
            write!(out, "{text}").ok();
        }
        Verb::Rename { target, name } => {
            let session = core.rename_agent(&core.resolve(&target)?, &name)?;
            writeln!(out, "{}\t{}", session.id, session.name).ok();
        }
        Verb::Tag { target, tags } => {
            let session = core.set_agent_tags(&core.resolve(&target)?, tags)?;
            writeln!(out, "{}\t{}", session.id, session.tags.join(",")).ok();
        }
        Verb::Send { target, text, via } => {
            let options = SendOptions::from(sender_from_env(&core, via)?);
            let receipt = core.send_message(&core.resolve(&target)?, &text, options)?;
            writeln!(out, "{}\t{}", receipt.recipient, receipt.pane).ok();
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
            let session = core.stop_agent(&core.resolve(&target)?)?;
            writeln!(out, "{}\t{}", session.id, session.state.as_str()).ok();
        }
        Verb::Interrupt { target } => {
            let id = core.resolve(&target)?;
            core.interrupt_agent(&id)?;
            writeln!(out, "{id}\tinterrupted").ok();
        }
        Verb::Respawn { target } => {
            let session = core.respawn_agent(&core.resolve(&target)?)?;
            writeln!(out, "{}\t{}", session.id, session.name).ok();
        }
    }
    Ok(ExitCode::SUCCESS)
}
