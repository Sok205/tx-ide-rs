//! One-time update steps: what a newer tx needs done once per machine (move files, rewrite a
//! setting, retire leftovers), so nobody does it by hand.
//!
//! `<data>/update-version` holds one integer, the last step applied; absent reads 0.
//! `UPDATE_STEPS` is append-only, versions strictly increasing from 1. Each step is
//! idempotent (it checks before it acts): `<config>` may be shared across machines, so a second
//! machine may find the work already done.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::config::{Directories, SHIPPED_PLUGINS_DIR, USER_HARNESS_DIR};

/// Under the data directory: the last update step applied, one integer and a newline.
pub const STAMP_FILE: &str = "update-version";

/// What an update step, or a plugin's updater, asks of the operator once it ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Running agents carry paths or environment the step changed: respawn them.
    pub respawn_agents: bool,
}

/// One update step. `run` sees only the directories, never an open core.
pub struct UpdateStep {
    pub version: u32,
    pub description: &'static str,
    pub run: fn(&Directories) -> io::Result<Outcome>,
}

/// Every update step, in order. Append only.
pub const UPDATE_STEPS: &[UpdateStep] = &[
    UpdateStep {
        version: 1,
        description: "the harness: move <config>/definitions to <config>/harness, remove <data>/definitions",
        run: definitions_to_harness,
    },
    UpdateStep {
        version: 2,
        description: "plugins run as their own programs",
        run: plugins_run_as_their_own_programs,
    },
];

/// The harness directories' name before step 1.
const DEFINITIONS_DIR: &str = "definitions";

/// Step 1: `definitions/` became `harness/`. The user's tree moves; the shipped one is written
/// again under `<data>/harness/` by the IDE's updater, so the old one goes, and a symlink there
/// goes without its target. Running agents link skills under the old path: respawn them.
fn definitions_to_harness(directories: &Directories) -> io::Result<Outcome> {
    let old = directories.config.join(DEFINITIONS_DIR);
    let new = directories.config.join(USER_HARNESS_DIR);
    if old.symlink_metadata().is_ok() {
        if new.symlink_metadata().is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "both {} and {} exist: merge the first into the second by hand, then remove it",
                    old.display(),
                    new.display()
                ),
            ));
        }
        std::fs::rename(&old, &new)?;
    }
    let shipped = directories.data.join(DEFINITIONS_DIR);
    match shipped.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&shipped)?,
        Ok(_) => std::fs::remove_file(&shipped)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(Outcome {
        respawn_agents: true,
    })
}

/// Step 2. Changes no file. Agents started before the upgrade keep reporting through `txi hook`;
/// one respawned wires its engine's own program as its hook command.
fn plugins_run_as_their_own_programs(_: &Directories) -> io::Result<Outcome> {
    Ok(Outcome {
        respawn_agents: true,
    })
}

/// A step that ran, as `apply_pending` reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Applied {
    pub version: u32,
    pub description: &'static str,
    pub outcome: Outcome,
}

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: not an update version: {text:?}", path.display())]
    Malformed { path: PathBuf, text: String },
    #[error("update step {version} ({description}) failed: {source}")]
    Failed {
        version: u32,
        description: &'static str,
        source: io::Error,
    },
}

/// The last update step this tx knows.
pub fn latest() -> u32 {
    latest_of(UPDATE_STEPS)
}

fn latest_of(steps: &[UpdateStep]) -> u32 {
    steps.last().map_or(0, |step| step.version)
}

pub fn stamp_path(directories: &Directories) -> PathBuf {
    directories.data.join(STAMP_FILE)
}

/// The stamp; absent reads 0.
pub fn current(directories: &Directories) -> Result<u32, UpdateError> {
    Ok(read_stamp(directories)?.unwrap_or(0))
}

fn read_stamp(directories: &Directories) -> Result<Option<u32>, UpdateError> {
    let path = stamp_path(directories);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(UpdateError::Io { path, source }),
    };
    match text.trim().parse() {
        Ok(version) => Ok(Some(version)),
        Err(_) => Err(UpdateError::Malformed { path, text }),
    }
}

/// Nothing to update: `<data>` is absent or empty, and `<config>` holds no subdirectory (a lone
/// `config.yaml` is the operator's settings, not tx's files). `<data>/shipped` does not count:
/// `tx setup` and `tx update` lay the shipped plugins out there before any step runs. Nor does
/// the stamp: it is tx's own note about the install, not data.
pub fn is_fresh(directories: &Directories) -> Result<bool, UpdateError> {
    let data_empty = entries(&directories.data)?
        .iter()
        .all(|path| path.ends_with(SHIPPED_PLUGINS_DIR) || path.ends_with(STAMP_FILE));
    let config_has_directory = entries(&directories.config)?
        .iter()
        .any(|path| path.is_dir());
    Ok(data_empty && !config_has_directory)
}

