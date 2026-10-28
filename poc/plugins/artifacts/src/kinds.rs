//! Per-kind behaviour, ported from main's `artifact_kinds.rs`: what `create` puts in the
//! directory, what `open` runs, and what `close` undoes. The record carries structured facts
//! (`Kind`); this module turns them into an editor launch.
//!
//! | verb     | document                  | diff                                    | tour                             |
//! |----------|---------------------------|-----------------------------------------|----------------------------------|
//! | create   | copy the file into `{dir}`| write `{dir}/diff.patch`                | copy the stops file into `{dir}` |
//! | refresh  | nothing                   | rewrite `diff.patch`                    | nothing                          |
//! | open     | the editor on the file    | `DiffviewOpen <base>` in the worktree   | the worktree with the tour       |
//! | close    | nothing                   | remove a temp worktree, if one was made | nothing                          |
//!
//! A diff whose worktree is gone opens in a temporary detached worktree at `base` (made from a
//! sibling checkout of the same repository, one per open) with the patch applied; `close`
//! removes it. With no checkout around, the patch itself opens as text. The diff and tour
//! launches pass nvim arguments (`+DiffviewOpen`, `+lua`) to the configured editor.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use crate::artifact::{Artifact, Kind};
use tx_core::write_atomic;

/// The patch a diff keeps beside its record.
pub const DIFF_PATCH: &str = "diff.patch";
/// Where an open makes a diff's temp worktrees, under the scratch directory.
pub(crate) const WORKTREES_DIR: &str = "worktrees";
/// The tour runtime, shipped as a resource and written beside the store when a tour opens.
pub(crate) const TOUR_RUNTIME: &[u8] = include_bytes!("../../../../nvim/lua/tx-tour.lua");

/// The refs tried, in order, as the default branch when a diff's base is not given.
const DEFAULT_BRANCH_CANDIDATES: [&str; 3] = ["origin/HEAD", "main", "master"];

