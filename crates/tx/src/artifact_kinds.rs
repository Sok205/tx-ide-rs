//! Per-kind behaviour: what `create` puts in the directory, what `open` spawns, and what `close`
//! undoes. The record carries structured facts (`Kind`); this module turns them into commands.
//!
//! | verb     | document                      | diff                                   | tour                                  |
//! |----------|-------------------------------|----------------------------------------|---------------------------------------|
//! | create   | copy the file into `{dir}`    | write `{dir}/diff.patch`               | copy the stops file into `{dir}`      |
//! | refresh  | nothing                       | rewrite `diff.patch`                   | nothing                               |
//! | open     | nvim on `{dir}/<file>`        | diffview in the worktree               | nvim in the worktree with the tour    |
//! | close    | nothing                       | remove a temp worktree, if one was made| nothing                               |

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::artifact::{Artifact, Kind};
use crate::artifact_store::atomic_write;
use crate::spawn::SpawnSpec;

/// The patch a diff keeps beside its record.
pub const DIFF_PATCH: &str = "diff.patch";

#[derive(Debug, thiserror::Error)]
pub enum KindError {
    #[error("artifact kind '{0}' is not supported yet")]
    Unsupported(&'static str),
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

/// Put the kind's content into a fresh `dir`: the source file for a document or tour.
pub fn create_content(kind: &Kind, dir: &Path, source: Option<&Path>) -> Result<(), KindError> {
    match kind {
        Kind::Document { file } | Kind::Tour { stops: file, .. } => {
            let source = source.expect("a document or tour is created from a file");
            copy_file(source, dir, file)
        }
        Kind::Diff { .. } => Err(KindError::Unsupported("diff")),
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
pub fn refresh(kind: &Kind, _dir: &Path) -> Result<(), KindError> {
    match kind {
        Kind::Document { .. } | Kind::Tour { .. } => Ok(()),
        Kind::Diff { .. } => Err(KindError::Unsupported("diff")),
    }
}

/// The nvim view for the artifact, named `name` and tagged `tags`.
pub fn open(
    artifact: &Artifact,
    dir: &Path,
    name: String,
    tags: Vec<String>,
) -> Result<Opened, KindError> {
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
        other => Err(KindError::Unsupported(other.name())),
    }
}

/// Run a pending close step.
pub fn close(cleanup: &Cleanup) -> Result<(), KindError> {
    match cleanup {
        Cleanup::RemoveWorktree(_) => Err(KindError::Unsupported("diff")),
    }
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
        let opened = open(&artifact, &dir, "Plan".into(), vec!["t".into()]).unwrap();
        assert_eq!(opened.cleanup, None);
        assert_eq!(opened.spec.cwd, dir.to_string_lossy());
        assert_eq!(
            opened.spec.cmd,
            format!("{NVIM_BASE_COMMAND} '{}'", dir.join("my plan.md").display())
        );
        assert_eq!(opened.spec.tags, ["t"]);
        assert_eq!(content_file(&artifact.kind), "my plan.md");
        assert_eq!(describe(&artifact.kind), "document");
        assert_eq!(view_suffix(&artifact.kind), "");
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
}
