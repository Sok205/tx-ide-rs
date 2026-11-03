//! Which repository an agent's worktree belongs to, and the branch it is on. Reading never
//! runs git: an unknown worktree is grouped under its parent directory, provisionally, until
//! [`ProjectCache::look_up_due`] (the TUI's worker, after a context was sent) has asked git once
//! for it. The branch is read again only when the worktree's `HEAD` file changed, which is a
//! stat per poll. A failed lookup is tried again after 1 s, then 2, 4 … up to a minute; a path
//! that does not exist is never tried again. Chat counts are kept here too: they are known only
//! once an agent's transcript was read (see [`crate::detail::detail`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

/// How long the first failed git lookup of a worktree stands; each further failure doubles it.
const FIRST_RETRY: Duration = Duration::from_secs(1);
/// The longest a failed lookup stands.
const LAST_RETRY: Duration = Duration::from_secs(60);

/// Modification time and length: a rewritten `HEAD` changes at least one of them.
type HeadStamp = (SystemTime, u64);

enum Lookup {
    /// Not asked yet.
    Pending,
    Found {
        /// The worktree's own git directory, where its `HEAD` lives.
        git_directory: PathBuf,
        head_stamp: Option<HeadStamp>,
        branch: Option<String>,
    },
    /// Git could not say; asked again from `retry_at`.
    Failed { failures: u32, retry_at: Instant },
    /// The path does not exist: never asked again.
    Gone,
}

struct Entry {
    /// The parent directory until git says otherwise.
    main_checkout: PathBuf,
    lookup: Lookup,
}

impl Entry {
    fn is_due(&self) -> bool {
        match &self.lookup {
            Lookup::Pending => true,
            Lookup::Found {
                git_directory,
                head_stamp,
                ..
            } => head_stamp_of(git_directory) != *head_stamp,
            Lookup::Failed { retry_at, .. } => Instant::now() >= *retry_at,
            Lookup::Gone => false,
        }
    }
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

    /// The main checkout of the repository `worktree` belongs to (the parent of its common git
    /// directory), as far as it is known: the worktree's parent directory until git was asked,
    /// and when git cannot say (a deleted repository).
    pub fn main_checkout(&mut self, worktree: &Path) -> PathBuf {
        self.entries
            .entry(worktree.to_owned())
            .or_insert_with(|| Entry {
                main_checkout: worktree.parent().unwrap_or(worktree).to_owned(),
                lookup: Lookup::Pending,
            })
            .main_checkout
            .clone()
    }

    /// The branch checked out in `worktree`, as far as it is known; `None` when detached, when
    /// git failed or was not asked yet.
    pub fn branch(&self, worktree: &Path) -> Option<String> {
        match &self.entries.get(worktree)?.lookup {
            Lookup::Found { branch, .. } => branch.clone(),
            _ => None,
        }
    }

    /// Whether [`ProjectCache::look_up_due`] has git to ask: a worktree not asked yet, one whose
    /// `HEAD` changed, or a failed one whose wait is over.
    pub fn is_due(&self) -> bool {
        self.entries.values().any(Entry::is_due)
    }

    /// Asks git about every worktree [`ProjectCache::is_due`] counts.
    pub fn look_up_due(&mut self) {
        for (worktree, entry) in &mut self.entries {
            if !entry.is_due() {
                continue;
            }
            match &mut entry.lookup {
                Lookup::Found {
                    git_directory,
                    head_stamp,
                    branch,
                } => {
                    *head_stamp = head_stamp_of(git_directory);
                    *branch = git(worktree, &["symbolic-ref", "--short", "HEAD"]);
                }
                lookup => {
                    let failures = match lookup {
                        Lookup::Failed { failures, .. } => *failures,
                        _ => 0,
                    };
                    *entry = look_up(worktree, failures);
                }
            }
        }
    }

    pub fn record_chats(&mut self, agent_id: &str, chats: usize) {
        self.chats.insert(agent_id.to_owned(), chats);
    }

    /// The message count of an agent whose detail was computed, else 0.
    pub fn chats(&self, agent_id: &str) -> usize {
        self.chats.get(agent_id).copied().unwrap_or(0)
    }
}

/// How long a lookup that failed `failures` times in a row stands.
fn retry_after(failures: u32) -> Duration {
    FIRST_RETRY
        .saturating_mul(2_u32.saturating_pow(failures.saturating_sub(1)))
        .min(LAST_RETRY)
}

