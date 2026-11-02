//! Per-kind behaviour, ported from production `crates/tx/src/artifact_kinds.rs` (the diff half).
//! The record carries structured facts (`Kind`); this module turns them into content and views.
//!
//! | step    | document                   | diff                                                 |
//! |---------|----------------------------|------------------------------------------------------|
//! | create  | the source file's bytes    | the patch of the worktree against `base`             |
//! | refresh | nothing (edited in place)  | the patch again (snapshotted by the service as a rev) |
//! | open    | nvim on `current<ext>`     | diffview against `base` in the worktree              |
//! | close   | nothing                    | remove the temp worktree, if one was made            |
//!
//! Unlike production, a diff's patch is the artifact's versioned content: `current.patch` is the
//! latest one and `revs/<n>.patch` the earlier snapshots, so `modify` and `open` re-snapshot it
//! through the service instead of rewriting a file here.
//!
//! A diff whose worktree is gone is opened in a temporary detached worktree at `base` (made from
//! the repository recorded at create, else a sibling checkout of the gone worktree, one per
//! open) with the patch applied; `close` removes it. When no checkout of the repository can be
//! found, the patch itself is opened as text. Any other kind is opened by the plugin that
//! registers the verb named after it.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::artifact::{Artifact, Kind};

/// The file name a diff's working copy and revisions carry (`current.patch`, `revs/<n>.patch`).
pub const DIFF_PATCH: &str = "diff.patch";

/// The refs tried, in order, as the default branch when a diff's base is not given.
const DEFAULT_BRANCH_CANDIDATES: [&str; 3] = ["origin/HEAD", "main", "master"];

#[derive(Debug, thiserror::Error)]
pub enum KindError {
    #[error("worktree {} does not exist", .0.display())]
    WorktreeMissing(PathBuf),
    #[error("no default branch (tried {}) in {}; pass --base", DEFAULT_BRANCH_CANDIDATES.join(", "), .0.display())]
    NoDefaultBranch(PathBuf),
    #[error("git {verb} in {}: {detail}", dir.display())]
    Git {
        verb: String,
        dir: PathBuf,
        detail: String,
    },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> KindError + '_ {
    move |source| KindError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// What `open` decided: where the editor runs, what it is told, and what to undo once that view
/// ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub working_directory: PathBuf,
    /// The editor's arguments (a file to open, or `-c 'DiffviewOpen <base>'`).
    pub arguments: Vec<String>,
    /// Whether the view shows a diffview (else a file).
    pub diffview: bool,
    pub cleanup: Option<Cleanup>,
}

/// A pending close step, persisted beside the record until the view ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cleanup {
    RemoveWorktree(PathBuf),
}

impl Cleanup {
    pub fn to_value(&self) -> Value {
        match self {
            Self::RemoveWorktree(path) => json!({"remove_worktree": path.to_string_lossy()}),
        }
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let path = value.get("remove_worktree")?.as_str()?;
        Some(Self::RemoveWorktree(PathBuf::from(path)))
    }

    pub fn describe(&self) -> String {
        match self {
            Self::RemoveWorktree(path) => format!("remove temp worktree {}", path.display()),
        }
    }
}

/// The suffix a kind's view name carries after the title.
pub fn view_suffix(kind: &Kind) -> &'static str {
    match kind {
        Kind::Diff { .. } => "-diff",
        Kind::Document { .. } | Kind::Other { .. } => "",
    }
}

/// The full commit id a diff pins: `base` resolved in `worktree`, or when empty the merge-base
/// of the default branch (`origin/HEAD`, else `main`, else `master`) and `HEAD`.
pub fn resolve_base(worktree: &Path, base: &str) -> Result<String, KindError> {
    if !worktree.is_dir() {
        return Err(KindError::WorktreeMissing(worktree.to_owned()));
    }
    if !base.is_empty() {
        return rev_parse(worktree, base);
    }
    let default = DEFAULT_BRANCH_CANDIDATES
        .iter()
        .find(|candidate| rev_parse(worktree, candidate).is_ok())
        .ok_or_else(|| KindError::NoDefaultBranch(worktree.to_owned()))?;
    let out = git(worktree, "merge-base", [default, "HEAD"])?;
    Ok(out.trim().to_owned())
}

