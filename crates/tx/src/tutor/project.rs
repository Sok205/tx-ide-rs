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
pub fn scaffold(skeleton: &Path, dir: &Path, lang: &str) -> Result<(), ProjectError> {
    let io_error = |path: &Path| {
        let path = path.to_path_buf();
        move |source| ProjectError::Io { path, source }
    };
    let occupied = std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some());
    if occupied && !is_tutor_project(dir) {
        return Err(ProjectError::NotEmpty(dir.to_path_buf()));
    }
    copy_dir(skeleton, dir).map_err(io_error(dir))?;
    std::fs::write(dir.join(MARKER), format!("{lang}\n")).map_err(io_error(dir))?;
    git(dir, &["init", "-q", "-b", "main"])?;
    git(dir, &["add", "-A"])?;
    git(
        dir,
        &[
            "-c", "user.name=tx tutor",
            "-c", "user.email=tutor@tx-ide.invalid",
            "commit", "-q", "-m", "tutor: skeleton",
        ],
    )
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
}
