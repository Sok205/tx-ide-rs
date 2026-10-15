//! Per-kind behaviour: what `create` puts in the directory, what `open` spawns, and what `close`
//! undoes. The record carries structured facts (`Kind`); this module turns them into commands.
//!
//! | verb     | document                      | diff                                   | tour                                  |
//! |----------|-------------------------------|----------------------------------------|---------------------------------------|
//! | create   | copy the file into `{dir}`    | write `{dir}/diff.patch`               | copy the stops file into `{dir}`      |
//! | refresh  | nothing                       | rewrite `diff.patch`                   | nothing                               |
//! | open     | nvim on `{dir}/<file>`        | diffview in the worktree               | nvim in the worktree with the tour    |
//! | close    | nothing                       | remove a temp worktree, if one was made| nothing                               |
//!
//! A diff whose worktree is gone is opened in a temporary detached worktree at `base` (made
//! from a sibling checkout of the same repository) with the patch applied; `close` removes it.
//! When no checkout of the repository can be found, the patch itself is opened as text.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::artifact::{Artifact, Kind};
use crate::artifact_store::atomic_write;
use crate::shlex::shlex_quote;
use crate::spawn::SpawnSpec;

/// The patch a diff keeps beside its record.
pub const DIFF_PATCH: &str = "diff.patch";

/// The refs tried, in order, as the default branch when a diff's base is not given.
const DEFAULT_BRANCH_CANDIDATES: [&str; 3] = ["origin/HEAD", "main", "master"];