/// The checkout's git common directory (the main repository's `.git`), which outlives a linked
/// worktree's removal; where a temporary worktree is made from once the worktree is gone.
pub fn common_directory(worktree: &Path) -> Result<PathBuf, KindError> {
    if !worktree.is_dir() {
        return Err(KindError::WorktreeMissing(worktree.to_owned()));
    }
    let out = git(
        worktree,
        "rev-parse",
        ["--path-format=absolute", "--git-common-dir"],
    )?;
    Ok(PathBuf::from(out.trim()))
}

fn rev_parse(worktree: &Path, reference: &str) -> Result<String, KindError> {
    let out = git(
        worktree,
        "rev-parse",
        ["--verify", "--quiet", &format!("{reference}^{{commit}}")],
    )?;
    Ok(out.trim().to_owned())
}

/// The kind's content at create: a document's source file, a diff's patch.
pub fn create_content(kind: &Kind, source: Option<&Path>) -> Result<Vec<u8>, KindError> {
    match kind {
        Kind::Document { .. } | Kind::Other { .. } => {
            let source = source.expect("a document is created from a file");
            fs::read(source).map_err(io_at(source))
        }
        Kind::Diff { worktree, base, .. } => patch(worktree, base),
    }
}

/// Derived content brought up to date: a diff's patch. A document is edited in place (`None`).
pub fn refresh(kind: &Kind) -> Result<Option<Vec<u8>>, KindError> {
    match kind {
        Kind::Document { .. } | Kind::Other { .. } => Ok(None),
        Kind::Diff { worktree, base, .. } => patch(worktree, base).map(Some),
    }
}

/// `git diff <base>` in the worktree, followed by one `new file` hunk per untracked file
/// (`git diff --no-index /dev/null <file>`), so `git apply` recreates the whole tree.
pub fn patch(worktree: &Path, base: &str) -> Result<Vec<u8>, KindError> {
    if !worktree.is_dir() {
        return Err(KindError::WorktreeMissing(worktree.to_owned()));
    }
    let mut out = git_bytes(worktree, "diff", [OsStr::new(base)])?;
    let untracked = git(
        worktree,
        "ls-files",
        ["-z", "--others", "--exclude-standard"],
    )?;
    for file in untracked.split('\0').filter(|file| !file.is_empty()) {
        // `diff --no-index` exits 1 when the files differ, which they always do here.
        let output = git_output(
            worktree,
            [
                OsStr::new("diff"),
                OsStr::new("--no-index"),
                OsStr::new("--"),
                OsStr::new("/dev/null"),
                OsStr::new(file),
            ],
        )?;
        if output.status.code().is_none_or(|code| code > 1) {
            return Err(git_failure(worktree, "diff --no-index", &output));
        }
        out.extend_from_slice(&output.stdout);
    }
    Ok(out)
}

