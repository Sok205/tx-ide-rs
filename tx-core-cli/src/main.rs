//! `tx`: the thin CLI over tx-core. Exit 0 on success, 1 on a core error (`tx: <error>` on
//! stderr), 2 on misuse (the usage line on stderr). `attach` replaces the process with tmux.

use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode};

use tx_client::tmux::Ui;
use tx_core::plugin::Index;
use tx_core::plugin::external::TRACE_VERB;
use tx_core::{
    AgentId, CoreError, CreateOptions, ForkOptions, IdleReason, ListFilter, SendOptions, Sender,
    TxCore,
};
use tx_core_cli::args::{self, Global, HarnessKind, Invocation, Verb};
use tx_core_cli::harness::Engines;

use tx_core_cli::plugins::warn_if_shipped_differ;
use tx_core_cli::setup::{Failure, Installation, config_file, core_config, load_config};

fn main() -> ExitCode {
    // First thing: the plugin programs and the sessions this run starts are told which `tx`
    // this is.
    if let Ok(exe) = std::env::current_exe() {
        tx_plugin::sdk::set_tx_bin(exe);
    }
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

/// `tx help`: the built-in usage, the config file this run would read, then one line per verb
/// of an active plugin, by verb name. A `config.yaml` that cannot be read still gets the
/// built-in usage.
fn help(global: &Global) {
    print!("{}", args::USAGE);
    let Ok(config) = core_config(global) else {
        return;
    };
    println!(
        "Config file here: {}",
        config_file(global, &config).display()
    );
    let Ok(installation) = Installation::read_with(global, config) else {
        return;
    };
    warn_if_shipped_differ(&installation.index, &installation.config.directories);
    let verbs = &installation.activation.verbs;
    if !verbs.is_empty() {
        println!("\nplugin verbs:");
        for verb in verbs.values() {
            println!("  {}", verb.usage);
        }
    }
}

/// A verb that is not built in: an active plugin's, or unknown. `tx` becomes that plugin's
/// program: its standard streams, terminal, signals and exit code are the plugin's own, and
/// neither a plugin nor the core is set up here.
fn plugin_verb(global: &Global, name: &str, argv: &[String]) -> Result<ExitCode, Failure> {
    let installation = Installation::read(global)?;
    let Some(verb) = installation.activation.verbs.get(name) else {
        return Err(Failure::Usage(args::UsageError(format!(
            "unknown verb {name:?}"
        ))));
    };
    // The internal `_…` verbs run from tmux hooks, where a warning is noise.
    if !name.starts_with('_') {
        tx_core_cli::update::warn_if_behind(&installation.config.directories);
        warn_if_shipped_differ(&installation.index, &installation.config.directories);
    }
    installation.cache();
    let error = installation
        .launcher(global, &verb.plugin)
        .command(TRACE_VERB, &verb.program, &verb.args)
        .args(argv)
        .exec();
    eprintln!("tx: {name}: cannot run {}: {error}", verb.program.display());
    Ok(ExitCode::from(1))
}

/// `tx plugins [--json]`: every plugin and its status. The index and `config.yaml` say it all,
/// so no core is opened.
fn plugins(global: &Global, json: bool) -> Result<ExitCode, Failure> {
    let installation = Installation::read(global)?;
    tx_core_cli::update::warn_if_behind(&installation.config.directories);
    warn_if_shipped_differ(&installation.index, &installation.config.directories);
    installation.cache();
    let rows = tx_core_cli::plugins::rows(&installation);
    let mut out = std::io::stdout().lock();
    if json {
        let rows = rows.iter().map(|row| row.json()).collect();
        writeln!(out, "{}", serde_json::Value::Array(rows)).ok();
    } else {
        for row in rows {
            writeln!(out, "{}", row.line()).ok();
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `tx harness check`: the harness, the plugins, or one of the two. Neither opens a core, and
/// nothing is written, the plugin index included.
fn harness_check(
    global: &args::Global,
    engine: Option<tx_core::EngineKind>,
    plugins_only: bool,
    names: &[String],
) -> Result<ExitCode, Failure> {
    let installation = tx_core_cli::setup::Installation::read_with(global, core_config(global)?)?;
    let mut out = std::io::stdout().lock();
    let mut clean = true;
    let mut summary = None;
    if !plugins_only {
        let engines = Engines::read(global, &installation);
        // The engine asked about is named next to the active ones before anything is printed.
        if let Some(kind) = engine
            && !engines.all().contains_key(&kind)
        {
            let enabled: Vec<String> = engines.all().keys().map(ToString::to_string).collect();
            eprintln!(
                "tx: {} (enabled: {})",
                CoreError::EngineDisabled(kind),
                enabled.join(", ")
            );
            return Ok(ExitCode::from(1));
        }
        let config = &installation.config;
        let profiles =
            tx_core::profile::Profiles::new(config.profile_dirs.clone(), config.skill_dirs.clone());
        let checked = tx_core_cli::harness::check(
            &config.directories,
            &profiles,
            &engines,
            engine,
            names,
            &mut out,
        )
        .map_err(Failure::Harness)?;
        clean = checked.clean;
        summary = Some(checked.summary);
    }
    let mut closing = summary.clone();
    if plugins_only || (engine.is_none() && names.is_empty()) {
        let ids = if plugins_only { names } else { &[] };
        if let Some(id) = tx_core_cli::check::unknown(&installation, ids) {
            eprintln!("tx harness check: no plugin {id:?}");
            return Ok(ExitCode::from(1));
        }
        let file = tx_core_cli::setup::config_file(global, &installation.config);
        let report = tx_core_cli::check::plugins(&installation, &file, ids, &mut out)?;
        clean &= report.problems == 0;
        let loaded = format!("{} plugins", report.loaded);
        closing = Some(match summary {
            Some(summary) => format!("{summary}, {loaded}"),
            None => loaded,
        });
    }
    if clean && let Some(closing) = closing {
        writeln!(out, "ok: {closing}").ok();
    }
    Ok(if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// `tx harness ls [KIND]`. What the active engines add is read only for the kinds that show
/// it, agents and tools: those read `config.yaml`, which says which engines are active.
fn harness_ls(global: &args::Global, kind: Option<HarnessKind>) -> Result<ExitCode, Failure> {
    let config = core_config(global)?;
    let directories = config.directories.clone();
    let mut out = std::io::stdout().lock();
    match kind {
        Some(HarnessKind::Skills | HarnessKind::Mcp) => {
            tx_core_cli::harness::list(&directories, kind, &[], &mut out)
        }
        _ => {
            let installation = tx_core_cli::setup::Installation::read_with(global, config)?;
            let engines = Engines::read(global, &installation);
            match kind {
                Some(HarnessKind::Tools) => {
                    tx_core_cli::tools::list(&directories, &engines, &mut out)
                }
                _ => tx_core_cli::harness::list(&directories, kind, &engines.notes(), &mut out),
            }
        }
    }
    .map_err(Failure::Harness)?;
    Ok(ExitCode::SUCCESS)
}

fn run(invocation: Invocation) -> Result<ExitCode, Failure> {
    match &invocation.verb {
        Verb::Help => {
            help(&invocation.global);
            return Ok(ExitCode::SUCCESS);
        }
        // `update` runs its steps before anything opens the core.
        Verb::Update { arguments } => {
            return tx_core_cli::update::run(&invocation.global, arguments);
        }
        Verb::Plugin { name, argv } => return plugin_verb(&invocation.global, name, argv),
        Verb::Plugins { json } => return plugins(&invocation.global, *json),
        _ => {}
    }
    let config = core_config(&invocation.global)?;
    tx_core_cli::update::warn_if_behind(&config.directories);
    // The harness is files under two roots: no core to open, nothing created. Neither
    // verb needs `config.yaml` for the warning: the index alone holds the shipped stamp.
    if matches!(
        invocation.verb,
        Verb::HarnessList { .. } | Verb::HarnessCheck { .. }
    ) {
        let index = Index::read(&config, tx_plugin::VERSION);
        warn_if_shipped_differ(&index, &config.directories);
    }
    match &invocation.verb {
        Verb::HarnessList { kind } => {
            return harness_ls(&invocation.global, *kind);
        }
        Verb::HarnessCheck {
            engine,
            plugins,
            names,
        } => return harness_check(&invocation.global, *engine, *plugins, names),
        _ => {}
    }
    let installation = Installation::read_with(&invocation.global, config)?;
    warn_if_shipped_differ(&installation.index, &installation.config.directories);
    let core = installation.open_hosted(&invocation.global, Vec::new())?;
    built_in(&core, invocation).or_else(|failure| match failure {
        // An engine kind nothing here provides is named next to the kinds that are provided.
        Failure::Core(error @ CoreError::EngineDisabled(_)) => {
            let enabled: Vec<String> = core
                .engine_kinds()
                .iter()
                .map(ToString::to_string)
                .collect();
            eprintln!("tx: {error} (enabled: {})", enabled.join(", "));
            Ok(ExitCode::from(1))
        }
        failure => Err(failure),
    })
}

/// A built-in verb that runs on the opened core.
fn built_in(core: &TxCore, invocation: Invocation) -> Result<ExitCode, Failure> {
    let mut out = std::io::stdout().lock();
    match invocation.verb {
        Verb::Help
        | Verb::Update { .. }
        | Verb::Plugin { .. }
        | Verb::Plugins { .. }
        | Verb::HarnessList { .. }
        | Verb::HarnessCheck { .. } => {
            unreachable!()
        }
        Verb::Engines => {
            for kind in core.engine_kinds() {
                writeln!(out, "{kind}").ok();
            }
        }
        Verb::Create {
            name,
            cwd,
            parent,
            tags,
            env,
            engine,
            roles,
            definition,
            options,
            engine_args,
            prompt,
        } => {
            // Records whose session vanished without a hook (a crash, a `kill -9`, a reboot)
            // read `ended` before they are listed.
            core.refresh_all()?;
            let parent = match parent {
                Some(parent) => Some(core.resolve(&parent)?),
                None => None,
            };
            // `config.yaml`'s `agent:` block fills in every flag not given.
            let defaults = &core.config().agent_defaults;
            // What the launch engine would do with the agent that its file does not let one
            // expect (Claude's whole bundle for one bundled skill): said before the launch, by
            // the engine that then launches it.
            let directories = &core.config().directories;
            if let Some(choice) = &definition
                && let Some(launch_engine) = core.engine(engine.unwrap_or(defaults.engine))
                && let Ok(agent) =
                    tx_core::harness::Harness::new(directories.harness_roots()).agent(&choice.agent)
            {
                let mut summary = agent.summary();
                summary.skills.extend(choice.skills.iter().cloned());
                let provided =
                    tx_core_cli::harness::provided_skills(directories).map_err(Failure::Harness)?;
                let request = tx_core::engine::DefinitionsRequest {
                    agents: std::slice::from_ref(&summary),
                    provided_skills: &provided,
                    cache: &directories.cache,
                };
                for warning in launch_engine.warnings(&request) {
                    eprintln!("tx: warning: {}", warning.message);
                }
            }
            let definition = definition.map(|choice| *choice);
            let mut launch = defaults.launch(engine, roles, definition, options, prompt);
            if !engine_args.is_empty() {
                launch = launch.option(tx_core::ENGINE_ARGS_OPTION, engine_args);
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
            engine_args,
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
            if !engine_args.is_empty() {
                options = options.option(tx_core::ENGINE_ARGS_OPTION, engine_args);
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
                        "idle_reason": session.idle_reason.map(IdleReason::as_str),
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
            let options = SendOptions::from(sender_from_env(core, via)?);
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
            let id = core.resolve(&target)?;
            // The resume needs the engine, and core asks it under the session lock: an engine
            // that runs as its own program is started before the lock is taken.
            if let Some(engine) = core.engine(core.get_agent(&id)?.engine) {
                engine.warm();
            }
            let session = core.respawn_agent(&id)?;
            writeln!(out, "{}\t{}", session.id, session.name).ok();
        }
        Verb::Gc {
            older_than,
            dry_run,
        } => {
            let build_outputs = load_config(&invocation.global, core.config())?
                .worktree
                .build_outputs;
            let mut total = 0;
            let mut failed = false;
            for worktree in tx_core_cli::gc::ended_worktrees(core, older_than)? {
                let outcome = tx_core_cli::gc::clean(&worktree.directory, &build_outputs, dry_run);
                for failure in &outcome.failures {
                    eprintln!("tx: gc: {failure}");
                }
                failed |= !outcome.failures.is_empty();
                for refusal in &outcome.refusals {
                    eprintln!(
                        "tx: gc: left {}: {}",
                        refusal.path.display(),
                        refusal.reason
                    );
                }
                if outcome.found {
                    total += outcome.bytes;
                    writeln!(
                        out,
                        "{}\t{}\t{}",
                        tx_core_cli::gc::human_size(outcome.bytes),
                        worktree.agents.join(","),
                        worktree.directory.display()
                    )
                    .ok();
                }
            }
            let label = if dry_run { "total, dry run" } else { "total" };
            writeln!(out, "{}\t{label}", tx_core_cli::gc::human_size(total)).ok();
            if failed {
                return Ok(ExitCode::from(1));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
