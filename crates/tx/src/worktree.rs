//! Git worktree creation for isolated agent worker spawns (worktree.py).

use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

/// A requested worktree could not be created (or git could not answer).
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error("invalid worktree name '{0}'")]
    InvalidName(String),
    #[error("worktree path already exists: {}", .0.display())]
    PathExists(PathBuf),
    #[error("could not find a free worktree name based on '{0}'")]
    NoFreeName(String),
    #[error("git did not report a main checkout")]
    NoMainCheckout,
    /// git's stripped stderr (else stdout, else a generic line).
    #[error("{0}")]
    Git(String),
    #[error("could not run git: {0}")]
    Spawn(#[source] io::Error),
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Create tx-owned detached worktrees from a repository checkout, under `root`
/// (`Home::worktrees_dir()` in production).
#[derive(Clone, Debug)]
pub struct WorktreeManager {
    root: PathBuf,
}

struct RepositoryContext {
    main_checkout: PathBuf,
    common_directory: PathBuf,
    registered_paths: HashSet<PathBuf>,
}

impl WorktreeManager {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn create(
        &self,
        starting_directory: &Path,
        worktree_name: &str,
    ) -> Result<PathBuf, WorktreeError> {
        if !is_valid_name(worktree_name) {
            return Err(WorktreeError::InvalidName(worktree_name.to_owned()));
        }
        let context = repository_context(starting_directory)?;
        let worktree_directory = self.path_for(&context, worktree_name);
        if worktree_directory.exists()
            || context
                .registered_paths
                .contains(&resolve_path(&worktree_directory))
        {
            return Err(WorktreeError::PathExists(worktree_directory));
        }
        if let Some(parent) = worktree_directory.parent() {
            std::fs::create_dir_all(parent).map_err(|source| WorktreeError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        git(
            starting_directory,
            [
                OsStr::new("worktree"),
                OsStr::new("add"),
                OsStr::new("--detach"),
                worktree_directory.as_os_str(),
                OsStr::new("HEAD"),
            ],
        )?;
        Ok(worktree_directory)
    }

    /// First `<base>`, `<base>-2`, … `<base>-99` free in both the session namespace
    /// (`unavailable_names`) and Git paths.
    pub fn next_name(
        &self,
        starting_directory: &Path,
        base_name: &str,
        unavailable_names: &HashSet<String>,
    ) -> Result<String, WorktreeError> {
        let context = repository_context(starting_directory)?;
        for suffix in 1..100 {
            let candidate = if suffix == 1 {
                base_name.to_owned()
            } else {
                format!("{base_name}-{suffix}")
            };
            if !is_valid_name(&candidate) || unavailable_names.contains(&candidate) {
                continue;
            }
            let path = self.path_for(&context, &candidate);
            if !path.exists() && !context.registered_paths.contains(&resolve_path(&path)) {
                return Ok(candidate);
            }
        }
        Err(WorktreeError::NoFreeName(base_name.to_owned()))
    }

    /// Create a detached worktree under a name free across live sessions and Git paths.
    pub fn create_unique(
        &self,
        starting_directory: &Path,
        base_name: &str,
        unavailable_names: &HashSet<String>,
    ) -> Result<(String, PathBuf), WorktreeError> {
        let name = self.next_name(starting_directory, base_name, unavailable_names)?;
        let path = self.create(starting_directory, &name)?;
        Ok((name, path))
    }

    /// Stable readable key for this repository under the global worktree root.
    pub fn repository_key(&self, starting_directory: &Path) -> Result<String, WorktreeError> {
        let context = repository_context(starting_directory)?;
        Ok(repository_key(
            &context.main_checkout,
            &context.common_directory,
        ))
    }

    /// Every registered checkout (sorted by path string) a read-only worker must not modify.
    pub fn repository_worktrees(&self, directory: &Path) -> Result<Vec<PathBuf>, WorktreeError> {
        let mut paths: Vec<PathBuf> = repository_context(directory)?
            .registered_paths
            .into_iter()
            .collect();
        paths.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
        Ok(paths)
    }

    /// Whether `directory` belongs to a linked worktree rather than the main checkout; `false`
    /// when git cannot answer.
    pub fn is_linked(&self, directory: &Path) -> bool {
        let git_path = |flag: &str| -> Result<PathBuf, WorktreeError> {
            let out = git(directory, [OsStr::new("rev-parse"), OsStr::new(flag)])?;
            Ok(resolve_git_path(directory, out.trim()))
        };
        match (git_path("--git-dir"), git_path("--git-common-dir")) {
            (Ok(git_directory), Ok(common_directory)) => git_directory != common_directory,
            _ => false,
        }
    }

    /// Canonical shared Git metadata directory for a checkout or linked worktree.
    pub fn git_common_directory(&self, directory: &Path) -> Result<PathBuf, WorktreeError> {
        git_common_directory(directory)
    }

    /// Remove a tx-created worktree after a pre-launch failure or temporary helper exit.
    pub fn remove(&self, directory: &Path) -> Result<(), WorktreeError> {
        git(
            directory,
            [
                OsStr::new("worktree"),
                OsStr::new("remove"),
                OsStr::new("--force"),
                directory.as_os_str(),
            ],
        )
        .map(drop)
    }

    fn path_for(&self, context: &RepositoryContext, worktree_name: &str) -> PathBuf {
        let label = format!(
            "{}--{worktree_name}",
            repository_slug(&context.main_checkout)
        );
        self.root
            .join(repository_key(
                &context.main_checkout,
                &context.common_directory,
            ))
            .join(label)
    }
}

/// `<slug>-<sha256(common dir)[:8]>`: the readable prefix keeps the root browsable, the digest of
/// the canonical shared Git directory keeps same-named repositories apart.
pub fn repository_key(main_checkout: &Path, common_directory: &Path) -> String {
    let digest = sha256_hex(common_directory.as_os_str().as_encoded_bytes());
    format!("{}-{}", repository_slug(main_checkout), &digest[..8])
}

/// Filesystem-safe repository label: `re.sub(r"[^A-Za-z0-9._-]+", "-", name).strip("-._")`, or
/// `repository` when nothing is left.
pub fn repository_slug(main_checkout: &Path) -> String {
    let name = main_checkout
        .file_name()
        .map(OsStr::to_string_lossy)
        .unwrap_or_default();
    let mut slug = String::with_capacity(name.len());
    let mut in_run = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            slug.push(c);
            in_run = false;
        } else if !in_run {
            slug.push('-');
            in_run = true;
        }
    }
    let slug = slug.trim_matches(['-', '.', '_']);
    if slug.is_empty() {
        "repository".to_owned()
    } else {
        slug.to_owned()
    }
}

fn is_valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/')
}