/// The view for the artifact. `current` is its working copy (`current<ext>`); `worktrees_root`
/// is where a diff's temporary worktree goes when its own is gone.
pub fn open(
    artifact: &Artifact,
    current: &Path,
    worktrees_root: &Path,
) -> Result<Opened, KindError> {
    let file_view = |path: &Path| Opened {
        working_directory: path.parent().expect("a file has a directory").to_path_buf(),
        arguments: vec![path.to_string_lossy().into_owned()],
        diffview: false,
        cleanup: None,
    };
    let Kind::Diff {
        worktree,
        base,
        repository,
    } = &artifact.kind
    else {
        return Ok(file_view(current));
    };
    let diffview = |working_directory: PathBuf, cleanup: Option<Cleanup>| Opened {
        working_directory,
        arguments: vec!["-c".to_owned(), format!("DiffviewOpen {base}")],
        diffview: true,
        cleanup,
    };
    if worktree.is_dir() {
        return Ok(diffview(worktree.clone(), None));
    }
    let recorded = repository
        .as_deref()
        .filter(|repository| rev_parse(repository, base).is_ok())
        .map(Path::to_path_buf);
    let Some(checkout) = recorded.or_else(|| sibling_checkout(worktree, base)) else {
        return Ok(file_view(current));
    };
    // One temp worktree per open: two views of the same diff never share one, so each view's
    // close removes only its own.
    let short_id: String = artifact.id.chars().take(8).collect();
    let token: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
    let temp = worktrees_root.join(format!("artifact-{short_id}-{token}"));
    fs::create_dir_all(worktrees_root).map_err(io_at(worktrees_root))?;
    git(
        &checkout,
        "worktree add",
        [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--detach"),
            temp.as_os_str(),
            OsStr::new(base),
        ],
    )?;
    let content = fs::read(current).map_err(io_at(current))?;
    if !content.iter().all(u8::is_ascii_whitespace) {
        git(&temp, "apply", [current.as_os_str()])?;
    }
    Ok(diffview(temp.clone(), Some(Cleanup::RemoveWorktree(temp))))
}

/// Another checkout of the gone worktree's repository, for a diff recorded without one: a
/// sibling directory (production keeps a repository's worktrees side by side) that has the base
/// commit.
fn sibling_checkout(worktree: &Path, base: &str) -> Option<PathBuf> {
    let parent = worktree.parent()?;
    let mut siblings: Vec<PathBuf> = fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path != worktree)
        .collect();
    siblings.sort();
    siblings
        .into_iter()
        .find(|sibling| rev_parse(sibling, base).is_ok())
}

/// Run a pending close step; already done is fine.
pub fn close(cleanup: &Cleanup) -> Result<(), KindError> {
    match cleanup {
        Cleanup::RemoveWorktree(temp) => remove_worktree(temp),
    }
}

/// `git worktree remove --force` on a tx-made temp worktree; a directory git no longer knows is
/// deleted outright. Already gone is fine.
fn remove_worktree(temp: &Path) -> Result<(), KindError> {
    if !temp.exists() {
        return Ok(());
    }
    if git(
        temp,
        "worktree remove",
        [
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            temp.as_os_str(),
        ],
    )
    .is_err()
    {
        fs::remove_dir_all(temp).map_err(io_at(temp))?;
    }
    Ok(())
}

fn git_output<S: AsRef<OsStr>>(
    dir: &Path,
    arguments: impl IntoIterator<Item = S>,
) -> Result<std::process::Output, KindError> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| KindError::Io {
            path: PathBuf::from("git"),
            source,
        })
}

/// Git's stripped stderr (else stdout) as the error.
fn git_failure(dir: &Path, verb: &str, output: &std::process::Output) -> KindError {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or("git command failed")
        .to_owned();
    KindError::Git {
        verb: verb.to_owned(),
        dir: dir.to_owned(),
        detail,
    }
}

/// `git -C <dir> <verb> <args…>` (a verb with a space is already in the args): stdout bytes on
/// success.
fn git_bytes<S: AsRef<OsStr>>(
    dir: &Path,
    verb: &str,
    arguments: impl IntoIterator<Item = S>,
) -> Result<Vec<u8>, KindError> {
    let mut words: Vec<&OsStr> = Vec::new();
    if !verb.contains(' ') {
        words.push(OsStr::new(verb));
    }
    let arguments: Vec<S> = arguments.into_iter().collect();
    words.extend(arguments.iter().map(AsRef::as_ref));
    let output = git_output(dir, words)?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(git_failure(dir, verb, &output))
}

