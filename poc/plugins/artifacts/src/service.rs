//! The artifact use cases, ported from main's v4 `artifact_service.rs`.
//!
//! Composes `ArtifactStore` (records) + `ArtifactContent` (the directory) + the plugin's own
//! `.log.jsonl` under the data dir (the core's `log.jsonl` stays the core's). Every mutation logs
//! one line, a content mutation stamps `modified_at` (archive stamps `archived_at` instead), an
//! open logs a read-visibility line, and the queries do not log.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Number, Value, json};

use crate::artifact::{Artifact, Kind};
use crate::kinds::{self, Cleanup, KindError, Opened};
use crate::store::{ArtifactContent, ArtifactStore, ArtifactStoreError, Scan};

pub(crate) const LOG_FILE: &str = ".log.jsonl";
/// What opens write outside the content directories: temp worktrees, the tour runtime.
pub(crate) const SCRATCH_DIR: &str = ".scratch";
/// Pending close steps beside the record, keyed by the view session that needs them.
const CLEANUP_FILE: &str = ".cleanup.json";

/// A precondition failed; the verb prints `txc artifact: <error>`, exit 1.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("artifact '{0}' not found")]
    NotFound(String),
    #[error("artifact id prefix '{token}' is ambiguous ({count} matches); use more characters")]
    Ambiguous { token: String, count: usize },
    /// `modify <file>` on a diff: its content is derived from the worktree, never hand-edited.
    #[error(
        "artifact {0} is a diff; its patch is rewritten from the worktree, so `modify` takes no file"
    )]
    NotEditable(String),
    /// A `.cleanup.json` entry naming a path outside the plugin's temp worktrees: never run.
    #[error("artifact {id}: refusing the close step for {}: not a temp worktree under {}", path.display(), root.display())]
    ForeignCleanup {
        id: String,
        path: PathBuf,
        root: PathBuf,
    },
    #[error(transparent)]
    Kind(#[from] KindError),
    #[error(transparent)]
    Store(#[from] ArtifactStoreError),
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

type Result<T, E = ArtifactError> = std::result::Result<T, E>;

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> ArtifactError + '_ {
    move |source| ArtifactError::Io {
        path: path.to_path_buf(),
        source,
    }
}

pub struct ArtifactService {
    dir: PathBuf,
    store: ArtifactStore,
    files: ArtifactContent,
    /// The actor of read lines: the calling session's id, or `user`.
    actor: String,
}

impl ArtifactService {
    pub fn new(dir: impl Into<PathBuf>, actor: &str) -> Self {
        let dir = dir.into();
        Self {
            store: ArtifactStore::at(&dir),
            files: ArtifactContent::at(&dir),
            dir,
            actor: actor.to_owned(),
        }
    }

    pub fn actor(&self) -> &str {
        &self.actor
    }

    pub fn store(&self) -> &ArtifactStore {
        &self.store
    }

    pub fn files(&self) -> &ArtifactContent {
        &self.files
    }

    pub fn log_path(&self) -> PathBuf {
        self.dir.join(LOG_FILE)
    }

    /// Every readable record, plus the skipped files' warnings.
    pub fn scan(&self) -> Scan {
        self.store.all()
    }

    /// Every readable record; skipped ones are dropped silently.
    pub fn all(&self) -> Vec<Artifact> {
        self.store.all().artifacts
    }

    pub(crate) fn append(&self, type_: &str, msg: &str, actor: &str) -> Result<()> {
        let path = self.log_path();
        fs::create_dir_all(&self.dir).map_err(io_at(&self.dir))?;
        let mut line = json!({
            "ts": tx_core::events::now(),
            "actor": actor,
            "type": type_,
            "msg": msg,
        })
        .to_string();
        line.push('\n');
        // One O_APPEND write per line: concurrent writers never tear each other's lines.
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(line.as_bytes()))
            .map_err(io_at(&path))
    }

    fn save(&self, artifact: &Artifact) -> Result<()> {
        self.store
            .save(artifact)
            .map_err(io_at(&self.store.record_path(&artifact.id)))
    }

    // ----- mutations -----------------------------------------------------------------------

    /// Register a new artifact: its directory with the kind's content, the record, one log line.
    /// `source` is the file a document or tour is created from; a diff's empty `base` resolves
    /// to the merge-base with the default branch.
    pub fn create(
        &self,
        owner: &str,
        kind: Kind,
        title: String,
        source: Option<&Path>,
    ) -> Result<Artifact> {
        let kind = match kind {
            Kind::Diff { worktree, base } => {
                let base = kinds::resolve_base(&worktree, &base)?;
                Kind::Diff { worktree, base }
            }
            other => other,
        };
        let artifact = Artifact::new(
            uuid::Uuid::new_v4().to_string(),
            kind,
            title,
            owner.to_owned(),
            now(),
        );
        let dir = self.files.content_dir(&artifact);
        fs::create_dir_all(&dir).map_err(io_at(&dir))?;
        kinds::create_content(&artifact.kind, &dir, source)?;
        self.save(&artifact)?;
        let msg = format!(
            "{} {} {}",
            artifact.id,
            artifact.kind.name(),
            artifact.title
        );
        self.append("artifact-create", &msg, owner)?;
        Ok(artifact)
    }

    /// Stamp `modified_at` (a meaningful revision happened); with `file`, replace a document's
    /// or tour's file first. A diff's patch is rewritten instead.
    pub fn modify(&self, artifact_id: &str, actor: &str, file: Option<&Path>) -> Result<Artifact> {
        let mut artifact = self.require(artifact_id)?;
        let dir = self.files.content_dir(&artifact);
        match file {
            Some(_) if artifact.kind.is_diff() => {
                return Err(ArtifactError::NotEditable(artifact.id));
            }
            Some(source) => kinds::replace_content(&artifact.kind, &dir, source)?,
            None => kinds::refresh(&artifact.kind, &dir)?,
        }
        artifact.modified_at = now();
        self.save(&artifact)?;
        self.append("artifact-modify", &artifact.id, actor)?;
        Ok(artifact)
    }

    /// Archive (hidden from prefix+a and default listings, content kept) or unarchive: metadata
    /// only, `modified_at` untouched, one log line.
    pub fn set_archived(&self, artifact_id: &str, actor: &str, archived: bool) -> Result<Artifact> {
        let mut artifact = self.require(artifact_id)?;
        artifact.archived_at = archived.then(now);
        self.save(&artifact)?;
        let type_ = if archived {
            "artifact-archive"
        } else {
            "artifact-unarchive"
        };
        self.append(type_, &artifact.id, actor)?;
        Ok(artifact)
    }

    // ----- reads ---------------------------------------------------------------------------

    /// Decide the view for an artifact: refresh derived content (a live diff's patch, which also
    /// stamps `modified_at`), then the kind's launch. The caller starts it and binds the session;
    /// one `artifact-open` line is logged as `actor`.
    pub fn open(&self, artifact_id: &str, actor: &str) -> Result<Opened> {
        let mut artifact = self.require(artifact_id)?;
        let dir = self.files.content_dir(&artifact);
        if artifact.kind.is_diff() && artifact.kind.worktree().is_some_and(Path::is_dir) {
            kinds::refresh(&artifact.kind, &dir)?;
            artifact.modified_at = now();
            self.save(&artifact)?;
        }
        let opened = kinds::open(&artifact, &dir, &self.dir.join(SCRATCH_DIR))?;
        self.append("artifact-open", &format!("{artifact_id} → {actor}"), actor)?;
        Ok(opened)
    }

    /// Every unarchived artifact `session_id` owns.
    pub fn artifacts_for_session(&self, session_id: &str) -> Vec<Artifact> {
        let mut artifacts = self.all();
        artifacts.retain(|artifact| !artifact.is_archived() && artifact.owner == session_id);
        artifacts
    }

    // ----- close steps ---------------------------------------------------------------------

    /// The pending close steps of an artifact, by view session id.
    pub fn pending_cleanups(&self, artifact: &Artifact) -> Result<BTreeMap<String, Cleanup>> {
        let path = self.files.file_path(artifact, CLEANUP_FILE);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(source) => return Err(io_at(&path)(source)),
        };
        let data: Value =
            serde_json::from_slice(&bytes).map_err(|source| ArtifactStoreError::Record {
                path: path.clone(),
                source: Box::new(source.into()),
            })?;
        Ok(data
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(view, value)| Some((view.clone(), Cleanup::from_value(value)?)))
            .collect())
    }

    // shortcut: `.cleanup.json` is read-modify-written without a lock, so two opens of one diff at
    // the same instant can drop an entry (doctor then reports the worktree); add a lock if that bites.
    fn write_cleanups(
        &self,
        artifact: &Artifact,
        cleanups: &BTreeMap<String, Cleanup>,
    ) -> Result<()> {
        let path = self.files.file_path(artifact, CLEANUP_FILE);
        if cleanups.is_empty() {
            return match fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io_at(&path)(error)),
                _ => Ok(()),
            };
        }
        let map: Map<String, Value> = cleanups
            .iter()
            .map(|(view, cleanup)| (view.clone(), cleanup.to_value()))
            .collect();
        let payload =
            serde_json::to_string_pretty(&Value::Object(map)).map_err(ArtifactStoreError::from)?;
        tx_core::write_atomic(&path, payload.as_bytes()).map_err(io_at(&path))
    }

    /// Remember the close step an open left behind, keyed by the view that needs it, so whichever
    /// `txc` process sees that view end can run it.
    pub fn record_cleanup(&self, artifact_id: &str, view: &str, cleanup: Cleanup) -> Result<()> {
        let artifact = self.require(artifact_id)?;
        let mut cleanups = self.pending_cleanups(&artifact)?;
        cleanups.insert(view.to_owned(), cleanup);
        self.write_cleanups(&artifact, &cleanups)
    }

    /// Run the close step recorded for `view`, if any; whether one ran. One `artifact-close`
    /// line per step run.
    pub fn close(&self, artifact_id: &str, view: &str) -> Result<bool> {
        let artifact = self.require(artifact_id)?;
        let mut cleanups = self.pending_cleanups(&artifact)?;
        let Some(cleanup) = cleanups.remove(view) else {
            return Ok(false);
        };
        // The file is plain JSON on disk: only ever remove what an open made.
        let root = self.dir.join(SCRATCH_DIR).join(kinds::WORKTREES_DIR);
        let Cleanup::RemoveWorktree(path) = &cleanup;
        if !path.starts_with(&root)
            || path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(ArtifactError::ForeignCleanup {
                id: artifact.id,
                path: path.clone(),
                root,
            });
        }
        kinds::close(&cleanup)?;
        self.write_cleanups(&artifact, &cleanups)?;
        let msg = format!("{artifact_id} {view} {}", cleanup.describe());
        self.append("artifact-close", &msg, &self.actor)?;
        Ok(true)
    }

    /// Run every pending close step of an artifact now (`artifact close`); how many ran.
    pub fn close_all(&self, artifact_id: &str) -> Result<usize> {
        let artifact = self.require(artifact_id)?;
        let views: Vec<String> = self.pending_cleanups(&artifact)?.into_keys().collect();
        for view in &views {
            self.close(artifact_id, view)?;
        }
        Ok(views.len())
    }

    // ----- diagnostics ---------------------------------------------------------------------

    /// Invariant check across the store (read-only); an empty list means clean. Every record has
    /// its directory and its kind's file, every directory has a record, and no close step waits
    /// on a view that has already ended (`view_is_alive` is the core's answer).
    pub fn doctor(&self, view_is_alive: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        let artifacts = self.all();
        let record_ids: HashSet<&str> = artifacts.iter().map(|a| a.id.as_str()).collect();
        for artifact in &artifacts {
            let dir = self.files.content_dir(artifact);
            if !dir.is_dir() {
                problems.push(format!("{}: content directory is missing", artifact.id));
                continue;
            }
            let file = kinds::content_file(&artifact.kind);
            if !dir.join(file).is_file() {
                problems.push(format!(
                    "{}: {} file {file} is missing",
                    artifact.id,
                    artifact.kind.name()
                ));
            }
            for (view, cleanup) in self.stale_cleanups(artifact, view_is_alive)? {
                problems.push(format!(
                    "{}: close step for ended view {view} never ran ({}); `doctor --repair` runs it",
                    artifact.id,
                    cleanup.describe()
                ));
            }
        }
        if self.dir.exists() {
            let mut entries: Vec<(String, bool)> = fs::read_dir(&self.dir)
                .map_err(io_at(&self.dir))?
                .filter_map(Result::ok)
                .map(|entry| {
                    let is_dir = entry.path().is_dir();
                    (entry.file_name().to_string_lossy().into_owned(), is_dir)
                })
                .filter(|(name, _)| !name.starts_with('.'))
                .collect();
            entries.sort();
            for (name, is_dir) in entries {
                if is_dir && !record_ids.contains(name.as_str()) {
                    problems.push(format!(
                        "{name}: content directory has no record (out-of-band creation; the \
                         record is authoritative)"
                    ));
                }
            }
        }
        Ok(problems)
    }

    fn stale_cleanups(
        &self,
        artifact: &Artifact,
        view_is_alive: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<(String, Cleanup)>> {
        let mut cleanups = self.pending_cleanups(artifact)?;
        cleanups.retain(|view, _| !view_is_alive(view));
        Ok(cleanups.into_iter().collect())
    }

    /// Run every close step whose view has ended (`doctor --repair`); the lines describe what ran.
    pub fn repair(&self, view_is_alive: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        for artifact in self.all() {
            for (view, cleanup) in self.stale_cleanups(&artifact, view_is_alive)? {
                self.close(&artifact.id, &view)?;
                lines.push(format!(
                    "{}: ran the close step of ended view {view} ({})",
                    artifact.id,
                    cleanup.describe()
                ));
            }
        }
        Ok(lines)
    }

    // ----- resolution ----------------------------------------------------------------------

    /// A full id or a unique id prefix → the full id. The empty token never resolves.
    pub fn resolve_id(&self, token: &str) -> Result<String> {
        if token.is_empty() {
            return Err(ArtifactError::NotFound(String::new()));
        }
        if self.store.load(token)?.is_some() {
            return Ok(token.to_owned());
        }
        let matches: Vec<String> = self
            .all()
            .into_iter()
            .filter(|artifact| artifact.id.starts_with(token))
            .map(|artifact| artifact.id)
            .collect();
        match matches.as_slice() {
            [] => Err(ArtifactError::NotFound(token.to_owned())),
            [only] => Ok(only.clone()),
            _ => Err(ArtifactError::Ambiguous {
                token: token.to_owned(),
                count: matches.len(),
            }),
        }
    }

    /// The authoritative record, or `NotFound`.
    pub fn require(&self, artifact_id: &str) -> Result<Artifact> {
        self.store
            .load(artifact_id)?
            .ok_or_else(|| ArtifactError::NotFound(artifact_id.to_owned()))
    }
}

