//! `tx gc`: deletes the build output (`worktree.build_outputs`) in the worktrees of agents that
//! ended longer ago than a threshold. The worktree, its branch, the record and the conversation
//! stay, so `tx respawn` still works and the agent just rebuilds.
//!
//! The paths come from the user's config, which already refuses absolute paths, `..` and `.git`.
//! Here each one is checked again on disk: no symlink is ever followed (a path that is one, or
//! lies under one, is left alone), and git must track nothing under it.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tx_core::{CoreError, ListFilter, TxCore};

use crate::config::BuildOutput;

/// A worktree whose agents all ended before the threshold.
#[derive(Debug)]
pub struct Worktree {
    pub directory: PathBuf,
    /// The ended agents that ran here, by name.
    pub agents: Vec<String>,
}

/// A configured path `clean` left alone, and why.
#[derive(Debug)]
pub struct Refusal {
    pub path: PathBuf,
    pub reason: &'static str,
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// Bytes on disk under the paths deleted (or, on a dry run, that would be).
    pub bytes: u64,
    /// Whether any configured path existed.
    pub found: bool,
    pub refusals: Vec<Refusal>,
    /// The paths that could not be measured or deleted; the others still were.
    pub failures: Vec<GcError>,
}

/// What became of one configured path.
enum Verdict {
    Missing,
    Refused(&'static str),
    Freed(u64),
}

#[derive(Debug, thiserror::Error)]
pub enum GcError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("git ls-files in {directory}: {detail}")]
    Git { directory: PathBuf, detail: String },
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> GcError + '_ {
    move |source| GcError::Io {
        path: path.to_owned(),
        source,
    }
}

/// The worktrees of agents ended longer ago than `older_than`, each once. Only directories under
/// `<data>/worktrees` count (core made those). A worktree that a running agent uses, or one that
/// ended within the threshold, is left alone. The end time is the record's `last_modified_at`:
/// the save that marked it ended, or a later edit, so an agent never reads as ended earlier than
/// it did.
pub fn ended_worktrees(core: &TxCore, older_than: Duration) -> Result<Vec<Worktree>, CoreError> {
    // A session that vanished without a hook reads `ended` (and is stamped now) before the lists.
    core.refresh_all()?;
    let worktrees_dir = canonical_or_self(&core.config().worktrees_dir());
    let cutoff = tx_core::events::now() - older_than.as_secs_f64();
    let mut busy = HashSet::new();
    for id in core.list_agents(&ListFilter::new().running())? {
        busy.insert(canonical_or_self(&core.get_agent(&id)?.working_directory));
    }
    let mut ended = Vec::new();
    for id in core.list_agents(&ListFilter::new().ended())? {
        let agent = core.get_agent(&id)?;
        let directory = canonical_or_self(&agent.working_directory);
        if agent.last_modified_at > cutoff {
            busy.insert(directory);
        } else {
            ended.push((directory, agent.name));
        }
    }
    let mut worktrees: Vec<Worktree> = Vec::new();
    for (directory, name) in ended {
        let made_by_core = directory
            .parent()
            .is_some_and(|parent| parent.starts_with(&worktrees_dir));
        if !made_by_core || busy.contains(&directory) {
            continue;
        }
        match worktrees
            .iter_mut()
            .find(|worktree| worktree.directory == directory)
        {
            Some(worktree) => worktree.agents.push(name),
            None => worktrees.push(Worktree {
                directory,
                agents: vec![name],
            }),
        }
    }
    Ok(worktrees)
}

fn canonical_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// Deletes each of `build_outputs` that exists in `worktree` (canonical), unless it is or lies
/// under a symlink, or git tracks something under it; with `dry_run`, only measures. A path that
/// fails is recorded and the rest go on.
pub fn clean(worktree: &Path, build_outputs: &[BuildOutput], dry_run: bool) -> Outcome {
    let mut outcome = Outcome::default();
    let mut seen = HashSet::new();
    for output in build_outputs {
        let path = worktree.join(output.path());
        match clean_path(worktree, output.path(), dry_run, &mut seen) {
            Ok(Verdict::Missing) => {}
            Ok(Verdict::Refused(reason)) => {
                outcome.found = true;
                outcome.refusals.push(Refusal { path, reason });
            }
            Ok(Verdict::Freed(bytes)) => {
                outcome.found = true;
                outcome.bytes += bytes;
            }
            Err(error) => {
                outcome.found = true;
                outcome.failures.push(error);
            }
        }
    }
    outcome
}

fn clean_path(
    worktree: &Path,
    relative: &Path,
    dry_run: bool,
    seen: &mut HashSet<(u64, u64)>,
) -> Result<Verdict, GcError> {
    // Walk down one name at a time without following links: the config allows plain names only,
    // so a path free of symlinks stays inside the worktree.
    let mut path = worktree.to_owned();
    let mut metadata = None;
    for component in relative.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(found) if found.file_type().is_symlink() => {
                return Ok(Verdict::Refused("it is a symlink or lies under one"));
            }
            Ok(found) => metadata = Some(found),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Verdict::Missing);
            }
            Err(source) => return Err(GcError::Io { path, source }),
        }
    }
    if tracked(worktree, relative)? {
        return Ok(Verdict::Refused("git tracks files under it"));
    }
    let bytes = disk_usage(&path, seen)?;
    if !dry_run {
        let removed = if metadata.is_some_and(|found| found.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.map_err(io_error(&path))?;
    }
    Ok(Verdict::Freed(bytes))
}

/// Whether git tracks `relative` or anything under it in `worktree`.
fn tracked(worktree: &Path, relative: &Path) -> Result<bool, GcError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["ls-files", "-z", "--"])
        .arg(relative)
        .output()
        .map_err(io_error(worktree))?;
    if !output.status.success() {
        return Err(GcError::Git {
            directory: worktree.to_owned(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(!output.stdout.is_empty())
}

/// The bytes allocated on disk under `path`, symlinks not followed; a file with several hard
/// links counts once (`seen` holds device and inode).
fn disk_usage(path: &Path, seen: &mut HashSet<(u64, u64)>) -> Result<u64, GcError> {
    let metadata = std::fs::symlink_metadata(path).map_err(io_error(path))?;
    let mut bytes = 0;
    if seen.insert((metadata.dev(), metadata.ino())) {
        bytes += metadata.blocks() * 512;
    }
    if metadata.is_dir() {
        for entry in std::fs::read_dir(path).map_err(io_error(path))? {
            bytes += disk_usage(&entry.map_err(io_error(path))?.path(), seen)?;
        }
    }
    Ok(bytes)
}

/// `1.7 GB`, `312.0 MB`, `4.0 KB`, `0 B`: decimal units, as `du -h --si` and Finder count.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_in_decimal_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(4096), "4.1 KB");
        assert_eq!(human_size(1_700_000_000), "1.7 GB");
        assert_eq!(human_size(21_000_000_000_000), "21.0 TB");
    }
}