fn git<S: AsRef<OsStr>>(
    dir: &Path,
    verb: &str,
    arguments: impl IntoIterator<Item = S>,
) -> Result<String, KindError> {
    git_bytes(dir, verb, arguments).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Number;

    fn artifact(id: &str, kind: Kind, filename: &str) -> Artifact {
        Artifact::new(
            id.into(),
            kind,
            Some("review".into()),
            filename.into(),
            Number::from(1),
            None,
            "s1".into(),
        )
    }

    #[test]
    fn documents_open_on_their_working_copy() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("my plan.md");
        fs::write(&source, "v1").unwrap();
        let kind = Kind::Document {
            file: "my plan.md".into(),
        };
        assert_eq!(create_content(&kind, Some(&source)).unwrap(), b"v1");
        assert_eq!(refresh(&kind).unwrap(), None);
        let current = root.path().join("art").join("current.md");
        let opened = open(
            &artifact("a", kind.clone(), "my plan.md"),
            &current,
            root.path(),
        )
        .unwrap();
        assert_eq!(
            opened,
            Opened {
                working_directory: root.path().join("art"),
                arguments: vec![current.to_string_lossy().into_owned()],
                diffview: false,
                cleanup: None,
            }
        );
        assert_eq!(view_suffix(&kind), "");
    }

    #[test]
    fn labels_and_cleanup_round_trip() {
        let diff = Kind::Diff {
            worktree: "/w".into(),
            base: "b28bae8af3042e3e".into(),
            repository: None,
        };
        assert_eq!(view_suffix(&diff), "-diff");
        let cleanup = Cleanup::RemoveWorktree("/tmp/x".into());
        assert_eq!(Cleanup::from_value(&cleanup.to_value()), Some(cleanup));
        assert_eq!(Cleanup::from_value(&json!({})), None);
    }

    /// A repository with `main` at one commit and a detached linked worktree one commit ahead,
    /// with an uncommitted edit and an untracked file.
    struct Repository {
        _root: tempfile::TempDir,
        main: PathBuf,
        worktree: PathBuf,
        base: String,
    }

    fn git_ok(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn repository() -> Repository {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("repos").join("main");
        fs::create_dir_all(&main).unwrap();
        git_ok(&main, &["init", "-q", "-b", "main"]);
        fs::write(main.join("a.txt"), "one\n").unwrap();
        git_ok(&main, &["add", "a.txt"]);
        git_ok(&main, &["commit", "-q", "-m", "base"]);
        let base = git_ok(&main, &["rev-parse", "HEAD"]);
        let worktree = root.path().join("repos").join("main--feature");
        git_ok(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                worktree.to_str().unwrap(),
                "HEAD",
            ],
        );
        fs::write(worktree.join("a.txt"), "one\ntwo\n").unwrap();
        git_ok(&worktree, &["commit", "-q", "-am", "feature"]);
        fs::write(worktree.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(worktree.join("new.txt"), "fresh\n").unwrap();
        Repository {
            _root: root,
            main,
            worktree,
            base,
        }
    }

    #[test]
    fn diff_base_patch_open_and_fallback() {
        let repository = repository();
        let Repository {
            main,
            worktree,
            base,
            ..
        } = &repository;
        assert_eq!(resolve_base(worktree, "").unwrap(), *base);
        assert_eq!(resolve_base(worktree, "main").unwrap(), *base);
        assert!(matches!(
            resolve_base(worktree, "nope"),
            Err(KindError::Git { .. })
        ));
        assert!(matches!(
            resolve_base(Path::new("/nonexistent/tree"), ""),
            Err(KindError::WorktreeMissing(_))
        ));

        let repository = common_directory(worktree).unwrap();
        assert_eq!(repository, main.join(".git").canonicalize().unwrap());
        assert!(matches!(
            common_directory(Path::new("/nonexistent/tree")),
            Err(KindError::WorktreeMissing(_))
        ));
        let kind = Kind::Diff {
            worktree: worktree.clone(),
            base: base.clone(),
            repository: None,
        };
        let patch = String::from_utf8(create_content(&kind, None).unwrap()).unwrap();
        assert!(patch.contains("+two\n+three\n"), "{patch}");
        assert!(patch.contains("+++ b/new.txt\n"), "{patch}");
        assert_eq!(refresh(&kind).unwrap().unwrap(), patch.as_bytes());

        let dir = main.parent().unwrap().parent().unwrap().join("art");
        fs::create_dir(&dir).unwrap();
        let current = dir.join("current.patch");
        fs::write(&current, &patch).unwrap();
        let artifact = artifact("d1234567-rest", kind, DIFF_PATCH);
        let worktrees_root = dir.parent().unwrap().join("worktrees");
        let live = open(&artifact, &current, &worktrees_root).unwrap();
        assert_eq!(
            live,
            Opened {
                working_directory: worktree.clone(),
                arguments: vec!["-c".into(), format!("DiffviewOpen {base}")],
                diffview: true,
                cleanup: None,
            }
        );

        git_ok(
            main,
            &["worktree", "remove", "--force", worktree.to_str().unwrap()],
        );
        assert!(!worktree.exists());
        let fallback = open(&artifact, &current, &worktrees_root).unwrap();
        let Some(Cleanup::RemoveWorktree(temp)) = fallback.cleanup.clone() else {
            panic!("a gone worktree opens in a temp one: {fallback:?}");
        };
        assert!(temp.starts_with(&worktrees_root));
        let temp_name = temp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(temp_name.starts_with("artifact-d1234567-"), "{temp_name}");
        assert_eq!(fallback.working_directory, temp);
        assert!(fallback.diffview);
        assert_eq!(
            fs::read_to_string(temp.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
        assert_eq!(fs::read_to_string(temp.join("new.txt")).unwrap(), "fresh\n");
        assert_eq!(git_ok(&temp, &["rev-parse", "HEAD"]), *base);

        // A second open gets its own temp worktree; closing one leaves the other intact, and
        // closing twice is fine.
        let again = open(&artifact, &current, &worktrees_root).unwrap();
        let Some(Cleanup::RemoveWorktree(other)) = again.cleanup.clone() else {
            panic!("{again:?}");
        };
        assert_ne!(other, temp);
        close(fallback.cleanup.as_ref().unwrap()).unwrap();
        assert!(!temp.exists());
        assert!(other.join("a.txt").exists());
        close(again.cleanup.as_ref().unwrap()).unwrap();
        assert!(!other.exists());
        close(fallback.cleanup.as_ref().unwrap()).unwrap();
        assert!(!git_ok(main, &["worktree", "list"]).contains("artifact-d1234567"));

        // A worktree gone from a directory with no sibling checkout (an agent's tree under
        // core's hashed worktrees root): the recorded repository makes the temp worktree.
        let far = super::tests::artifact(
            "f",
            Kind::Diff {
                worktree: PathBuf::from("/nonexistent/repo-1a2b3c4d/repo--agent"),
                base: base.clone(),
                repository: Some(repository.clone()),
            },
            DIFF_PATCH,
        );
        let from_repository = open(&far, &current, &worktrees_root).unwrap();
        let Some(Cleanup::RemoveWorktree(temp)) = from_repository.cleanup.clone() else {
            panic!("the recorded repository gives a temp worktree: {from_repository:?}");
        };
        assert!(from_repository.diffview);
        assert_eq!(fs::read_to_string(temp.join("new.txt")).unwrap(), "fresh\n");
        assert_eq!(git_ok(&temp, &["rev-parse", "HEAD"]), *base);
        close(from_repository.cleanup.as_ref().unwrap()).unwrap();
        assert!(!temp.exists());

        // No checkout of the repository anywhere near, and none recorded (or the recorded one is
        // gone too): the patch opens as text.
        for repository in [None, Some(PathBuf::from("/nonexistent/repo/.git"))] {
            let orphan = super::tests::artifact(
                "o",
                Kind::Diff {
                    worktree: PathBuf::from("/nonexistent/repo--gone"),
                    base: base.clone(),
                    repository,
                },
                DIFF_PATCH,
            );
            let text = open(&orphan, &current, &worktrees_root).unwrap();
            assert_eq!(text.cleanup, None);
            assert!(!text.diffview);
            assert_eq!(text.arguments, [current.to_string_lossy().into_owned()]);
            assert!(matches!(
                refresh(&orphan.kind),
                Err(KindError::WorktreeMissing(_))
            ));
        }
    }
}
