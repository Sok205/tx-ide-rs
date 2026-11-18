//! `tx check` for plugins: what is wrong with the installed ones (a problem, exit 1) and what
//! is only odd (a warning). It reads the installation and the files under it and opens no core:
//! nothing is created and no plugin program runs.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use tx_core::config::SHIPPED_PLUGINS_DIR;
use tx_core::plugin::manifest::{BIN_DIR, MANIFEST_FILE};
use tx_core::plugin::{Manifest, PluginStatus};
use tx_core::{roles, updates};
use tx_plugin::bundle::Stamp;
use tx_plugin::config::RETIRED;
use tx_plugin::setup::{Failure, Installation};

/// What a plugin check found.
pub struct Report {
    /// Lines printed that fail the check; warnings are not counted.
    pub problems: usize,
    /// Installed plugins whose manifest loads, enabled or not.
    pub loaded: usize,
}

/// The first of `ids` that is not an installed plugin.
pub fn unknown<'a>(installation: &Installation, ids: &'a [String]) -> Option<&'a str> {
    ids.iter()
        .map(String::as_str)
        .find(|id| !installation.index.contains(id))
}

/// Prints the problems and warnings of the installed plugins, in plugin order, one line each. With
/// `ids`, only those plugins are checked, and not the config file or the shipped stamp, which
/// belong to no plugin.
pub fn plugins(
    installation: &Installation,
    config_file: &Path,
    ids: &[String],
    out: &mut dyn Write,
) -> Result<Report, Failure> {
    let mut checking = Checking {
        installation,
        out,
        problems: 0,
    };
    let mut loaded = 0;
    let mut owners: BTreeMap<&str, &str> = roles::SHIPPED
        .iter()
        .map(|(name, _)| (*name, roles::PLUGIN_ID))
        .collect();
    let index = &installation.index;
    for state in &installation.activation.plugins {
        let Some(position) = state.installed else {
            continue;
        };
        let installed = &index.plugins[position];
        if !ids.is_empty() && !ids.contains(&installed.id) {
            continue;
        }
        let path = installed.directory.join(MANIFEST_FILE);
        let Ok(manifest) = &installed.manifest else {
            checking.unreadable(position, &path);
            continue;
        };
        loaded += 1;
        if state.status != PluginStatus::Active {
            continue;
        }
        checking.active(&path, &installed.directory, manifest);
        for problem in &installation.activation.problems {
            if problem.plugin() == installed.id {
                checking.problem(&path, problem);
            }
        }
        for resource in &manifest.resources {
            if let Some(owner) = owners.insert(resource.as_str(), installed.id.as_str()) {
                checking.warning(&path, format!("{resource} replaces \"{owner}\"'s"));
            }
        }
    }
    if ids.is_empty() {
        checking.config(config_file);
        checking.stamp()?;
    }
    Ok(Report {
        problems: checking.problems,
        loaded,
    })
}

struct Checking<'a> {
    installation: &'a Installation,
    out: &'a mut dyn Write,
    problems: usize,
}

