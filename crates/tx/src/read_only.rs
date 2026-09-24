//! Fail-closed OS boundary for an entire read-only agent process tree (read_only.py): the agent
//! command is wrapped in `sandbox-exec` (macOS) or `bwrap` (Linux) with every repository-owned
//! path write-denied. Host facts (`platform.system()`, `$PATH`) are explicit inputs.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::pyjson;
use crate::shlex::{ShlexError, shlex_join, shlex_split};
use crate::worktree::resolve_path;

/// Wrapper binaries the reconciler must see through when matching a pane's command.
pub const READ_ONLY_WRAPPER_BINARIES: [&str; 2] = ["sandbox-exec", "bwrap"];

/// `os.confstr("CS_PATH")` on macOS and glibc: `shutil.which`'s fallback when `$PATH` is unset.
const CS_PATH: &str = "/usr/bin:/bin";

/// The host cannot enforce tx's whole-process repository write boundary.
#[derive(Debug, thiserror::Error)]
pub enum ReadOnlySandboxError {
    #[error("cannot sandbox an empty agent command")]
    EmptyCommand,
    /// `shlex.split` refused the command (a `ValueError` in the Python).
    #[error("{0}")]
    Split(#[from] ShlexError),
    #[error("refusing to apply a read-only boundary to filesystem root")]
    FilesystemRoot,
    #[error("macOS sandbox-exec is unavailable")]
    SandboxExecUnavailable,
    #[error("Linux read-only sessions require bubblewrap (bwrap)")]
    BwrapUnavailable,
    #[error("read-only agent sessions are unsupported on {0}")]
    Unsupported(String),
}

/// `platform.system()`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum System {
    Darwin,
    Linux,
    /// Any other `platform.system()` value (empty when unknown).
    Other(String),
}

impl System {
    /// The system this binary was built for.
    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Self::Darwin
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other(capitalize(std::env::consts::OS))
        }
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Host facts the wrapper depends on.
#[derive(Clone, Copy, Debug)]
pub struct SandboxHost<'a> {
    pub system: &'a System,
    /// `$PATH` (`None` when unset) for locating the wrapper binary.
    pub path: Option<&'a OsStr>,
}

/// Run an agent and every descendant with all repository-owned paths read-only. Returns the
/// shell-quoted launch command; engine permission controls stay enabled inside the boundary.
pub fn wrap_read_only_command(
    command: &str,
    workspace: &Path,
    repository_worktrees: &[PathBuf],
    git_common_directory: &Path,
    host: SandboxHost<'_>,
) -> Result<String, ReadOnlySandboxError> {
    let arguments = shlex_split(command)?;
    if arguments.is_empty() {
        return Err(ReadOnlySandboxError::EmptyCommand);
    }
    let workspace_parent = workspace.parent().unwrap_or(workspace);
    let boundaries = minimal_boundaries(
        std::iter::once(workspace_parent)
            .chain(repository_worktrees.iter().map(PathBuf::as_path))
            .chain(std::iter::once(git_common_directory)),
    )?;
    match host.system {
        System::Darwin => {
            let executable = which("sandbox-exec", host.path)
                .ok_or(ReadOnlySandboxError::SandboxExecUnavailable)?;
            let denied = boundaries
                .iter()
                .map(|boundary| {
                    let quoted =
                        pyjson::dumps(&Value::String(boundary.to_string_lossy().into_owned()));
                    format!("(subpath {quoted})")
                })
                .collect::<Vec<_>>()
                .join(" ");
            let profile = format!("(version 1)\n(allow default)\n(deny file-write* {denied})");
            let mut wrapped = vec![executable, "-p".to_owned(), profile];
            wrapped.extend(arguments);
            Ok(shlex_join(&wrapped))
        }
        System::Linux => {
            let executable =
                which("bwrap", host.path).ok_or(ReadOnlySandboxError::BwrapUnavailable)?;
            let mut wrapped: Vec<String> = [executable.as_str(), "--bind", "/", "/"]
                .map(str::to_owned)
                .into();
            for boundary in &boundaries {
                let boundary = boundary.to_string_lossy().into_owned();
                wrapped.extend(["--ro-bind".to_owned(), boundary.clone(), boundary]);
            }
            wrapped.extend([
                "--chdir".to_owned(),
                resolve_path(workspace).to_string_lossy().into_owned(),
                "--".to_owned(),
            ]);
            wrapped.extend(arguments);
            Ok(shlex_join(&wrapped))
        }
        System::Other(name) => Err(ReadOnlySandboxError::Unsupported(if name.is_empty() {
            "this platform".to_owned()
        } else {
            name.clone()
        })),
    }
}

/// Canonical non-overlapping paths, shallowest first; denying a parent already denies every child.
fn minimal_boundaries<'a>(
    paths: impl Iterator<Item = &'a Path>,
) -> Result<Vec<PathBuf>, ReadOnlySandboxError> {
    let mut resolved: Vec<PathBuf> = Vec::new();
    for path in paths.map(resolve_path) {
        if !resolved.contains(&path) {
            resolved.push(path);
        }
    }
    resolved.sort_by_key(|path| path.components().count());
    if resolved.iter().any(|path| path == Path::new("/")) {
        return Err(ReadOnlySandboxError::FilesystemRoot);
    }
    Ok(resolved
        .iter()
        .filter(|path| {
            !resolved
                .iter()
                .any(|parent| path != &parent && path.starts_with(parent))
        })
        .cloned()
        .collect())
}