#[derive(Debug, thiserror::Error)]
pub enum KindError {
    #[error("artifact kind '{0}' is not supported yet")]
    Unsupported(&'static str),
    #[error("{} file {file} is missing from {} (see `tx artifact doctor`)", kind, dir.display())]
    ContentMissing {
        kind: &'static str,
        file: String,
        dir: PathBuf,
    },
    #[error("worktree {} does not exist", .0.display())]
    WorktreeMissing(PathBuf),
    #[error("no default branch (tried {}) in {} — pass --base", DEFAULT_BRANCH_CANDIDATES.join(", "), .0.display())]
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

/// What `open` decided: the view to spawn, and what to undo once that view ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub spec: SpawnSpec,
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

/// The file the kind keeps under `{dir}` (what `doctor` expects to find).
pub fn content_file(kind: &Kind) -> &str {
    match kind {
        Kind::Document { file } => file,
        Kind::Diff { .. } => DIFF_PATCH,
        Kind::Tour { stops, .. } => stops,
    }
}

/// A short label for listings: `document`, `diff vs <base7>`, `tour`.
pub fn describe(kind: &Kind) -> String {
    match kind {
        Kind::Document { .. } => "document".into(),
        Kind::Diff { base, .. } => format!("diff vs {}", short(base)),
        Kind::Tour { .. } => "tour".into(),
    }
}

/// The suffix a kind's view name carries after the title.
pub fn view_suffix(kind: &Kind) -> &'static str {
    match kind {
        Kind::Document { .. } => "",
        Kind::Diff { .. } => "-diff",
        Kind::Tour { .. } => "-tour",
    }
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
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

fn rev_parse(worktree: &Path, reference: &str) -> Result<String, KindError> {
    let out = git(
        worktree,
        "rev-parse",
        ["--verify", "--quiet", &format!("{reference}^{{commit}}")],
    )?;
    Ok(out.trim().to_owned())
}

/// Put the kind's content into a fresh `dir`: the source file for a document or tour, the
/// patch for a diff.
pub fn create_content(kind: &Kind, dir: &Path, source: Option<&Path>) -> Result<(), KindError> {
    match kind {
        Kind::Document { file } | Kind::Tour { stops: file, .. } => {
            let source = source.expect("a document or tour is created from a file");
            copy_file(source, dir, file)
        }
        Kind::Diff { worktree, base } => write_patch(worktree, base, dir),
    }
}

/// Replace a document's or tour's file with `source` (a diff is never hand-edited).
pub fn replace_content(kind: &Kind, dir: &Path, source: &Path) -> Result<(), KindError> {
    copy_file(source, dir, content_file(kind))
}

fn copy_file(source: &Path, dir: &Path, name: &str) -> Result<(), KindError> {
    let content = fs::read(source).map_err(io_at(source))?;
    let target = dir.join(name);
    atomic_write(dir, &format!(".{name}."), &content, &target).map_err(io_at(&target))
}

/// Bring derived content up to date: a diff's patch. Documents and tours are edited in place.
pub fn refresh(kind: &Kind, dir: &Path) -> Result<(), KindError> {
    match kind {
        Kind::Document { .. } | Kind::Tour { .. } => Ok(()),
        Kind::Diff { worktree, base } => write_patch(worktree, base, dir),
    }
}

/// `git diff <base>` in the worktree → `{dir}/diff.patch`, atomically.
fn write_patch(worktree: &Path, base: &str, dir: &Path) -> Result<(), KindError> {
    if !worktree.is_dir() {
        return Err(KindError::WorktreeMissing(worktree.to_owned()));
    }
    let patch = git(worktree, "diff", [base])?;
    let target = dir.join(DIFF_PATCH);
    atomic_write(dir, ".diff.patch.", patch.as_bytes(), &target).map_err(io_at(&target))
}

/// The nvim view for the artifact, named `name` and tagged `tags`. `worktrees_root` is where a
/// diff's temporary worktree goes when its own is gone.
pub fn open(
    artifact: &Artifact,
    dir: &Path,
    name: String,
    tags: Vec<String>,
    worktrees_root: &Path,
) -> Result<Opened, KindError> {
    require_content(&artifact.kind, dir)?;
    match &artifact.kind {
        Kind::Document { file } => {
            let path = dir.join(file);
            Ok(Opened {
                spec: SpawnSpec::for_nvim(
                    name,
                    tags,
                    dir.to_string_lossy(),
                    None,
                    Some(&path.to_string_lossy()),
                ),
                cleanup: None,
            })
        }
        Kind::Diff { worktree, base } => {
            if worktree.is_dir() {
                return Ok(Opened {
                    spec: SpawnSpec::for_nvim(
                        name,
                        tags,
                        worktree.to_string_lossy(),
                        Some(base),
                        None,
                    ),
                    cleanup: None,
                });
            }
            let Some(checkout) = sibling_checkout(worktree, base) else {
                let patch = dir.join(DIFF_PATCH);
                return Ok(Opened {
                    spec: SpawnSpec::for_nvim(
                        name,
                        tags,
                        dir.to_string_lossy(),
                        None,
                        Some(&patch.to_string_lossy()),
                    ),
                    cleanup: None,
                });
            };
            let short_id: String = artifact.id.chars().take(8).collect();
            let temp = worktrees_root.join(format!("artifact-{short_id}"));
            fs::create_dir_all(worktrees_root).map_err(io_at(worktrees_root))?;
            if temp.exists() {
                // Debris from a view that was never closed: start over.
                remove_worktree(&temp)?;
            }
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
            let patch = dir.join(DIFF_PATCH);
            let content = fs::read(&patch).map_err(io_at(&patch))?;
            if !content.iter().all(u8::is_ascii_whitespace) {
                git(&temp, "apply", [patch.as_os_str()])?;
            }
            Ok(Opened {
                spec: SpawnSpec::for_nvim(name, tags, temp.to_string_lossy(), Some(base), None),
                cleanup: Some(Cleanup::RemoveWorktree(temp)),
            })
        }
        Kind::Tour { worktree, stops } => {
            let stops_path = dir.join(stops);
            if !worktree.is_dir() {
                return Ok(Opened {
                    spec: SpawnSpec::for_nvim(
                        name,
                        tags,
                        dir.to_string_lossy(),
                        None,
                        Some(&stops_path.to_string_lossy()),
                    ),
                    cleanup: None,
                });
            }
            let mut spec = SpawnSpec::for_nvim(name, tags, worktree.to_string_lossy(), None, None);
            spec.cmd.push(' ');
            spec.cmd.push_str(&shlex_quote(&format!(
                "+lua require(\"tx-tour\")({})",
                lua_string(&stops_path.to_string_lossy())
            )));
            Ok(Opened {
                spec,
                cleanup: None,
            })
        }
    }
}

/// A Lua double-quoted string literal.
fn lua_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Another checkout of the gone worktree's repository: a sibling directory (tx keeps a
/// repository's worktrees side by side) that has the base commit.
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

/// The kind's content file must be there before a view opens on it: nvim on a missing path
/// would silently start a new empty buffer (and a save would recreate the artifact's file).
fn require_content(kind: &Kind, dir: &Path) -> Result<(), KindError> {
    let file = content_file(kind);
    if dir.join(file).is_file() {
        return Ok(());
    }
    Err(KindError::ContentMissing {
        kind: kind.name(),
        file: file.to_owned(),
        dir: dir.to_owned(),
    })
}

/// Run a pending close step.
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

/// `git -C <dir> <args…>`: stdout on success, git's stripped stderr (else stdout) as the error.
fn git<S: AsRef<OsStr>>(
    dir: &Path,
    verb: &str,
    arguments: impl IntoIterator<Item = S>,
) -> Result<String, KindError> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir);
    if !verb.contains(' ') {
        command.arg(verb);
    }
    let output = command
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|source| KindError::Io {
            path: PathBuf::from("git"),
            source,
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or("git command failed")
        .to_owned();
    Err(KindError::Git {
        verb: verb.to_owned(),
        dir: dir.to_owned(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spawn::NVIM_BASE_COMMAND;
    use serde_json::Number;

    #[test]
    fn document_create_replace_and_open() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("my plan.md");
        fs::write(&source, "v1").unwrap();
        let dir = root.path().join("art");
        fs::create_dir(&dir).unwrap();
        let kind = Kind::Document {
            file: "my plan.md".into(),
        };
        create_content(&kind, &dir, Some(&source)).unwrap();
        assert_eq!(fs::read_to_string(dir.join("my plan.md")).unwrap(), "v1");
        fs::write(&source, "v2").unwrap();
        replace_content(&kind, &dir, &source).unwrap();
        assert_eq!(fs::read_to_string(dir.join("my plan.md")).unwrap(), "v2");
        refresh(&kind, &dir).unwrap();

        let artifact = Artifact::new(
            "a".into(),
            kind,
            "Plan".into(),
            "s1".into(),
            Number::from(1),
        );
        let opened = open(
            &artifact,
            &dir,
            "Plan".into(),
            vec!["t".into()],
            root.path(),
        )
        .unwrap();
        assert_eq!(opened.cleanup, None);
        assert_eq!(opened.spec.cwd, dir.to_string_lossy());
        assert_eq!(
            opened.spec.cmd,
            format!("{NVIM_BASE_COMMAND} '{}'", dir.join("my plan.md").display())
        );
        assert_eq!(opened.spec.tags, ["t"]);
        assert_eq!(content_file(&artifact.kind), "my plan.md");
        fs::remove_file(dir.join("my plan.md")).unwrap();
        let missing = open(&artifact, &dir, "Plan".into(), vec![], root.path()).unwrap_err();
        assert!(matches!(missing, KindError::ContentMissing { .. }));
        assert!(
            missing
                .to_string()
                .starts_with("document file my plan.md is missing from "),
            "{missing}"
        );
        assert_eq!(describe(&artifact.kind), "document");
        assert_eq!(view_suffix(&artifact.kind), "");
    }

    #[test]
    fn tour_opens_in_the_worktree_with_the_runtime() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join("tree");
        fs::create_dir(&worktree).unwrap();
        let dir = root.path().join("art");
        fs::create_dir(&dir).unwrap();
        let source = root.path().join("stops.lua");
        fs::write(&source, "return {}").unwrap();
        let kind = Kind::Tour {
            worktree: worktree.clone(),
            stops: "stops.lua".into(),
        };
        create_content(&kind, &dir, Some(&source)).unwrap();
        let artifact = Artifact::new(
            "t".into(),
            kind,
            "tour".into(),
            "s1".into(),
            Number::from(1),
        );
        let opened = open(&artifact, &dir, "tour-tour".into(), vec![], root.path()).unwrap();
        assert_eq!(opened.cleanup, None);
        assert_eq!(opened.spec.cwd, worktree.to_string_lossy());
        assert_eq!(
            opened.spec.cmd,
            format!(
                "{NVIM_BASE_COMMAND} '+lua require(\"tx-tour\")(\"{}\")'",
                dir.join("stops.lua").display()
            )
        );
        fs::remove_dir(&worktree).unwrap();
        let text = open(&artifact, &dir, "tour-tour".into(), vec![], root.path()).unwrap();
        assert_eq!(text.spec.cwd, dir.to_string_lossy());
        assert!(text.spec.cmd.ends_with("stops.lua"), "{}", text.spec.cmd);
        assert_eq!(lua_string("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }

    #[test]
    fn labels_and_cleanup_round_trip() {
        let diff = Kind::Diff {
            worktree: "/w".into(),
            base: "b28bae8af3042e3e".into(),
        };
        assert_eq!(describe(&diff), "diff vs b28bae8");
        assert_eq!(view_suffix(&diff), "-diff");
        assert_eq!(content_file(&diff), DIFF_PATCH);
        let tour = Kind::Tour {
            worktree: "/w".into(),
            stops: "stops.lua".into(),
        };
        assert_eq!(describe(&tour), "tour");
        assert_eq!(view_suffix(&tour), "-tour");
        let cleanup = Cleanup::RemoveWorktree("/tmp/x".into());
        assert_eq!(Cleanup::from_value(&cleanup.to_value()), Some(cleanup));
        assert_eq!(Cleanup::from_value(&json!({})), None);
    }

    /// A repository with `main` at one commit and a detached linked worktree one commit ahead.
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

        let dir = main.parent().unwrap().parent().unwrap().join("art");
        fs::create_dir(&dir).unwrap();
        let kind = Kind::Diff {
            worktree: worktree.clone(),
            base: base.clone(),
        };
        create_content(&kind, &dir, None).unwrap();
        let patch = fs::read_to_string(dir.join(DIFF_PATCH)).unwrap();
        assert!(patch.contains("+two\n+three\n"), "{patch}");

        let artifact = Artifact::new(
            "d1234567-rest".into(),
            kind,
            "review".into(),
            "s1".into(),
            Number::from(1),
        );
        let worktrees_root = dir.parent().unwrap().join("worktrees");
        let live = open(
            &artifact,
            &dir,
            "review-diff".into(),
            vec![],
            &worktrees_root,
        )
        .unwrap();
        assert_eq!(live.cleanup, None);
        assert_eq!(live.spec.cwd, worktree.to_string_lossy());
        assert_eq!(
            live.spec.cmd,
            format!("{NVIM_BASE_COMMAND} +'DiffviewOpen {base}'")
        );

        git_ok(
            main,
            &["worktree", "remove", "--force", worktree.to_str().unwrap()],
        );
        assert!(!worktree.exists());
        let fallback = open(
            &artifact,
            &dir,
            "review-diff".into(),
            vec![],
            &worktrees_root,
        )
        .unwrap();
        let temp = worktrees_root.join("artifact-d1234567");
        assert_eq!(
            fallback.cleanup,
            Some(Cleanup::RemoveWorktree(temp.clone()))
        );
        assert_eq!(fallback.spec.cwd, temp.to_string_lossy());
        assert_eq!(
            fs::read_to_string(temp.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
        assert_eq!(git_ok(&temp, &["rev-parse", "HEAD"]), *base);

        // Opening again while the debris is still there starts over rather than failing.
        let again = open(
            &artifact,
            &dir,
            "review-diff".into(),
            vec![],
            &worktrees_root,
        )
        .unwrap();
        assert_eq!(again.cleanup, fallback.cleanup);

        close(fallback.cleanup.as_ref().unwrap()).unwrap();
        assert!(!temp.exists());
        close(fallback.cleanup.as_ref().unwrap()).unwrap();
        assert!(!git_ok(main, &["worktree", "list"]).contains("artifact-d1234567"));

        // No checkout of the repository anywhere near: the patch opens as text.
        let orphan = Artifact::new(
            "o".into(),
            Kind::Diff {
                worktree: PathBuf::from("/nonexistent/repo--gone"),
                base: base.clone(),
            },
            "orphan".into(),
            "s1".into(),
            Number::from(1),
        );
        let text = open(&orphan, &dir, "orphan-diff".into(), vec![], &worktrees_root).unwrap();
        assert_eq!(text.cleanup, None);
        assert!(text.spec.cmd.ends_with(DIFF_PATCH), "{}", text.spec.cmd);
        assert!(matches!(
            refresh(&orphan.kind, &dir),
            Err(KindError::WorktreeMissing(_))
        ));
    }
}