impl Checking<'_> {
    fn problem(&mut self, path: &Path, text: impl std::fmt::Display) {
        self.problems += 1;
        writeln!(self.out, "{}: {text}", path.display()).ok();
    }

    fn warning(&mut self, path: &Path, text: impl std::fmt::Display) {
        writeln!(self.out, "{}: warning: {text}", path.display()).ok();
    }

    /// A plugin whose manifest does not load: the manifest's own error, found again by reading
    /// it (the index keeps only the reason), or, for a second copy of an id, where the first is.
    fn unreadable(&mut self, position: usize, path: &Path) {
        let plugins = &self.installation.index.plugins;
        let installed = &plugins[position];
        match plugins[..position]
            .iter()
            .find(|earlier| earlier.id == installed.id)
        {
            Some(first) => {
                self.problems += 1;
                writeln!(
                    self.out,
                    "{}: plugin \"{}\" is also installed in {}",
                    installed.directory.display(),
                    installed.id,
                    first.directory.display()
                )
                .ok();
            }
            None => {
                let error = Manifest::read(path).expect_err("the index found it unreadable");
                self.problems += 1;
                writeln!(self.out, "{error}").ok();
            }
        }
    }

    /// What an active plugin needs from the machine and from the other plugins.
    fn active(&mut self, path: &Path, directory: &Path, manifest: &Manifest) {
        for name in manifest.bins() {
            let program = directory.join(BIN_DIR).join(name);
            match std::fs::metadata(&program) {
                Err(_) => self.problem(path, format!("bin/{name} is missing")),
                Ok(metadata) if metadata.permissions().mode() & 0o111 == 0 => {
                    self.problem(path, format!("bin/{name} is not executable"));
                }
                Ok(_) => {}
            }
        }
        for resource in &manifest.resources {
            if std::fs::metadata(directory.join(resource)).is_err() {
                self.problem(path, format!("resource {resource} is missing"));
            }
        }
        for required in &manifest.requires {
            let state = self
                .installation
                .activation
                .plugins
                .iter()
                .find(|state| state.installed.is_some() && state.id == *required);
            let why = match state.map(|state| &state.status) {
                None => "which is not installed",
                Some(PluginStatus::Disabled) => "which is disabled",
                Some(PluginStatus::Error(_)) => "which does not load",
                Some(_) => continue,
            };
            self.problem(path, format!("requires \"{required}\", {why}"));
        }
        for program in &manifest.needs {
            if !on_path(program) {
                self.warning(path, format!("needs {program}, which is not on PATH"));
            }
        }
    }

    /// `config.yaml`: the engine every launch defaults to must exist, and an entry that names no
    /// plugin is worth a word (never an error: a plugin may be installed on another machine).
    fn config(&mut self, file: &Path) {
        let installation = self.installation;
        // The kinds of the active plugins' engines, sorted.
        let active = &installation.activation.engines;
        let wanted = installation.file.agent.engine;
        if !active.contains_key(&wanted) {
            let names: Vec<String> = active.keys().map(ToString::to_string).collect();
            self.problem(
                file,
                format!(
                    "agent.engine \"{wanted}\" is not an active engine (active: {})",
                    names.join(", ")
                ),
            );
        }
        for state in &installation.activation.plugins {
            if state.status != PluginStatus::Missing {
                continue;
            }
            let retired = match RETIRED.contains(&state.id.as_str()) {
                true => format!(" (retired: {})", retirement(&state.id)),
                false => String::new(),
            };
            self.warning(
                file,
                format!("plugins.{}: no such plugin{retired}", state.id),
            );
        }
    }

    /// The shipped plugins must come from this tx, and an install that has data must have them.
    fn stamp(&mut self) -> Result<(), Failure> {
        let directories = &self.installation.config.directories;
        let shipped = directories.data.join(SHIPPED_PLUGINS_DIR);
        match Stamp::read(&shipped) {
            Err(error) => {
                self.problems += 1;
                writeln!(self.out, "{error}").ok();
            }
            Ok(Some(stamp)) if stamp.tx_version != tx_plugin::VERSION => self.problem(
                &shipped,
                format!(
                    "plugins are from tx {}, this is tx {}: run install --upgrade",
                    stamp.tx_version,
                    tx_plugin::VERSION
                ),
            ),
            Ok(Some(_)) => {}
            Ok(None) => {
                if !updates::is_fresh(directories).map_err(Failure::Update)? {
                    self.problem(&shipped, "no shipped plugins are installed: run install");
                }
            }
        }
        Ok(())
    }
}

/// Why an id that no plugin has any more was once one.
fn retirement(id: &str) -> &'static str {
    match id {
        "hooks" => "agent state moved into core and the engine adapters",
        "history" => "the transcript mirror; core archives conversations through the engines",
        "menu" => "renamed home; an old menu entry no longer configures anything",
        _ => unreachable!("not in RETIRED"),
    }
}

/// Whether `program` is an executable file in a directory of `PATH`.
fn on_path(program: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| is_executable(&directory.join(program)))
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_on_the_path_when_an_executable_file_of_that_name_is() {
        assert!(on_path("sh"));
        assert!(!on_path("no-such-program-anywhere"));
    }

    #[test]
    fn a_retired_id_has_a_reason() {
        for id in RETIRED {
            assert!(!retirement(id).is_empty());
        }
    }
}
