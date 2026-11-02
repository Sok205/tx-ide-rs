//! Which repository an agent's worktree belongs to, and the branch it is on. The answers come
//! from `git`, run once per worktree; the branch is re-read only when the worktree's `HEAD`
//! file changed, which is a stat per poll. Chat counts are kept here too: they are known only
//! once an agent's transcript was read (see [`crate::detail::detail`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

/// How long a failed git lookup stands before the worktree is looked up again: a repository that
/// appears later, or a git that failed to spawn once, is noticed within this window.
const FAILED_LOOKUP_RETRY: Duration = Duration::from_secs(1);

/// Modification time and length: a rewritten `HEAD` changes at least one of them.
type HeadStamp = (SystemTime, u64);

struct Entry {
    main_checkout: PathBuf,
    /// The worktree's own git directory, where its `HEAD` lives; `None` when git failed.
    git_directory: Option<PathBuf>,
    /// When the lookup failed, if it did.
    failed_at: Option<Instant>,
    head_stamp: Option<HeadStamp>,
    branch: Option<String>,
}

#[derive(Default)]
pub struct ProjectCache {
    entries: BTreeMap<PathBuf, Entry>,
    chats: BTreeMap<String, usize>,
}

impl ProjectCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// The main checkout of the repository `worktree` belongs to: the parent of its common git
    /// directory. The worktree's parent directory when git cannot say (a deleted repository).
    pub fn main_checkout(&mut self, worktree: &Path) -> PathBuf {
        self.entry(worktree).main_checkout.clone()
    }

    /// The branch checked out in `worktree`; `None` when detached or when git failed.
    pub fn branch(&mut self, worktree: &Path) -> Option<String> {
        let entry = self.entry(worktree);
        let git_directory = entry.git_directory.as_ref()?;
        let stamp = head_stamp(git_directory);
        if entry.head_stamp != stamp || stamp.is_none() {
            entry.branch = git(worktree, &["symbolic-ref", "--short", "HEAD"]);
            entry.head_stamp = stamp;
        }
        entry.branch.clone()
    }

    pub fn record_chats(&mut self, agent_id: &str, chats: usize) {
        self.chats.insert(agent_id.to_owned(), chats);
    }

    /// The message count of an agent whose detail was computed, else 0.
    pub fn chats(&self, agent_id: &str) -> usize {
        self.chats.get(agent_id).copied().unwrap_or(0)
    }

    fn entry(&mut self, worktree: &Path) -> &mut Entry {
        let entry = self
            .entries
            .entry(worktree.to_owned())
            .or_insert_with(|| look_up(worktree));
        if entry
            .failed_at
            .is_some_and(|at| at.elapsed() >= FAILED_LOOKUP_RETRY)
        {
            *entry = look_up(worktree);
        }
        entry
    }
}

fn look_up(worktree: &Path) -> Entry {
    let fallback = || Entry {
        main_checkout: worktree.parent().unwrap_or(worktree).to_owned(),
        git_directory: None,
        failed_at: Some(Instant::now()),
        head_stamp: None,
        branch: None,
    };
    if worktree.as_os_str().is_empty() {
        return fallback();
    }
    let Some(output) = git(worktree, &["rev-parse", "--git-dir", "--git-common-dir"]) else {
        return fallback();
    };
    let mut lines = output.lines();
    let resolved = |line: Option<&str>| worktree.join(line?).canonicalize().ok();
    let (Some(git_directory), Some(common_directory)) =
        (resolved(lines.next()), resolved(lines.next()))
    else {
        return fallback();
    };
    let Some(main_checkout) = common_directory.parent() else {
        return fallback();
    };
    Entry {
        main_checkout: main_checkout.to_owned(),
        git_directory: Some(git_directory),
        failed_at: None,
        head_stamp: None,
        branch: None,
    }
}

fn head_stamp(git_directory: &Path) -> Option<HeadStamp> {
    let metadata = std::fs::metadata(git_directory.join("HEAD")).ok()?;
    Some((metadata.modified().ok()?, metadata.len()))
}

/// Trimmed stdout of `git -C <directory> <arguments>`; `None` on any failure or empty output.
fn git(directory: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(arguments)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(status.status.success(), "git {arguments:?}");
    }

    fn repository(root: &Path) -> PathBuf {
        let main = root.join("project");
        std::fs::create_dir(&main).unwrap();
        run_git(&main, &["init", "-b", "main"]);
        run_git(&main, &["commit", "--allow-empty", "-m", "first"]);
        main.canonicalize().unwrap()
    }

    #[test]
    fn a_worktree_resolves_to_its_main_checkout() {
        let root = tempfile::tempdir().unwrap();
        let main = repository(root.path());
        let worktree = root.path().join("project--feature");
        run_git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );
        let mut cache = ProjectCache::new();
        assert_eq!(cache.main_checkout(&worktree), main);
        assert_eq!(cache.main_checkout(&main), main);
        assert_eq!(cache.branch(&worktree).as_deref(), Some("feature"));
        assert_eq!(cache.branch(&main).as_deref(), Some("main"));
    }

    #[test]
    fn a_deleted_repository_falls_back_to_the_parent_directory() {
        let root = tempfile::tempdir().unwrap();
        let main = repository(root.path());
        let worktree = root.path().join("project--feature");
        run_git(
            &main,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        );
        std::fs::remove_dir_all(&main).unwrap();
        let mut cache = ProjectCache::new();
        assert_eq!(cache.main_checkout(&worktree), worktree.parent().unwrap());
        assert_eq!(cache.branch(&worktree), None);
        let gone = root.path().join("gone/worktree");
        assert_eq!(cache.main_checkout(&gone), root.path().join("gone"));
    }

    #[test]
    fn branch_follows_head_changes() {
        let root = tempfile::tempdir().unwrap();
        let main = repository(root.path());
        let mut cache = ProjectCache::new();
        assert_eq!(cache.branch(&main).as_deref(), Some("main"));
        run_git(&main, &["checkout", "-b", "a-longer-branch-name"]);
        assert_eq!(cache.branch(&main).as_deref(), Some("a-longer-branch-name"));
        run_git(&main, &["checkout", "--detach"]);
        assert_eq!(cache.branch(&main), None);
        run_git(&main, &["checkout", "main"]);
        assert_eq!(cache.branch(&main).as_deref(), Some("main"));
    }

    #[test]
    fn an_empty_path_is_not_a_repository() {
        let mut cache = ProjectCache::new();
        assert_eq!(cache.main_checkout(Path::new("")), Path::new(""));
        assert_eq!(cache.branch(Path::new("")), None);
    }

    #[test]
    fn a_failed_lookup_is_retried_after_the_retry_window() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("later");
        std::fs::create_dir(&directory).unwrap();
        let mut cache = ProjectCache::new();
        assert_eq!(cache.main_checkout(&directory), root.path());
        assert_eq!(cache.branch(&directory), None);
        run_git(&directory, &["init", "-b", "main"]);
        run_git(&directory, &["commit", "--allow-empty", "-m", "first"]);
        assert_eq!(cache.main_checkout(&directory), root.path());
        std::thread::sleep(FAILED_LOOKUP_RETRY + std::time::Duration::from_millis(100));
        assert_eq!(
            cache.main_checkout(&directory),
            directory.canonicalize().unwrap()
        );
        assert_eq!(cache.branch(&directory).as_deref(), Some("main"));
    }

    #[test]
    fn chats_are_zero_until_recorded() {
        let mut cache = ProjectCache::new();
        assert_eq!(cache.chats("a"), 0);
        cache.record_chats("a", 7);
        assert_eq!(cache.chats("a"), 7);
    }
}
