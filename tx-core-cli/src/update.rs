//! `tx update`: lays the shipped plugins out when it is given a checkout's bundles and build,
//! runs the core's pending update steps, publishes under `<data>` the resources of core and of
//! every active plugin, then runs every plugin's updater, its `update` program; and the warning
//! every other verb prints while a step is pending.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::Arc;

use tx_core::config::SHIPPED_PLUGINS_DIR;
use tx_core::plugin::{Installed, Launcher, ManifestProgram, PLUGIN_API, PluginState};
use tx_core::updates::{self, Applied};
use tx_core::{CoreConfig, Directories, PluginStatus, roles};
use tx_plugin::bundle::{self, Program, Source, Stamp};
use tx_plugin::sdk::{RESPAWN_LINE, RETIRE_ENV};

use crate::args::{Global, UsageError};
use crate::setup::{Failure, Installation, absolute, core_config, exec_context};

const RESPAWN: &str = "respawn running agents to pick this up";
/// What a program is run with once after it was written, and the role the trace gives that run.
const PROBE: &str = "--tx-probe";
const TRACE_PROBE: &str = "probe";
/// The role the trace gives a plugin's `update` program, retiring or not.
const TRACE_UPDATE: &str = "update";

/// What a run adds up to: whether something asked for the agents to be respawned, and whether
/// something failed.
#[derive(Default)]
struct Summary {
    respawn: bool,
    failed: bool,
}

impl Summary {
    /// The last line and the exit code.
    fn finish(&self, out: &mut impl Write) -> ExitCode {
        if self.respawn {
            writeln!(out, "{RESPAWN}").ok();
        }
        if self.failed {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        }
    }
}

/// The words after `update`: `[--bundles DIR --binaries DIR [--link]]`, the two directories
/// together or not at all, made absolute (links and the stamp hold them).
fn source(arguments: &[String]) -> Result<Option<Source>, UsageError> {
    let mut bundles = None;
    let mut binaries = None;
    let mut link = false;
    let mut words = arguments.iter();
    while let Some(word) = words.next() {
        let mut directory = || {
            words
                .next()
                .map(|value| absolute(PathBuf::from(value)))
                .ok_or_else(|| UsageError(format!("{word} needs a value")))
        };
        match word.as_str() {
            "--bundles" => bundles = Some(directory()?),
            "--binaries" => binaries = Some(directory()?),
            "--link" => link = true,
            _ => return Err(UsageError(format!("unexpected argument {word:?}"))),
        }
    }
    match (bundles, binaries) {
        (Some(bundles), Some(binaries)) => Ok(Some(Source {
            bundles,
            binaries,
            link,
        })),
        (None, None) if link => Err(UsageError(
            "--link needs --bundles and --binaries".to_owned(),
        )),
        (None, None) => Ok(None),
        _ => Err(UsageError(
            "--bundles and --binaries go together".to_owned(),
        )),
    }
}

