//! Plugins that run as their own programs, seen from a core: how a program of a plugin is
//! started ([`ExecContext`], [`Launcher`]) and the proxy that stands, in this process, for a
//! verb of a plugin that is not hosted in it.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;

use super::index::{Activation, Index, Installed, PluginState};
use super::manifest::{BIN_DIR, PLUGIN_API};
use super::{Verb, VerbError, VerbIo};
use crate::config::CoreConfig;
use crate::core::TxCore;

/// The role of a traced start: a verb of a plugin, run by `tx` or by a proxy.
pub const TRACE_VERB: &str = "verb";

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
        self.trace(role, program);
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

    /// One append, so lines of processes tracing into the same file never mix. A trace that
    /// cannot be written never fails what it traces.
    fn trace(&self, role: &str, program: &Path) {
        let Some(file) = &self.context.trace else {
            return;
        };
        let line = format!("{role}\t{}\t{}\n", self.id, program.display());
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
        {
            let _ = file.write_all(line.as_bytes());
        }
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
