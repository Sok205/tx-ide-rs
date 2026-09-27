//! The user's tutorial project: the pack skeleton copied into a fresh git repo.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Marks a directory as a tutor project; holds the pack language.
pub const MARKER: &str = ".tx-tutor";

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("{} is not empty and not a tx tutor project; pick another --dir", .0.display())]
    NotEmpty(PathBuf),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("git {args} failed: {stderr}")]
    Git { args: String, stderr: String },
}

pub fn is_tutor_project(dir: &Path) -> bool {
    dir.join(MARKER).is_file()
}

/// Copy `skeleton` into `dir` (created if needed), write the marker, `git init -b main` and
/// commit. Refuses a non-empty directory that is not already a tutor project.
///
/// All work is done in a staging directory; if any step fails, `dir` is left untouched.
pub fn scaffold(skeleton: &Path, dir: &Path, lang: &str) -> Result<(), ProjectError> {
    let io_error = |path: &Path| {
        let path = path.to_path_buf();
        move |source| ProjectError::Io { path, source }
    };
    // Up-front check: if dir exists, is non-empty, and is not a tutor project, refuse.
    let occupied = std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some());
    if occupied && !is_tutor_project(dir) {
        return Err(ProjectError::NotEmpty(dir.to_path_buf()));
    }
    // Create parent directory.
    let parent = dir.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(io_error(parent))?;
    // Create staging directory.
    let staging = tempfile::Builder::new()
        .prefix(".tx-tutor-staging-")
        .tempdir_in(parent)
        .map_err(|source| ProjectError::Io { path: parent.to_path_buf(), source })?;
    let staging_path = staging.path();
    // Do all work in staging directory.
    copy_dir(skeleton, staging_path).map_err(io_error(staging_path))?;
    std::fs::write(staging_path.join(MARKER), format!("{lang}\n")).map_err(io_error(staging_path))?;
    git(staging_path, &["init", "-q", "-b", "main"])?;
    git(staging_path, &["add", "-A"])?;
    git(
        staging_path,
        &[
            "-c", "user.name=tx tutor",
            "-c", "user.email=tutor@tx-ide.invalid",
            "commit", "-q", "-m", "tutor: skeleton",
        ],
    )?;
    // On success: remove target if it exists (empty only), then move staging to target.
    if dir.exists() {
        std::fs::remove_dir(dir).map_err(io_error(dir))?;
    }
    let staging_path = staging.keep();
    std::fs::rename(staging_path, dir).map_err(io_error(dir))?;
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn git(dir: &Path, args: &[&str]) -> Result<(), ProjectError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|source| ProjectError::Io { path: dir.to_path_buf(), source })?;
    if output.status.success() {
        return Ok(());
    }
    Err(ProjectError::Git {
        args: args.join(" "),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skeleton() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("server.py"), "print('hi')\n").unwrap();
        std::fs::create_dir(dir.path().join("static")).unwrap();
        std::fs::write(dir.path().join("static/a.txt"), "a").unwrap();
        dir
    }

    #[test]
    fn scaffolds_a_committed_git_project() {
        let skeleton = skeleton();
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("python");
        scaffold(skeleton.path(), &dir, "python").unwrap();
        assert!(is_tutor_project(&dir));
        assert_eq!(std::fs::read_to_string(dir.join("static/a.txt")).unwrap(), "a");
        let log = Command::new("git").args(["log", "--oneline"]).current_dir(&dir).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 1);
        let status = Command::new("git").args(["status", "--porcelain"]).current_dir(&dir).output().unwrap();
        assert!(status.stdout.is_empty(), "everything is committed");
    }

    #[test]
    fn refuses_a_non_empty_foreign_directory() {
        let skeleton = skeleton();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mine.txt"), "x").unwrap();
        assert!(matches!(scaffold(skeleton.path(), dir.path(), "python"), Err(ProjectError::NotEmpty(_))));
        assert!(!is_tutor_project(dir.path()));
    }

    #[test]
    fn failure_leaves_dir_untouched_and_retry_succeeds() {
        let skeleton = skeleton();
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("python");
        // First attempt: scaffold from a non-existent skeleton path.
        let nonexistent = root.path().join("nonexistent");
        let err = scaffold(nonexistent.as_path(), &dir, "python").unwrap_err();
        assert!(matches!(err, ProjectError::Io { .. }));
        // Target dir should not exist after failure.
        assert!(!dir.exists(), "target dir should not exist after failure");
        // Second attempt: scaffold from the valid skeleton should now succeed.
        scaffold(skeleton.path(), &dir, "python").unwrap();
        assert!(is_tutor_project(&dir));
        assert_eq!(std::fs::read_to_string(dir.join("static/a.txt")).unwrap(), "a");
    }
}