/// A directory's entries; an absent directory has none.
fn entries(directory: &Path) -> Result<Vec<PathBuf>, UpdateError> {
    let failed = |source| UpdateError::Io {
        path: directory.to_path_buf(),
        source,
    };
    let reader = match std::fs::read_dir(directory) {
        Ok(reader) => reader,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(failed(error)),
    };
    reader
        .map(|entry| entry.map(|entry| entry.path()).map_err(failed))
        .collect()
}

/// `(current, latest)` when steps are pending; `None` when up to date, fresh, or ahead of
/// this tx.
pub fn behind(directories: &Directories) -> Result<Option<(u32, u32)>, UpdateError> {
    behind_of(directories, UPDATE_STEPS)
}

fn behind_of(
    directories: &Directories,
    steps: &[UpdateStep],
) -> Result<Option<(u32, u32)>, UpdateError> {
    let latest = latest_of(steps);
    let current = match read_stamp(directories)? {
        Some(version) => version,
        None if is_fresh(directories)? => return Ok(None),
        None => 0,
    };
    Ok((current < latest).then_some((current, latest)))
}

/// Runs every pending step in order, writing the stamp after each, so a failure leaves it
/// at the last success; `report` hears of each as it lands. A fresh install is stamped at the
/// latest version without running any.
pub fn apply_pending(
    directories: &Directories,
    report: impl FnMut(Applied),
) -> Result<(), UpdateError> {
    apply_pending_with(directories, UPDATE_STEPS, report)
}

fn apply_pending_with(
    directories: &Directories,
    steps: &[UpdateStep],
    mut report: impl FnMut(Applied),
) -> Result<(), UpdateError> {
    let stamp = read_stamp(directories)?;
    let latest = latest_of(steps);
    let current = match stamp {
        Some(version) => version,
        None if is_fresh(directories)? => latest,
        None => 0,
    };
    for step in steps.iter().filter(|step| step.version > current) {
        let outcome = (step.run)(directories).map_err(|source| UpdateError::Failed {
            version: step.version,
            description: step.description,
            source,
        })?;
        write_stamp(directories, step.version)?;
        report(Applied {
            version: step.version,
            description: step.description,
            outcome,
        });
    }
    // An absent stamp is written even when nothing ran (fresh, or no step pending).
    if stamp.is_none() {
        write_stamp(directories, latest)?;
    }
    Ok(())
}