fn repository_context(starting_directory: &Path) -> Result<RepositoryContext, WorktreeError> {
    let listing = git(
        starting_directory,
        [
            OsStr::new("worktree"),
            OsStr::new("list"),
            OsStr::new("--porcelain"),
        ],
    )?;
    let worktree_lines: Vec<&str> = listing
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .collect();
    let first = worktree_lines
        .first()
        .ok_or(WorktreeError::NoMainCheckout)?;
    Ok(RepositoryContext {
        main_checkout: resolve_path(Path::new(first)),
        common_directory: git_common_directory(starting_directory)?,
        registered_paths: worktree_lines
            .iter()
            .map(|path| resolve_path(Path::new(path)))
            .collect(),
    })
}

fn git_common_directory(directory: &Path) -> Result<PathBuf, WorktreeError> {
    let out = git(
        directory,
        [OsStr::new("rev-parse"), OsStr::new("--git-common-dir")],
    )?;
    Ok(resolve_git_path(directory, out.trim()))
}

fn resolve_git_path(starting_directory: &Path, git_path: &str) -> PathBuf {
    resolve_path(&starting_directory.join(git_path))
}

/// `git -C <dir> <args…>`; stdout on success, git's stripped stderr (else stdout) as the error.
fn git<'a>(
    starting_directory: &Path,
    arguments: impl IntoIterator<Item = &'a OsStr>,
) -> Result<String, WorktreeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(starting_directory)
        .args(arguments)
        .stdin(Stdio::inherit())
        .output()
        .map_err(WorktreeError::Spawn)?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or("git worktree command failed");
    Err(WorktreeError::Git(detail.to_owned()))
}

const MAX_SYMLINK_HOPS: usize = 40;

/// `Path.resolve()` (non-strict `os.path.realpath`): absolute against the cwd, every existing
/// symlink followed, `..` applied after resolution, missing tails kept as written.
pub(crate) fn resolve_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_owned())
    };
    let mut pending: Vec<OsString> = components_of(&absolute);
    pending.reverse();
    let mut resolved = PathBuf::from("/");
    let mut hops = 0;
    while let Some(part) = pending.pop() {
        if part == ".." {
            resolved.pop();
            continue;
        }
        let next = resolved.join(&part);
        let is_link = std::fs::symlink_metadata(&next).is_ok_and(|m| m.file_type().is_symlink());
        match std::fs::read_link(&next) {
            Ok(target) if is_link && hops < MAX_SYMLINK_HOPS => {
                hops += 1;
                if target.is_absolute() {
                    resolved = PathBuf::from("/");
                }
                pending.extend(components_of(&target).into_iter().rev());
            }
            _ => resolved = next,
        }
    }
    resolved
}