/// In order. With `--bundles`, the shipped plugins are laid out in `<data>/shipped` first: a
/// source that lacks something stops the update before a byte is written. Then the core's
/// update steps, before an updater opens the core: opening it creates `<data>/agents`, which
/// would make a fresh install look used. A failed step stops there. Then the resources are
/// published; when that is refused no updater runs, since an updater works on what was
/// published. Then the updaters: the plugins' `update` programs in plugin order, an active
/// one to update and a disabled one to retire; what each prints is prefixed with its plugin id.
/// A failed updater is named and the others still run, since they are independent. Exit 1 on
/// any failure. `arguments` are the words after `update`.
pub fn run(global: &Global, arguments: &[String]) -> Result<ExitCode, Failure> {
    let source = source(arguments).map_err(Failure::Usage)?;
    let config = core_config(global)?;
    let mut out = std::io::stdout().lock();
    let mut summary = Summary::default();
    if let Some(source) = &source
        && !ship(global, &config, source, &mut out, &mut summary)?
    {
        summary.failed = true;
        return Ok(summary.finish(&mut out));
    }
    let mut any = false;
    let result = updates::apply_pending(&config.directories, |applied: Applied| {
        any = true;
        summary.respawn |= applied.outcome.respawn_agents;
        writeln!(out, "update {}: {}", applied.version, applied.description).ok();
    });
    if let Err(error) = result {
        if summary.respawn {
            writeln!(out, "{RESPAWN}").ok();
        }
        return Err(Failure::Update(error));
    }
    if !any {
        writeln!(
            out,
            "update steps are at {}: nothing pending",
            updates::latest()
        )
        .ok();
    }
    let installation = Installation::read_with(global, config)?;
    installation.cache();
    if !publish(&installation, &mut summary) {
        summary.failed = true;
        return Ok(summary.finish(&mut out));
    }
    let context = Arc::new(exec_context(global, &installation.config));
    for state in &installation.activation.plugins {
        let Some(position) = state.installed else {
            continue;
        };
        let installed = &installation.index.plugins[position];
        if let Ok(manifest) = &installed.manifest
            && let Some(update) = &manifest.update
        {
            // A manifest that reads is an active plugin's or a disabled one's.
            let retire = state.status == PluginStatus::Disabled;
            let launcher = Launcher::new(&context, &installation.config, state, installed);
            run_program(&launcher, update, retire, &mut out, &mut summary);
        }
    }
    Ok(summary.finish(&mut out))
}

/// Publishes the resources under `<data>`. The sources, in order: core's own roles and
/// definitions, then the files each active plugin's manifest lists, in plugin order; a later
/// source replaces an earlier one of the same name. A file a plugin lists and does not have is
/// named, and the rest is published all the same. `false`: nothing could be published, and why
/// is on stderr.
fn publish(installation: &Installation, summary: &mut Summary) -> bool {
    let mut resources: BTreeMap<String, Vec<u8>> = roles::SHIPPED
        .iter()
        .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
        .collect();
    for (state, installed, manifest) in installation.activation.active(&installation.index) {
        for name in &manifest.resources {
            let path = installed.directory.join(name);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    resources.insert(name.clone(), bytes);
                }
                Err(error) => {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        eprintln!("tx: update: {}: resource {name} is missing", state.id);
                    } else {
                        eprintln!("tx: update: {}: {}: {error}", state.id, path.display());
                    }
                    summary.failed = true;
                }
            }
        }
    }
    match bundle::publish(&installation.config.directories.data, &resources) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("tx: update: {error}");
            false
        }
    }
}

/// Runs the `update` program of an installed plugin, with no input: every line it prints is
/// shown after the plugin's id, except the one that asks for the agents to be respawned.
/// `retire`: the plugin stopped being active, and the program is told to take back what it
/// generated. A program that cannot start or does not exit 0 is named, and is a failure of the
/// update, not its end.
fn run_program(
    launcher: &Launcher,
    update: &ManifestProgram,
    retire: bool,
    out: &mut impl Write,
    summary: &mut Summary,
) {
    let id = launcher.id();
    let program = launcher.program(&update.bin);
    let mut command = launcher.command(TRACE_UPDATE, &program, &update.args);
    command.stdin(Stdio::null()).stdout(Stdio::piped());
    if retire {
        command.env(RETIRE_ENV, "1");
    } else {
        command.env_remove(RETIRE_ENV);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "tx: update: {id}: cannot run {}: {error}",
                program.display()
            );
            summary.failed = true;
            return;
        }
    };
    let printed = BufReader::new(child.stdout.take().expect("the output is piped"));
    for line in printed.split(b'\n').map_while(Result::ok) {
        let line = String::from_utf8_lossy(&line);
        if line == RESPAWN_LINE {
            summary.respawn = true;
        } else {
            writeln!(out, "{id}: {line}").ok();
        }
    }
    match child.wait() {
        Ok(status) if status.success() => {}
        Ok(status) => {
            // Killed by a signal: `128 + signal`, the way a shell reports it.
            let code = status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or_default());
            eprintln!("tx: update: {id}: exit {code}");
            summary.failed = true;
        }
        Err(error) => {
            eprintln!("tx: update: {id}: {error}");
            summary.failed = true;
        }
    }
}