fn now() -> Number {
    // The clock is always finite; the fallback only keeps this total.
    Number::from_f64(tx_core::events::now()).unwrap_or_else(|| Number::from(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(dir: &Path) -> ArtifactService {
        ArtifactService::new(dir.join("plugins/artifacts"), "user")
    }

    fn document(service: &ArtifactService, dir: &Path, body: &str) -> Artifact {
        let source = dir.join("p.md");
        fs::write(&source, body).unwrap();
        let kind = Kind::Document {
            file: "p.md".into(),
        };
        service
            .create("s1", kind, "Plan".into(), Some(&source))
            .unwrap()
    }

    #[test]
    fn create_modify_open_and_doctor_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path());
        let artifact = document(&service, dir.path(), "a\nb\n");
        assert_eq!(artifact.owner, "s1");
        assert_eq!(artifact.created_at, artifact.modified_at);
        let path = service.files().file_path(&artifact, "p.md");
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nb\n");
        let opened = service.open(&artifact.id, "s1").unwrap();
        assert_eq!(opened.args, [path.to_string_lossy()]);
        assert!(service.doctor(&|_| true).unwrap().is_empty());

        let source = dir.path().join("p.md");
        fs::write(&source, "a\nc\n").unwrap();
        let modified = service.modify(&artifact.id, "s2", Some(&source)).unwrap();
        assert!(modified.modified_secs() >= artifact.modified_secs());
        assert_eq!(fs::read_to_string(&path).unwrap(), "a\nc\n");
        assert_eq!(service.artifacts_for_session("s1").len(), 1);
        assert!(service.artifacts_for_session("s2").is_empty());

        assert_eq!(service.resolve_id(&artifact.id[..4]).unwrap(), artifact.id);
        assert!(matches!(
            service.resolve_id(""),
            Err(ArtifactError::NotFound(_))
        ));

        fs::remove_file(&path).unwrap();
        fs::create_dir(dir.path().join("plugins/artifacts/stray")).unwrap();
        let problems = service.doctor(&|_| true).unwrap();
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].ends_with("document file p.md is missing"));
        assert!(problems[1].starts_with("stray: content directory"));
    }

    #[test]
    fn archiving_hides_from_the_session_query_and_is_reversible() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path());
        let artifact = document(&service, dir.path(), "a\n");
        let archived = service.set_archived(&artifact.id, "s1", true).unwrap();
        assert!(archived.is_archived());
        assert_eq!(archived.modified_at, artifact.modified_at);
        assert!(service.artifacts_for_session("s1").is_empty());
        assert_eq!(service.all().len(), 1);
        let back = service.set_archived(&artifact.id, "s1", false).unwrap();
        assert!(!back.is_archived());
        assert_eq!(service.artifacts_for_session("s1").len(), 1);
    }

    #[test]
    fn cleanups_are_recorded_closed_and_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path());
        let artifact = document(&service, dir.path(), "x");
        let root = dir.path().join("plugins/artifacts/.scratch/worktrees");
        let temp = root.join("artifact-x");
        fs::create_dir_all(&temp).unwrap();
        service
            .record_cleanup(
                &artifact.id,
                "view-1",
                Cleanup::RemoveWorktree(temp.clone()),
            )
            .unwrap();
        let gone = Cleanup::RemoveWorktree(root.join("gone"));
        service
            .record_cleanup(&artifact.id, "view-2", gone)
            .unwrap();
        assert_eq!(service.pending_cleanups(&artifact).unwrap().len(), 2);
        assert!(service.doctor(&|_| true).unwrap().is_empty());
        let stale = service.doctor(&|view| view == "view-1").unwrap();
        assert_eq!(stale.len(), 1);
        assert!(stale[0].contains("ended view view-2"), "{stale:?}");

        assert!(!service.close(&artifact.id, "nobody").unwrap());
        assert!(service.close(&artifact.id, "view-1").unwrap());
        assert!(!temp.exists());
        assert_eq!(service.repair(&|_| false).unwrap().len(), 1);
        assert!(service.pending_cleanups(&artifact).unwrap().is_empty());
        assert!(!service.files().file_path(&artifact, CLEANUP_FILE).exists());
        assert_eq!(service.close_all(&artifact.id).unwrap(), 0);
        let log = fs::read_to_string(service.log_path()).unwrap();
        assert_eq!(log.matches("artifact-close").count(), 2, "{log}");

        // A step naming anything but a temp worktree is refused and kept.
        let outside = dir.path().join("precious");
        fs::create_dir(&outside).unwrap();
        for path in [outside.clone(), root.join("../../../../precious")] {
            let step = Cleanup::RemoveWorktree(path);
            service
                .record_cleanup(&artifact.id, "view-3", step)
                .unwrap();
            assert!(matches!(
                service.close(&artifact.id, "view-3"),
                Err(ArtifactError::ForeignCleanup { .. })
            ));
            assert!(outside.exists());
            service.write_cleanups(&artifact, &BTreeMap::new()).unwrap();
        }
    }
}