/// The entry git gives `worktree`, which failed `failures` times before.
fn look_up(worktree: &Path, failures: u32) -> Entry {
    let parent = worktree.parent().unwrap_or(worktree).to_owned();
    if !worktree.exists() {
        return Entry {
            main_checkout: parent,
            lookup: Lookup::Gone,
        };
    }
    let failed = || Entry {
        main_checkout: parent.clone(),
        lookup: Lookup::Failed {
            failures: failures + 1,
            retry_at: Instant::now() + retry_after(failures + 1),
        },
    };
    let Some(output) = git(worktree, &["rev-parse", "--git-dir", "--git-common-dir"]) else {
        return failed();
    };
    let mut lines = output.lines();
    let resolved = |line: Option<&str>| worktree.join(line?).canonicalize().ok();
    let (Some(git_directory), Some(common_directory)) =
        (resolved(lines.next()), resolved(lines.next()))
    else {
        return failed();
    };
    let Some(main_checkout) = common_directory.parent() else {
        return failed();
    };
    Entry {
        main_checkout: main_checkout.to_owned(),
        lookup: Lookup::Found {
            head_stamp: head_stamp_of(&git_directory),
            branch: git(worktree, &["symbolic-ref", "--short", "HEAD"]),
            git_directory,
        },
    }
}

fn head_stamp_of(git_directory: &Path) -> Option<HeadStamp> {
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

    /// The main checkout and branch of `worktree` once git was asked.
    fn resolved(cache: &mut ProjectCache, worktree: &Path) -> (PathBuf, Option<String>) {
        cache.main_checkout(worktree);
        cache.look_up_due();
        (cache.main_checkout(worktree), cache.branch(worktree))
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
        assert_eq!(
            resolved(&mut cache, &worktree),
            (main.clone(), Some("feature".into()))
        );
        assert_eq!(
            resolved(&mut cache, &main),
            (main.clone(), Some("main".into()))
        );
        assert!(!cache.is_due());
    }

    #[test]
    fn reading_never_runs_git_and_an_unknown_worktree_sits_under_its_parent() {
        let root = tempfile::tempdir().unwrap();
        let main = repository(root.path());
        let mut cache = ProjectCache::new();
        assert!(!cache.is_due());
        assert_eq!(
            cache.main_checkout(&main),
            root.path().canonicalize().unwrap()
        );
        assert_eq!(cache.branch(&main), None);
        assert!(cache.is_due());
        cache.look_up_due();
        assert!(!cache.is_due());
        assert_eq!(cache.main_checkout(&main), main);
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
        assert_eq!(
            resolved(&mut cache, &worktree),
            (worktree.parent().unwrap().to_owned(), None)
        );
        let gone = root.path().join("gone/worktree");
        assert_eq!(
            resolved(&mut cache, &gone),
            (root.path().join("gone"), None)
        );
    }

    #[test]
    fn branch_follows_head_changes() {
        let root = tempfile::tempdir().unwrap();
        let main = repository(root.path());
        let mut cache = ProjectCache::new();
        assert_eq!(resolved(&mut cache, &main).1.as_deref(), Some("main"));
        run_git(&main, &["checkout", "-b", "a-longer-branch-name"]);
        assert!(cache.is_due());
        assert_eq!(
            resolved(&mut cache, &main).1.as_deref(),
            Some("a-longer-branch-name")
        );
        run_git(&main, &["checkout", "--detach"]);
        assert_eq!(resolved(&mut cache, &main).1, None);
        run_git(&main, &["checkout", "main"]);
        assert_eq!(resolved(&mut cache, &main).1.as_deref(), Some("main"));
    }

    #[test]
    fn an_empty_path_is_not_a_repository() {
        let mut cache = ProjectCache::new();
        assert_eq!(resolved(&mut cache, Path::new("")), (PathBuf::new(), None));
        assert!(!cache.is_due());
    }

    #[test]
    fn a_failed_lookup_is_retried_after_the_retry_window() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("later");
        std::fs::create_dir(&directory).unwrap();
        let mut cache = ProjectCache::new();
        assert_eq!(
            resolved(&mut cache, &directory),
            (root.path().to_owned(), None)
        );
        run_git(&directory, &["init", "-b", "main"]);
        run_git(&directory, &["commit", "--allow-empty", "-m", "first"]);
        assert!(!cache.is_due());
        std::thread::sleep(FIRST_RETRY + Duration::from_millis(100));
        assert!(cache.is_due());
        assert_eq!(
            resolved(&mut cache, &directory),
            (directory.canonicalize().unwrap(), Some("main".into()))
        );
    }

    #[test]
    fn a_missing_path_is_not_retried() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("missing");
        let mut cache = ProjectCache::new();
        assert_eq!(
            resolved(&mut cache, &directory),
            (root.path().to_owned(), None)
        );
        std::fs::create_dir(&directory).unwrap();
        run_git(&directory, &["init", "-b", "main"]);
        std::thread::sleep(FIRST_RETRY + Duration::from_millis(100));
        assert!(!cache.is_due());
        assert_eq!(
            resolved(&mut cache, &directory),
            (root.path().to_owned(), None)
        );
    }

    #[test]
    fn failed_lookups_back_off_to_a_minute() {
        let waits: Vec<u64> = (1..=8)
            .map(|failures| retry_after(failures).as_secs())
            .collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(retry_after(u32::MAX), LAST_RETRY);
    }

    #[test]
    fn chats_are_zero_until_recorded() {
        let mut cache = ProjectCache::new();
        assert_eq!(cache.chats("a"), 0);
        cache.record_chats("a", 7);
        assert_eq!(cache.chats("a"), 7);
    }
}