/// `shutil.which(name)` on POSIX: the first `dir/name` on the search path that exists, is not a
/// directory and is executable. An empty `$PATH` finds nothing; an empty entry means the cwd.
fn which(name: &str, path: Option<&OsStr>) -> Option<String> {
    let search = path
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| CS_PATH.to_owned());
    if search.is_empty() {
        return None;
    }
    let mut seen = Vec::new();
    for dir in search.split(':') {
        if seen.contains(&dir) {
            continue;
        }
        seen.push(dir);
        let candidate = if dir.is_empty() {
            name.to_owned()
        } else if dir.ends_with('/') {
            format!("{dir}{name}")
        } else {
            format!("{dir}/{name}")
        };
        let executable =
            std::fs::metadata(&candidate).is_ok_and(|m| !m.is_dir()) && access_x(&candidate);
        if executable {
            return Some(candidate);
        }
    }
    None
}

/// `os.access(path, X_OK)` for the current user.
fn access_x(path: &str) -> bool {
    let Ok(c_path) = std::ffi::CString::new(path) else {
        return false;
    };
    // SAFETY: `c_path` is a valid NUL-terminated string that outlives the call.
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture {
        _dir: tempfile::TempDir,
        base: PathBuf,
        bin: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let bin = base.join("bin");
        fs::create_dir(&bin).unwrap();
        for name in READ_ONLY_WRAPPER_BINARIES {
            let path = bin.join(name);
            fs::write(&path, "#!/bin/sh\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        for sub in ["repo/.git", "wt/key/repo--w", "wt/key/repo--o", "repo/sub"] {
            fs::create_dir_all(base.join(sub)).unwrap();
        }
        Fixture {
            _dir: dir,
            base,
            bin,
        }
    }

    const COMMAND: &str = "claude --effort high --permission-mode dontAsk";

    fn wrap(
        fx: &Fixture,
        system: &System,
        path: Option<&OsStr>,
    ) -> Result<String, ReadOnlySandboxError> {
        let worktrees = [
            fx.base.join("repo"),
            fx.base.join("repo/sub"),
            fx.base.join("wt/key/repo--o"),
            fx.base.join("wt/key/repo--w"),
        ];
        wrap_read_only_command(
            COMMAND,
            &fx.base.join("wt/key/repo--w"),
            &worktrees,
            &fx.base.join("repo/.git"),
            SandboxHost { system, path },
        )
    }

    #[test]
    fn linux_bwrap_argv() {
        let fx = fixture();
        let path = format!("/nonexistent:{}", fx.bin.display());
        let out = wrap(&fx, &System::Linux, Some(OsStr::new(&path))).unwrap();
        let b = fx.base.display();
        assert_eq!(
            out,
            format!(
                "{b}/bin/bwrap --bind / / --ro-bind {b}/repo {b}/repo --ro-bind {b}/wt/key {b}/wt/key \
                 --chdir {b}/wt/key/repo--w -- {COMMAND}"
            )
        );
    }

    #[test]
    fn darwin_sandbox_exec_profile() {
        let fx = fixture();
        let path = fx.bin.as_os_str();
        let out = wrap(&fx, &System::Darwin, Some(path)).unwrap();
        let b = fx.base.display();
        let profile = format!(
            "(version 1)\n(allow default)\n(deny file-write* (subpath \"{b}/repo\") (subpath \"{b}/wt/key\"))"
        );
        let expected = shlex_join(&[format!("{b}/bin/sandbox-exec"), "-p".to_owned(), profile])
            + " "
            + COMMAND;
        assert_eq!(out, expected);
    }

    #[test]
    fn refusals() {
        let fx = fixture();
        let empty = OsStr::new("");
        assert_eq!(
            wrap(&fx, &System::Darwin, Some(empty))
                .unwrap_err()
                .to_string(),
            "macOS sandbox-exec is unavailable"
        );
        assert_eq!(
            wrap(&fx, &System::Linux, Some(fx.base.as_os_str()))
                .unwrap_err()
                .to_string(),
            "Linux read-only sessions require bubblewrap (bwrap)"
        );
        assert_eq!(
            wrap(&fx, &System::Other("FreeBSD".into()), None)
                .unwrap_err()
                .to_string(),
            "read-only agent sessions are unsupported on FreeBSD"
        );
        assert_eq!(
            wrap(&fx, &System::Other(String::new()), None)
                .unwrap_err()
                .to_string(),
            "read-only agent sessions are unsupported on this platform"
        );
        let host = SandboxHost {
            system: &System::Linux,
            path: None,
        };
        assert_eq!(
            wrap_read_only_command("  ", Path::new("/a/b"), &[], Path::new("/a"), host)
                .unwrap_err()
                .to_string(),
            "cannot sandbox an empty agent command"
        );
        assert_eq!(
            wrap_read_only_command("x", Path::new("/a"), &[], Path::new("/a/.git"), host)
                .unwrap_err()
                .to_string(),
            "refusing to apply a read-only boundary to filesystem root"
        );
    }
}