fn components_of(path: &Path) -> Vec<OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_owned()),
            Component::ParentDir => Some(OsString::from("..")),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        })
        .collect()
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;

    /// A temp git repo with one commit, isolated from the host's git config.
    pub(crate) fn git_repo(parent: &Path, name: &str) -> PathBuf {
        let repo = parent.join(name);
        fs::create_dir_all(&repo).unwrap();
        fs::write(repo.join("f"), "x").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "f"],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .unwrap();
            assert!(status.success());
        }
        repo
    }

    #[test]
    fn slug_matches_python() {
        for (name, slug) in [
            ("My Repo", "My-Repo"),
            ("proj.git", "proj.git"),
            ("..x!!y__", "x-y"),
            ("日本", "repository"),
            ("a  b@@c", "a-b-c"),
        ] {
            assert_eq!(
                repository_slug(&Path::new("/p").join(name)),
                slug,
                "{name:?}"
            );
        }
        assert_eq!(repository_slug(Path::new("/")), "repository");
    }

    #[test]
    fn resolve_path_follows_symlinks_and_keeps_missing_tail() {
        let dir = tempfile::tempdir().unwrap();
        let real = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(real.join("target")).unwrap();
        std::os::unix::fs::symlink(real.join("target"), real.join("link")).unwrap();
        std::os::unix::fs::symlink("target/../target", real.join("rel")).unwrap();
        assert_eq!(
            resolve_path(&dir.path().join("link/missing/../x")),
            real.join("target/x")
        );
        assert_eq!(
            resolve_path(&dir.path().join("rel/./a")),
            real.join("target/a")
        );
        assert_eq!(resolve_path(Path::new("/")), PathBuf::from("/"));
    }

    #[test]
    fn create_next_name_and_linked() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let repo = git_repo(&base, "My Repo");
        let manager = WorktreeManager::new(base.join("wt"));
        let key = manager.repository_key(&repo).unwrap();
        let digest = &sha256_hex(repo.join(".git").as_os_str().as_encoded_bytes())[..8];
        assert_eq!(key, format!("My-Repo-{digest}"));

        let none = HashSet::new();
        let (name, path) = manager.create_unique(&repo, "w", &none).unwrap();
        assert_eq!(name, "w");
        assert_eq!(path, base.join("wt").join(&key).join("My-Repo--w"));
        assert!(path.join("f").is_file());
        assert!(manager.is_linked(&path));
        assert!(!manager.is_linked(&repo));
        assert!(!manager.is_linked(&base));
        assert_eq!(
            manager.git_common_directory(&path).unwrap(),
            repo.join(".git")
        );
        assert_eq!(manager.repository_key(&path).unwrap(), key);

        let taken: HashSet<String> = ["w-2".to_owned()].into();
        fs::create_dir_all(base.join("wt").join(&key).join("My-Repo--w-3")).unwrap();
        assert_eq!(manager.next_name(&repo, "w", &taken).unwrap(), "w-4");

        let mut listed = vec![repo.clone(), path.clone()];
        listed.sort();
        assert_eq!(manager.repository_worktrees(&repo).unwrap(), listed);

        assert_eq!(
            manager.create(&repo, "w").unwrap_err().to_string(),
            format!("worktree path already exists: {}", path.display())
        );
        manager.remove(&path).unwrap();
        assert!(!path.exists());
        assert_eq!(manager.repository_worktrees(&repo).unwrap(), vec![repo]);
    }

    #[test]
    fn name_errors() {
        let dir = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(dir.path()).unwrap();
        let repo = git_repo(&base, "r");
        let manager = WorktreeManager::new(base.join("wt"));
        for bad in ["", ".", "..", "a/b"] {
            assert_eq!(
                manager.create(&repo, bad).unwrap_err().to_string(),
                format!("invalid worktree name '{bad}'")
            );
        }
        let none = HashSet::new();
        assert_eq!(
            manager
                .next_name(&repo, "a/b", &none)
                .unwrap_err()
                .to_string(),
            "could not find a free worktree name based on 'a/b'"
        );
        // `.` is skipped, `.-2` is a legal name.
        assert_eq!(manager.next_name(&repo, ".", &none).unwrap(), ".-2");
        let err = manager.create(&base, "w").unwrap_err();
        assert!(
            matches!(err, WorktreeError::Git(ref m) if m.starts_with("fatal: not a git repository")),
            "{err}"
        );
    }
}