/// Lays the shipped plugins out in `<data>/shipped` from `source`: a plugin that left the
/// selection is retired before its directory goes; one line per plugin that changed; each
/// program newly copied run once; then the stamp. `false`: it could not be done, and why is on
/// stderr; a source that lacks something has changed nothing.
fn ship(
    global: &Global,
    config: &CoreConfig,
    source: &Source,
    out: &mut impl Write,
    summary: &mut Summary,
) -> Result<bool, Failure> {
    let refused = |error: &bundle::Error| eprintln!("tx: update: {error}");
    let shipped = config.directories.data.join(SHIPPED_PLUGINS_DIR);
    // What is installed now, before anything changes; a `config.yaml` that does not read
    // stops here.
    let installation = Installation::read_with(global, config.clone())?;
    let plan = match bundle::plan(source, &shipped) {
        Ok(plan) => plan,
        Err(errors) => {
            errors.iter().for_each(refused);
            return Ok(false);
        }
    };
    let context = Arc::new(exec_context(global, config));
    for name in plan.removed() {
        if let Some((state, installed)) = installed_in(&installation, &shipped.join(name))
            && let Ok(manifest) = &installed.manifest
            && let Some(update) = &manifest.update
        {
            let launcher = Launcher::new(&context, config, state, installed);
            run_program(&launcher, update, true, out, summary);
        }
    }
    let report = match plan.apply() {
        Ok(report) => report,
        Err(error) => {
            refused(&error);
            return Ok(false);
        }
    };
    for change in &report.changes {
        let removed = if change.removed { " removed" } else { "" };
        writeln!(out, "shipped: {}{removed}", change.id).ok();
    }
    if !source.link {
        probe(global, config, &shipped, &report.programs)?;
    }
    let stamp = Stamp {
        tx_version: tx_plugin::VERSION.to_owned(),
        api: PLUGIN_API,
        bundles: source.bundles.clone(),
        binaries: source.binaries.clone(),
        linked: source.link,
    };
    if let Err(error) = stamp.write(&shipped) {
        refused(&error);
        return Ok(false);
    }
    Ok(true)
}

/// The plugin installed in `directory`, as the index and the activation have it.
fn installed_in<'a>(
    installation: &'a Installation,
    directory: &Path,
) -> Option<(&'a PluginState, &'a Installed)> {
    let position = installation
        .index
        .plugins
        .iter()
        .position(|installed| installed.directory == directory)?;
    let state = installation
        .activation
        .plugins
        .iter()
        .find(|state| state.installed == Some(position))?;
    Some((state, &installation.index.plugins[position]))
}

/// Runs each of `programs`, just copied into the shipped root, once with `--tx-probe`, all at
/// the same time: on macOS the first run of a new program file is slow, and this pays for it
/// here instead of on the first key. Whether a program answers is not this step's to judge.
fn probe(
    global: &Global,
    config: &CoreConfig,
    shipped: &Path,
    programs: &[Program],
) -> Result<(), Failure> {
    if programs.is_empty() {
        return Ok(());
    }
    let installation = Installation::read_with(global, config.clone())?;
    let context = Arc::new(exec_context(global, config));
    let arguments = [PROBE.to_owned()];
    let children: Vec<_> = programs
        .iter()
        .filter_map(|program| {
            let (state, installed) = installed_in(&installation, &shipped.join(&program.id))?;
            Launcher::new(&context, config, state, installed)
                .command(TRACE_PROBE, &program.path, &arguments)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        })
        .collect();
    for mut child in children {
        let _ = child.wait();
    }
    Ok(())
}

/// On stderr, once: update steps this tx knows have not run here. Never refuses; a stamp that
/// cannot be read is named the same way.
pub fn warn_if_behind(directories: &Directories) {
    match updates::behind(directories) {
        Ok(None) => {}
        Ok(Some((current, latest))) => {
            let pending = latest - current;
            let steps = if pending == 1 { "step" } else { "steps" };
            eprintln!("tx: {pending} update {steps} pending: run tx update");
        }
        Err(error) => eprintln!("tx: {error}"),
    }
}