#[derive(Debug, thiserror::Error)]
pub enum KindError {
    #[error("{} file {file} is missing from {} (see `txc artifact doctor`)", kind, dir.display())]
    ContentMissing {
        kind: &'static str,
        file: String,
        dir: PathBuf,
    },
    #[error("a {0} is created from a file")]
    NoSource(&'static str),
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

/// What `open` decided: where the editor runs and its arguments, and what to undo once the view
/// ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub cwd: PathBuf,
    pub args: Vec<String>,
    pub cleanup: Option<Cleanup>,
}

impl Opened {
    fn file(cwd: &Path, file: PathBuf) -> Self {
        Self {
            cwd: cwd.to_owned(),
            args: vec![file.to_string_lossy().into_owned()],
            cleanup: None,
        }
    }
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
        Kind::Diff { base, .. } => format!("diff vs {}", base.get(..7).unwrap_or(base)),
        other => other.name().to_owned(),
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

/// The full commit a diff pins: `base` resolved in `worktree`, or when empty the merge-base of
/// the default branch (`origin/HEAD`, else `main`, else `master`) and `HEAD`.
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
    Ok(git(worktree, "merge-base", [default, "HEAD"])?
        .trim()
        .to_owned())
}

fn rev_parse(worktree: &Path, reference: &str) -> Result<String, KindError> {
    let commit = format!("{reference}^{{commit}}");
    Ok(
        git(worktree, "rev-parse", ["--verify", "--quiet", &commit])?
            .trim()
            .to_owned(),
    )
}

/// Put the kind's content into a fresh `dir`: the source file for a document or tour, the patch
/// for a diff.
pub fn create_content(kind: &Kind, dir: &Path, source: Option<&Path>) -> Result<(), KindError> {
    match kind {
        Kind::Diff { worktree, base } => write_patch(worktree, base, dir),
        Kind::Document { file } | Kind::Tour { stops: file, .. } => {
            let source = source.ok_or(KindError::NoSource(kind.name()))?;
            copy_file(source, dir, file)
        }
    }
}

/// Replace a document's or tour's file with `source` (a diff is never hand-edited).
pub fn replace_content(kind: &Kind, dir: &Path, source: &Path) -> Result<(), KindError> {
    copy_file(source, dir, content_file(kind))
}

fn copy_file(source: &Path, dir: &Path, name: &str) -> Result<(), KindError> {
    let content = fs::read(source).map_err(io_at(source))?;
    let target = dir.join(name);
    write_atomic(&target, &content).map_err(io_at(&target))
}

/// Bring derived content up to date: a diff's patch. Documents and tours are edited in place.
pub fn refresh(kind: &Kind, dir: &Path) -> Result<(), KindError> {
    match kind {
        Kind::Diff { worktree, base } => write_patch(worktree, base, dir),
        _ => Ok(()),
    }
}

/// `git diff <base>` in the worktree, atomically into `{dir}/diff.patch`.
fn write_patch(worktree: &Path, base: &str, dir: &Path) -> Result<(), KindError> {
    if !worktree.is_dir() {
        return Err(KindError::WorktreeMissing(worktree.to_owned()));
    }
    let patch = git(worktree, "diff", [base])?;
    let target = dir.join(DIFF_PATCH);
    write_atomic(&target, patch.as_bytes()).map_err(io_at(&target))
}

/// The editor launch for the artifact. `scratch` holds what an open writes outside the content
/// directory: a diff's temporary worktrees and the tour runtime.
pub fn open(artifact: &Artifact, dir: &Path, scratch: &Path) -> Result<Opened, KindError> {
    require_content(&artifact.kind, dir)?;
    match &artifact.kind {
        Kind::Document { file } => Ok(Opened::file(dir, dir.join(file))),
        Kind::Diff { worktree, base } => {
            let diffview = |cwd: &Path, cleanup| Opened {
                cwd: cwd.to_owned(),
                args: vec![format!("+DiffviewOpen {base}")],
                cleanup,
            };
            if worktree.is_dir() {
                return Ok(diffview(worktree, None));
            }
            let Some(checkout) = sibling_checkout(worktree, base) else {
                return Ok(Opened::file(dir, dir.join(DIFF_PATCH)));
            };
            // One temp worktree per open: two views of the same diff never share one, so each
            // view's close removes only its own.
            let short_id: String = artifact.id.chars().take(8).collect();
            let token: String = uuid::Uuid::new_v4().to_string().chars().take(8).collect();
            let root = scratch.join(WORKTREES_DIR);
            let temp = root.join(format!("artifact-{short_id}-{token}"));
            fs::create_dir_all(&root).map_err(io_at(&root))?;
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
            Ok(diffview(&temp, Some(Cleanup::RemoveWorktree(temp.clone()))))
        }
        Kind::Tour { worktree, stops } => {
            let stops = dir.join(stops);
            if !worktree.is_dir() {
                return Ok(Opened::file(dir, stops));
            }
            let runtime = scratch.join("tx-tour.lua");
            fs::create_dir_all(scratch).map_err(io_at(scratch))?;
            // Atomic: a concurrent open never lets `dofile` read half a file.
            write_atomic(&runtime, TOUR_RUNTIME).map_err(io_at(&runtime))?;
            Ok(Opened {
                cwd: worktree.clone(),
                args: vec![format!(
                    "+lua dofile({})({})",
                    lua_string(&runtime.to_string_lossy()),
                    lua_string(&stops.to_string_lossy())
                )],
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

/// Another checkout of the gone worktree's repository: a sibling directory (worktrees sit side
/// by side) that has the base commit.
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

/// The kind's content file must exist before a view opens on it: the editor on a missing path
/// would start an empty buffer, and a save would recreate the artifact's file.
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

/// Run a pending close step: `git worktree remove --force` on a temp worktree; a directory git
/// no longer knows is deleted outright. Already gone is fine.
pub fn close(cleanup: &Cleanup) -> Result<(), KindError> {
    let Cleanup::RemoveWorktree(temp) = cleanup;
    if !temp.exists() {
        return Ok(());
    }
    let removed = git(
        temp,
        "worktree remove",
        [
            OsStr::new("worktree"),
            OsStr::new("remove"),
            OsStr::new("--force"),
            temp.as_os_str(),
        ],
    );
    if removed.is_err() {
        fs::remove_dir_all(temp).map_err(io_at(temp))?;
    }
    Ok(())
}

/// `git -C <dir> <args…>` (a one-word `verb` is passed first): stdout on success, git's
/// stripped stderr (else stdout) as the error.
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
        .map_err(io_at(Path::new("git")))?;
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
    use serde_json::Number;

    fn artifact(kind: Kind) -> Artifact {
        Artifact::new(
            "d1234567-rest".into(),
            kind,
            "T".into(),
            "s1".into(),
            Number::from(1),
        )
    }

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
        fs::write(&source, "v2").unwrap();
        replace_content(&kind, &dir, &source).unwrap();
        assert_eq!(fs::read_to_string(dir.join("my plan.md")).unwrap(), "v2");
        refresh(&kind, &dir).unwrap();

        let artifact = artifact(kind);
        let opened = open(&artifact, &dir, root.path()).unwrap();
        assert_eq!(opened, Opened::file(&dir, dir.join("my plan.md")));
        fs::remove_file(dir.join("my plan.md")).unwrap();
        let missing = open(&artifact, &dir, root.path()).unwrap_err();
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
        let artifact = artifact(kind);
        let scratch = root.path().join(".scratch");
        let opened = open(&artifact, &dir, &scratch).unwrap();
        let runtime = scratch.join("tx-tour.lua");
        assert_eq!(opened.cwd, worktree);
        assert_eq!(
            opened.args,
            [format!(
                "+lua dofile(\"{}\")(\"{}\")",
                runtime.display(),
                dir.join("stops.lua").display()
            )]
        );
        assert_eq!(fs::read(&runtime).unwrap(), TOUR_RUNTIME);
        fs::remove_dir(&worktree).unwrap();
        let text = open(&artifact, &dir, &scratch).unwrap();
        assert_eq!(text, Opened::file(&dir, dir.join("stops.lua")));
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
        let cleanup = Cleanup::RemoveWorktree("/tmp/x".into());
        assert_eq!(Cleanup::from_value(&cleanup.to_value()), Some(cleanup));
        assert_eq!(Cleanup::from_value(&json!({})), None);
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

    #[test]
    fn diff_base_patch_open_and_fallback() {
        let root = tempfile::tempdir().unwrap();
        let main = root.path().join("repos").join("main");
        fs::create_dir_all(&main).unwrap();
        git_ok(&main, &["init", "-q", "-b", "main"]);
        fs::write(main.join("a.txt"), "one\n").unwrap();
        git_ok(&main, &["add", "a.txt"]);
        git_ok(&main, &["commit", "-q", "-m", "base"]);
        let base = git_ok(&main, &["rev-parse", "HEAD"]);
        let worktree = root.path().join("repos").join("main--feature");
        let tree = worktree.to_str().unwrap();
        git_ok(&main, &["worktree", "add", "-q", "--detach", tree, "HEAD"]);
        fs::write(worktree.join("a.txt"), "one\ntwo\n").unwrap();
        git_ok(&worktree, &["commit", "-q", "-am", "feature"]);
        fs::write(worktree.join("a.txt"), "one\ntwo\nthree\n").unwrap();

        assert_eq!(resolve_base(&worktree, "").unwrap(), base);
        assert_eq!(resolve_base(&worktree, "main").unwrap(), base);
        assert!(matches!(
            resolve_base(&worktree, "nope"),
            Err(KindError::Git { .. })
        ));
        assert!(matches!(
            resolve_base(Path::new("/nonexistent/tree"), ""),
            Err(KindError::WorktreeMissing(_))
        ));

        let dir = root.path().join("art");
        fs::create_dir(&dir).unwrap();
        let kind = Kind::Diff {
            worktree: worktree.clone(),
            base: base.clone(),
        };
        create_content(&kind, &dir, None).unwrap();
        let patch = fs::read_to_string(dir.join(DIFF_PATCH)).unwrap();
        assert!(patch.contains("+two\n+three\n"), "{patch}");

        let artifact = artifact(kind);
        let scratch = root.path().join(".scratch");
        let live = open(&artifact, &dir, &scratch).unwrap();
        assert_eq!(live.cleanup, None);
        assert_eq!(live.cwd, worktree);
        assert_eq!(live.args, [format!("+DiffviewOpen {base}")]);

        git_ok(&main, &["worktree", "remove", "--force", tree]);
        let fallback = open(&artifact, &dir, &scratch).unwrap();
        let Some(Cleanup::RemoveWorktree(temp)) = fallback.cleanup.clone() else {
            panic!("a gone worktree opens in a temp one: {fallback:?}");
        };
        assert!(temp.starts_with(scratch.join("worktrees")));
        let temp_name = temp.file_name().unwrap().to_string_lossy().into_owned();
        assert!(temp_name.starts_with("artifact-d1234567-"), "{temp_name}");
        assert_eq!(fallback.cwd, temp);
        assert_eq!(
            fs::read_to_string(temp.join("a.txt")).unwrap(),
            "one\ntwo\nthree\n"
        );
        assert_eq!(git_ok(&temp, &["rev-parse", "HEAD"]), base);

        // A second open gets its own temp worktree; closing one leaves the other intact.
        let again = open(&artifact, &dir, &scratch).unwrap();
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
        assert!(!git_ok(&main, &["worktree", "list"]).contains("artifact-d1234567"));

        // No checkout of the repository anywhere near: the patch opens as text.
        let orphan = Artifact::new(
            "o".into(),
            Kind::Diff {
                worktree: PathBuf::from("/nonexistent/repo--gone"),
                base,
            },
            "orphan".into(),
            "s1".into(),
            Number::from(1),
        );
        let text = open(&orphan, &dir, &scratch).unwrap();
        assert_eq!(text, Opened::file(&dir, dir.join(DIFF_PATCH)));
        assert!(matches!(
            refresh(&orphan.kind, &dir),
            Err(KindError::WorktreeMissing(_))
        ));
    }
}