/// Temp file in `<data>` plus rename: a reader sees the old stamp or the new one.
fn write_stamp(directories: &Directories, version: u32) -> Result<(), UpdateError> {
    let data = &directories.data;
    let failed = |path: &Path| {
        let path = path.to_path_buf();
        move |source| UpdateError::Io { path, source }
    };
    std::fs::create_dir_all(data).map_err(failed(data))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".update-version.")
        .suffix(".tmp")
        .tempfile_in(data)
        .map_err(failed(data))?;
    writeln!(temporary, "{version}").map_err(failed(temporary.path()))?;
    let path = stamp_path(directories);
    temporary
        .persist(&path)
        .map_err(|error| failed(&path)(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (tempfile::TempDir, Directories) {
        let root = tempfile::tempdir().unwrap();
        let directories = Directories::under(root.path());
        (root, directories)
    }

    /// Not fresh: a record directory exists.
    fn used_layout() -> (tempfile::TempDir, Directories) {
        let (root, directories) = layout();
        std::fs::create_dir_all(directories.data.join("agents")).unwrap();
        (root, directories)
    }

    fn stamp(directories: &Directories) -> String {
        std::fs::read_to_string(stamp_path(directories)).unwrap()
    }

    /// Idempotent: checks before it acts.
    fn touch(directories: &Directories, name: &str) -> io::Result<()> {
        let path = directories.data.join(name);
        if path.exists() {
            return Ok(());
        }
        std::fs::write(path, "")
    }

    fn first(directories: &Directories) -> io::Result<Outcome> {
        // Ran after the stamp of nothing: the stamp must still read 0 here.
        assert_eq!(current(directories).unwrap(), 0);
        touch(directories, "first")?;
        Ok(Outcome::default())
    }

    fn second(directories: &Directories) -> io::Result<Outcome> {
        assert_eq!(current(directories).unwrap(), 1, "stamped after the first");
        assert!(directories.data.join("first").exists(), "ran in order");
        touch(directories, "second")?;
        Ok(Outcome {
            respawn_agents: true,
        })
    }

    fn broken(_: &Directories) -> io::Result<Outcome> {
        Err(io::Error::other("boom"))
    }

    const TWO: &[UpdateStep] = &[
        UpdateStep {
            version: 1,
            description: "first",
            run: first,
        },
        UpdateStep {
            version: 2,
            description: "second",
            run: second,
        },
    ];

    fn collect(directories: &Directories, steps: &[UpdateStep]) -> Vec<Applied> {
        let mut applied = Vec::new();
        apply_pending_with(directories, steps, |one| applied.push(one)).unwrap();
        applied
    }

    #[test]
    fn shipped_versions_count_up_from_one() {
        for (index, step) in UPDATE_STEPS.iter().enumerate() {
            assert_eq!(step.version as usize, index + 1, "{}", step.description);
        }
        assert_eq!(latest(), UPDATE_STEPS.len() as u32);
    }

    #[test]
    fn step_two_changes_no_file_and_asks_for_a_respawn() {
        let (_root, directories) = used_layout();
        let before = entries(&directories.data).unwrap();
        let applied = collect(&directories, UPDATE_STEPS);
        assert_eq!(applied.len(), 2);
        assert_eq!(applied[1].version, 2);
        assert_eq!(applied[1].description, "plugins run as their own programs");
        assert!(applied[1].outcome.respawn_agents);
        let mut after = entries(&directories.data).unwrap();
        after.retain(|path| !path.ends_with(STAMP_FILE));
        assert_eq!(after, before);
        assert_eq!(stamp(&directories), "2\n");
    }

    #[test]
    fn an_absent_stamp_reads_zero() {
        let (_root, directories) = layout();
        assert_eq!(current(&directories).unwrap(), 0);
    }

    #[test]
    fn a_malformed_stamp_is_an_error() {
        let (_root, directories) = used_layout();
        std::fs::write(stamp_path(&directories), "two\n").unwrap();
        assert!(matches!(
            current(&directories),
            Err(UpdateError::Malformed { .. })
        ));
    }

    #[test]
    fn runs_pending_in_order_and_stamps_after_each() {
        let (_root, directories) = used_layout();
        let applied = collect(&directories, TWO);
        assert_eq!(
            applied,
            vec![
                Applied {
                    version: 1,
                    description: "first",
                    outcome: Outcome::default()
                },
                Applied {
                    version: 2,
                    description: "second",
                    outcome: Outcome {
                        respawn_agents: true
                    }
                },
            ]
        );
        assert_eq!(stamp(&directories), "2\n");
        assert_eq!(behind_of(&directories, TWO).unwrap(), None);
    }

    #[test]
    fn runs_only_what_the_stamp_has_not_seen() {
        let (_root, directories) = used_layout();
        std::fs::write(directories.data.join("first"), "").unwrap();
        std::fs::write(stamp_path(&directories), "1\n").unwrap();
        assert_eq!(behind_of(&directories, TWO).unwrap(), Some((1, 2)));
        let applied = collect(&directories, TWO);
        assert_eq!(
            applied.iter().map(|one| one.version).collect::<Vec<_>>(),
            [2]
        );
    }

    #[test]
    fn a_failure_keeps_the_stamp_at_the_last_success() {
        let (_root, directories) = used_layout();
        let steps = &[
            UpdateStep {
                version: 1,
                description: "first",
                run: first,
            },
            UpdateStep {
                version: 2,
                description: "broken",
                run: broken,
            },
        ];
        let mut applied = Vec::new();
        let error = apply_pending_with(&directories, steps, |one| applied.push(one)).unwrap_err();
        assert!(
            matches!(error, UpdateError::Failed { version: 2, .. }),
            "{error}"
        );
        assert_eq!(error.to_string(), "update step 2 (broken) failed: boom");
        assert_eq!(applied.len(), 1, "the first was reported");
        assert_eq!(stamp(&directories), "1\n");
    }

    #[test]
    fn a_rerun_does_nothing() {
        let (_root, directories) = used_layout();
        collect(&directories, TWO);
        assert!(collect(&directories, TWO).is_empty());
        assert_eq!(stamp(&directories), "2\n");
    }

    #[test]
    fn a_rerun_after_a_lost_stamp_is_idempotent() {
        let (_root, directories) = used_layout();
        collect(&directories, TWO);
        std::fs::remove_file(stamp_path(&directories)).unwrap();
        // The steps check before they act, so running them again changes nothing; only
        // `first`'s own stamp assertion needs the stamp gone, which it is.
        assert_eq!(collect(&directories, TWO).len(), 2);
        assert_eq!(stamp(&directories), "2\n");
    }

    #[test]
    fn a_fresh_install_is_stamped_at_the_latest_without_running() {
        let (_root, directories) = layout();
        std::fs::create_dir_all(&directories.config).unwrap();
        std::fs::write(directories.config.join("config.yaml"), "").unwrap();
        assert!(is_fresh(&directories).unwrap());
        assert_eq!(behind_of(&directories, TWO).unwrap(), None);
        assert!(collect(&directories, TWO).is_empty());
        assert_eq!(stamp(&directories), "2\n");
        assert!(!directories.data.join("first").exists());
    }

    #[test]
    fn shipped_plugins_laid_out_first_leave_the_install_fresh() {
        let (_root, directories) = layout();
        let shipped = directories.data.join(SHIPPED_PLUGINS_DIR);
        std::fs::create_dir_all(shipped.join("mockup")).unwrap();
        std::fs::write(shipped.join(".selection"), "{}").unwrap();
        assert!(is_fresh(&directories).unwrap());
        assert!(collect(&directories, TWO).is_empty());
        assert_eq!(stamp(&directories), "2\n");
        assert!(!directories.data.join("first").exists());
    }

    #[test]
    fn a_lost_stamp_with_nothing_pending_is_written_back() {
        let (_root, directories) = used_layout();
        assert!(collect(&directories, &[]).is_empty());
        assert_eq!(stamp(&directories), "0\n");
    }

    fn old_layout() -> (tempfile::TempDir, Directories) {
        let (root, directories) = used_layout();
        let user = directories.config.join("definitions/agents");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("x.md"), "mine").unwrap();
        let shipped = directories.data.join("definitions/skills/s");
        std::fs::create_dir_all(&shipped).unwrap();
        std::fs::write(shipped.join("SKILL.md"), "shipped").unwrap();
        (root, directories)
    }

    #[test]
    fn step_one_moves_the_users_definitions_and_drops_the_shipped_ones() {
        let (_root, directories) = old_layout();
        let applied = collect(&directories, UPDATE_STEPS);
        assert_eq!(applied.len(), 2);
        assert_eq!(applied[0].version, 1);
        assert!(applied[0].outcome.respawn_agents);
        assert_eq!(
            std::fs::read_to_string(directories.config.join("harness/agents/x.md")).unwrap(),
            "mine"
        );
        assert!(!directories.config.join("definitions").exists());
        assert!(!directories.data.join("definitions").exists());
        assert_eq!(stamp(&directories), "2\n");
        assert!(
            collect(&directories, UPDATE_STEPS).is_empty(),
            "a rerun does nothing"
        );
    }

    #[test]
    fn step_one_again_after_a_lost_stamp_changes_nothing() {
        let (_root, directories) = old_layout();
        collect(&directories, UPDATE_STEPS);
        std::fs::remove_file(stamp_path(&directories)).unwrap();
        std::fs::create_dir_all(directories.data.join("harness")).unwrap();
        assert_eq!(collect(&directories, UPDATE_STEPS).len(), 2);
        assert!(directories.config.join("harness/agents/x.md").exists());
        assert!(directories.data.join("harness").exists());
    }

    #[test]
    fn step_one_refuses_when_both_trees_exist() {
        let (_root, directories) = old_layout();
        std::fs::create_dir_all(directories.config.join("harness")).unwrap();
        let error = apply_pending(&directories, |_| {}).unwrap_err();
        let text = error.to_string();
        assert!(
            text.contains(&directories.config.join("definitions").display().to_string()),
            "{text}"
        );
        assert!(
            text.contains(&directories.config.join("harness").display().to_string()),
            "{text}"
        );
        assert!(directories.config.join("definitions/agents/x.md").exists());
        assert!(
            directories.data.join("definitions").exists(),
            "nothing removed"
        );
        assert_eq!(current(&directories).unwrap(), 0);
    }

    #[test]
    fn step_one_removes_a_shipped_symlink_without_following_it() {
        let (root, directories) = used_layout();
        let target = root.path().join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("kept"), "").unwrap();
        std::os::unix::fs::symlink(&target, directories.data.join("definitions")).unwrap();
        collect(&directories, UPDATE_STEPS);
        assert!(
            directories
                .data
                .join("definitions")
                .symlink_metadata()
                .is_err()
        );
        assert!(target.join("kept").exists());
    }

    #[test]
    fn a_data_directory_holding_only_the_stamp_is_fresh() {
        let (_root, directories) = layout();
        write_stamp(&directories, 1).unwrap();
        assert!(is_fresh(&directories).unwrap());
    }

    #[test]
    fn a_config_subdirectory_is_not_fresh() {
        let (_root, directories) = layout();
        std::fs::create_dir_all(directories.config.join("roles")).unwrap();
        assert!(!is_fresh(&directories).unwrap());
        assert_eq!(behind_of(&directories, TWO).unwrap(), Some((0, 2)));
    }
}
