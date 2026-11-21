//! tx-core plugin `switch`: plugins on and off without editing `config.yaml` by hand.
//! `tx switch <id> on|off` writes `plugins.<id>.enabled` ([`edit`]) and runs `tx update`, which
//! publishes or retires the plugin's keys and files; `tx switch` alone picks in a list
//! ([`list`]), the popup of prefix+P. Publishes `keys/switch.tmux`.

pub mod edit;
pub mod list;

use std::process::Stdio;
use std::sync::Arc;

use tx_core::{Plugin, PluginError, Registry, TxCore, Verb, VerbError, VerbIo};
use tx_plugin::args::Global;

pub const PLUGIN_ID: &str = "switch";

#[derive(Clone, Copy, Debug, Default)]
pub struct SwitchPlugin;

impl Plugin for SwitchPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(Switch));
        Ok(())
    }
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

pub struct Switch;

impl Verb for Switch {
    fn name(&self) -> &'static str {
        "switch"
    }

    fn usage(&self) -> &'static str {
        "switch [<id> on|off]   switch a plugin in config.yaml, then tx update; no arguments: pick in a list (prefix+P)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        let mut plugins = core.plugin_status().to_vec();
        plugins.sort_by(|left, right| left.0.cmp(&right.0));
        let changes = match argv {
            [] => {
                let changes = list::run(&plugins).map_err(failed)?;
                if changes.is_empty() {
                    return Ok(0);
                }
                changes
            }
            [id, state] if matches!(state.as_str(), "on" | "off") => {
                if !plugins.iter().any(|(known, _)| known == id) {
                    let _ = writeln!(io.stderr, "tx switch: no plugin named {id:?}");
                    return Ok(1);
                }
                vec![(id.clone(), state == "on")]
            }
            _ => {
                let _ = writeln!(io.stderr, "usage: tx switch [<id> on|off]");
                return Ok(2);
            }
        };
        let code = apply(core, io, &changes)?;
        if argv.is_empty() {
            let _ = write!(io.stdout, "\nEnter closes ");
            let _ = io.stdout.flush();
            let _ = io.stdin.read(&mut [0]);
        }
        Ok(code)
    }
}

/// Writes `changes` into the config file, then runs `tx update` and passes its output on.
fn apply(core: &TxCore, io: &mut VerbIo<'_>, changes: &[(String, bool)]) -> Result<i32, VerbError> {
    let path = tx_plugin::setup::config_file(&Global::default(), core.config());
    let mut text = match std::fs::read_to_string(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        read => read.map_err(|error| failed(format!("{}: {error}", path.display())))?,
    };
    for (id, enabled) in changes {
        text = edit::set_enabled(&text, id, *enabled);
    }
    let entries = tx_plugin::config::entries(
        &tx_plugin::config::parse(&text, &path)
            .map_err(failed)?
            .plugins,
    );
    if let Some((id, _)) = changes
        .iter()
        .find(|(id, enabled)| entries[id].enabled != *enabled)
    {
        return Err(failed(format!(
            "{}: could not switch {id}; edit it by hand",
            path.display()
        )));
    }
    std::fs::write(&path, text).map_err(|error| failed(format!("{}: {error}", path.display())))?;
    for (id, enabled) in changes {
        let _ = writeln!(io.stdout, "{id}: {}", if *enabled { "on" } else { "off" });
    }
    let output = std::process::Command::new(tx_plugin::sdk::tx_bin())
        .args(core.config().cli_arguments())
        .arg("update")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| failed(format!("tx update: {error}")))?;
    let _ = io.stdout.write_all(&output.stdout);
    let _ = io.stdout.write_all(&output.stderr);
    Ok(output.status.code().unwrap_or(1))
}
