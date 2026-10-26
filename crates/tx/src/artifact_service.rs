//! The artifact use-case core.
//!
//! Composes `ArtifactStore` (records) + `ArtifactContent` (the directory) + `EventLog`; every
//! mutation logs exactly one line, a content mutation stamps `modified_at` (archive stamps
//! `archived_at` instead), `open` logs a read-visibility line,
//! and the cross-store queries (`artifacts_for_session`, `doctor`) do not log.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{Map, Number, Value};

use crate::artifact::{Artifact, Kind};
use crate::artifact_kinds::{self, Cleanup, KindError, Opened};
use crate::artifact_store::{
    ArtifactContent, ArtifactStore, ArtifactStoreError, Scan, atomic_write,
};
use crate::events::{self, EventLog};
use crate::pyjson::dumps_pretty;
use crate::storage::Home;

/// Pending close steps beside the record, keyed by the view session that needs them.
const CLEANUP_FILE: &str = ".cleanup.json";

/// An artifact use-case precondition failed; the CLI prints `tx artifact: <error>`, exit 1.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("artifact '{0}' not found")]
    NotFound(String),
    #[error("artifact id prefix '{token}' is ambiguous ({count} matches) — use more characters")]
    Ambiguous { token: String, count: usize },
    /// `modify <file>` on a diff: its content is derived from the worktree, never hand-edited.
    #[error(
        "artifact {0} is a diff — its patch is rewritten from the worktree; `modify` takes no file"
    )]
    NotEditable(String),
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

/// Where the tolerant store scan reports a skipped record (one stderr line in the CLI).
pub type SkipWarn = Rc<dyn Fn(&str)>;

pub struct ArtifactService {
    store: ArtifactStore,
    files: ArtifactContent,
    /// Where a diff's temporary worktree goes when its own is gone.
    worktrees_root: PathBuf,
    log: Rc<EventLog>,
    /// `$TX_SESSION_ID`: the actor of the read lines (`EventLog.append`'s default).
    env_actor: String,
    warn: SkipWarn,
}

impl ArtifactService {
    pub fn new(home: &Home, log: Rc<EventLog>, env_actor: &str, warn: SkipWarn) -> Self {
        Self {
            store: ArtifactStore::new(home),
            files: ArtifactContent::new(home),
            worktrees_root: home.worktrees_dir(),
            log,
            env_actor: env_actor.to_owned(),
            warn,
        }
    }

    pub fn store(&self) -> &ArtifactStore {
        &self.store
    }

    pub fn files(&self) -> &ArtifactContent {
        &self.files
    }

    /// `store.all()`: every readable record, warning about each skipped one.
    pub fn all(&self) -> Vec<Artifact> {
        self.scan(self.store.all())
    }

    fn scan(&self, scan: Scan) -> Vec<Artifact> {
        for skipped in &scan.skipped {
            (self.warn)(&skipped.warning());
        }
        scan.artifacts
    }

    fn append(&self, type_: &str, msg: &str, actor: &str) -> Result<()> {
        self.log
            .append(type_, msg, actor)
            .map_err(io_at(self.log.path()))
    }

    // ----- mutations -----------------------------------------------------------------------

    /// Register a new artifact: its directory with the kind's content, the record, one log line.
    /// `source` is the file a document or tour is created from; a diff's empty `base` is
    /// resolved to the merge-base with the default branch.
    pub fn create(
        &self,
        owner: &str,
        kind: Kind,
        title: String,
        source: Option<&Path>,
    ) -> Result<Artifact> {
        let kind = match kind {
            Kind::Diff { worktree, base } => {
                let base = artifact_kinds::resolve_base(&worktree, &base)?;
                Kind::Diff { worktree, base }
            }
            other => other,
        };
        let artifact = Artifact::new(
            uuid::Uuid::new_v4().to_string(),
            kind,
            title,
            owner.to_owned(),
            float(events::now()),
        );
        let dir = self.files.content_dir(&artifact);
        std::fs::create_dir_all(&dir).map_err(io_at(&dir))?;
        artifact_kinds::create_content(&artifact.kind, &dir, source)?;
        self.save(&artifact)?;
        self.append(
            "artifact-create",
            &format!(
                "{} {} {}",
                artifact.id,
                artifact.kind.name(),
                artifact.title
            ),
            owner,
        )?;
        Ok(artifact)
    }

    /// Stamp `modified_at` (the agent's "a meaningful revision happened"); with `file`, replace a
    /// document's or tour's file first. A diff's patch is rewritten instead.
    pub fn modify(&self, artifact_id: &str, actor: &str, file: Option<&Path>) -> Result<Artifact> {
        let mut artifact = self.require(artifact_id)?;
        let dir = self.files.content_dir(&artifact);
        match file {
            Some(_) if artifact.kind.is_diff() => {
                return Err(ArtifactError::NotEditable(artifact.id));
            }
            Some(source) => artifact_kinds::replace_content(&artifact.kind, &dir, source)?,
            None => artifact_kinds::refresh(&artifact.kind, &dir)?,
        }
        artifact.modified_at = float(events::now());
        self.save(&artifact)?;
        self.append("artifact-modify", &artifact.id, actor)?;
        Ok(artifact)
    }

    /// Archive (hide from pickers and default listings, content kept) or unarchive — metadata
    /// only, `modified_at` untouched, one log line.
    pub fn set_archived(&self, artifact_id: &str, actor: &str, archived: bool) -> Result<Artifact> {
        let mut artifact = self.require(artifact_id)?;
        artifact.archived_at = archived.then(|| float(events::now()));
        self.save(&artifact)?;
        let type_ = if archived {
            "artifact-archive"
        } else {
            "artifact-unarchive"
        };
        self.append(type_, &artifact.id, actor)?;
        Ok(artifact)
    }

    fn save(&self, artifact: &Artifact) -> Result<()> {
        self.store
            .save(artifact)
            .map_err(io_at(&self.store.record_path(&artifact.id)))
    }

    // ----- reads ---------------------------------------------------------------------------

    /// The artifact's directory; neither reads nor logs.
    pub fn dir(&self, artifact_id: &str) -> Result<PathBuf> {
        Ok(self.files.content_dir(&self.require(artifact_id)?))
    }

    /// Decide the view for an artifact: refresh derived content (a diff's patch, which also
    /// stamps `modified_at`), then the kind's spawn spec named `name`. The caller spawns it and
    /// binds the session; one `artifact-open` line is logged as `actor`.
    pub fn open(
        &self,
        artifact_id: &str,
        actor: &str,
        name: String,
        tags: Vec<String>,
    ) -> Result<Opened> {
        let mut artifact = self.require(artifact_id)?;
        let dir = self.files.content_dir(&artifact);
        if artifact.kind.is_diff() && artifact.kind.worktree().is_some_and(Path::is_dir) {
            artifact_kinds::refresh(&artifact.kind, &dir)?;
            artifact.modified_at = float(events::now());
            self.save(&artifact)?;
        }
        let opened = artifact_kinds::open(&artifact, &dir, name, tags, &self.worktrees_root)?;
        self.append("artifact-open", &format!("{artifact_id} → {actor}"), actor)?;
        Ok(opened)
    }

    // ----- close steps ---------------------------------------------------------------------

    fn cleanup_path(&self, artifact: &Artifact) -> PathBuf {
        self.files.file_path(artifact, CLEANUP_FILE)
    }

    /// The pending close steps of an artifact, by view session id.
    pub fn pending_cleanups(&self, artifact: &Artifact) -> Result<BTreeMap<String, Cleanup>> {
        let path = self.cleanup_path(artifact);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(source) => return Err(io_at(&path)(source)),
        };
        let data: Value = serde_json::from_slice(&bytes).map_err(ArtifactStoreError::from)?;
        let entries = data.as_object().cloned().unwrap_or_default();
        Ok(entries
            .iter()
            .filter_map(|(session, value)| Some((session.clone(), Cleanup::from_value(value)?)))
            .collect())
    }

    fn write_cleanups(
        &self,
        artifact: &Artifact,
        cleanups: &BTreeMap<String, Cleanup>,
    ) -> Result<()> {
        let path = self.cleanup_path(artifact);
        if cleanups.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io_at(&path)(error)),
                _ => Ok(()),
            };
        }
        let map: Map<String, Value> = cleanups
            .iter()
            .map(|(session, cleanup)| (session.clone(), cleanup.to_value()))
            .collect();
        let payload = dumps_pretty(&Value::Object(map));
        let dir = self.files.content_dir(artifact);
        atomic_write(&dir, ".cleanup.", payload.as_bytes(), &path).map_err(io_at(&path))
    }

    /// Remember the close step an `open` left behind, keyed by the view that needs it, so the
    /// close can run from whichever `tx` process sees that view end.
    pub fn record_cleanup(
        &self,
        artifact_id: &str,
        view_session_id: &str,
        cleanup: Cleanup,
    ) -> Result<()> {
        let artifact = self.require(artifact_id)?;
        let mut cleanups = self.pending_cleanups(&artifact)?;
        cleanups.insert(view_session_id.to_owned(), cleanup);
        self.write_cleanups(&artifact, &cleanups)
    }

    /// Run the close step recorded for `view_session_id`, if any; whether one ran. Logs one
    /// `artifact-close` line per step run.
    pub fn close(&self, artifact_id: &str, view_session_id: &str) -> Result<bool> {
        let artifact = self.require(artifact_id)?;
        let mut cleanups = self.pending_cleanups(&artifact)?;
        let Some(cleanup) = cleanups.remove(view_session_id) else {
            return Ok(false);
        };
        artifact_kinds::close(&cleanup)?;
        self.write_cleanups(&artifact, &cleanups)?;
        self.append(
            "artifact-close",
            &format!("{artifact_id} {view_session_id} {}", cleanup.describe()),
            &self.env_actor,
        )?;
        Ok(true)
    }

    /// Run every pending close step of an artifact now (`tx artifact close`); how many ran.
    pub fn close_all(&self, artifact_id: &str) -> Result<usize> {
        let artifact = self.require(artifact_id)?;
        let views: Vec<String> = self.pending_cleanups(&artifact)?.into_keys().collect();
        for view in &views {
            self.close(artifact_id, view)?;
        }
        Ok(views.len())
    }

    /// Every unarchived artifact `session_id` owns.
    pub fn artifacts_for_session(&self, session_id: &str) -> Vec<Artifact> {
        self.scan(
            self.store
                .query(|artifact| !artifact.is_archived() && artifact.owner == session_id),
        )
    }

    // ----- diagnostics ---------------------------------------------------------------------

    /// Invariant check across the store (read-only); an empty list means clean. Every record has
    /// its directory and its kind's file; every directory has a record; no close step waits on
    /// a view that has already ended (`view_is_alive` is the session store's answer).
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
            let file = artifact_kinds::content_file(&artifact.kind);
            if !dir.join(file).is_file() {
                problems.push(format!(
                    "{}: {} file {file} is missing",
                    artifact.id,
                    artifact.kind.name()
                ));
            }
            for (view, cleanup) in self.stale_cleanups(artifact, view_is_alive)? {
                problems.push(format!(
                    "{}: close step for ended view {view} never ran ({}) — `doctor --repair` \
                     runs it",
                    artifact.id,
                    cleanup.describe()
                ));
            }
        }
        let directory = self.store.directory();
        if directory.exists() {
            let mut entries: Vec<(String, bool)> = std::fs::read_dir(directory)
                .map_err(io_at(directory))?
                .filter_map(|entry| entry.ok())
                .map(|entry| {
                    let is_dir = entry.path().is_dir();
                    (entry.file_name().to_string_lossy().into_owned(), is_dir)
                })
                .collect();
            entries.sort();
            for (name, is_dir) in entries {
                if is_dir && !record_ids.contains(name.as_str()) {
                    problems.push(format!(
                        "{name}: content directory under artifacts/ has no record (out-of-band \
                         creation — the record is authoritative)"
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
        Ok(self
            .pending_cleanups(artifact)?
            .into_iter()
            .filter(|(view, _)| !view_is_alive(view))
            .collect())
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

    /// The actor of read lines when the caller has no session of its own.
    pub fn env_actor(&self) -> &str {
        &self.env_actor
    }
}

fn float(value: f64) -> Number {
    // `time.time()` is always finite, so the fallback is unreachable in practice.
    Number::from_f64(value).unwrap_or_else(|| Number::from(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(home: &Home) -> ArtifactService {
        ArtifactService::new(
            home,
            Rc::new(EventLog::new(home.log_path())),
            "",
            Rc::new(|_: &str| {}),
        )
    }

    #[test]
    fn create_modify_and_doctor_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let service = service(&home);
        let source = dir.path().join("p.md");
        std::fs::write(&source, "a\nb\n").unwrap();
        let artifact = service
            .create(
                "s1",
                Kind::Document {
                    file: "p.md".into(),
                },
                "Plan".into(),
                Some(&source),
            )
            .unwrap();
        assert_eq!(artifact.owner, "s1");
        assert_eq!(artifact.created_at, artifact.modified_at);
        let path = service.dir(&artifact.id).unwrap().join("p.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nb\n");
        let opened = service
            .open(&artifact.id, "s1", "Plan".into(), vec!["t".into()])
            .unwrap();
        assert!(opened.spec.cmd.ends_with("/p.md"), "{}", opened.spec.cmd);
        assert!(service.doctor(&|_| true).unwrap().is_empty());

        std::fs::write(&source, "a\nc\n").unwrap();
        let modified = service.modify(&artifact.id, "s2", Some(&source)).unwrap();
        assert!(modified.modified_at.as_f64() >= artifact.modified_at.as_f64());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a\nc\n");
        assert_eq!(service.artifacts_for_session("s1").len(), 1);
        assert!(service.artifacts_for_session("s2").is_empty());

        assert!(matches!(
            service.modify(&artifact.id[..4], "s1", None),
            Err(ArtifactError::NotFound(_))
        ));
        assert_eq!(service.resolve_id(&artifact.id[..4]).unwrap(), artifact.id);
        assert!(matches!(
            service.resolve_id(""),
            Err(ArtifactError::NotFound(_))
        ));

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(home.artifacts_dir().join("stray")).unwrap();
        let problems = service.doctor(&|_| true).unwrap();
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].ends_with("document file p.md is missing"));
        assert!(problems[1].starts_with("stray: content directory"));
    }

    #[test]
    fn archiving_hides_from_the_session_query_and_is_reversible() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let service = service(&home);
        let source = dir.path().join("p.md");
        std::fs::write(&source, "a\n").unwrap();
        let artifact = service
            .create(
                "s1",
                Kind::Document {
                    file: "p.md".into(),
                },
                "Plan".into(),
                Some(&source),
            )
            .unwrap();
        assert_eq!(service.artifacts_for_session("s1").len(), 1);
        let archived = service.set_archived(&artifact.id, "s1", true).unwrap();
        assert!(archived.is_archived());
        assert_eq!(archived.modified_at, artifact.modified_at);
        assert!(service.artifacts_for_session("s1").is_empty());
        assert!(service.require(&artifact.id).unwrap().is_archived());
        assert_eq!(service.all().len(), 1);
        let back = service.set_archived(&artifact.id, "s1", false).unwrap();
        assert!(!back.is_archived());
        assert_eq!(service.artifacts_for_session("s1").len(), 1);
        assert!(service.doctor(&|_| true).unwrap().is_empty());
    }

    #[test]
    fn cleanups_are_recorded_closed_and_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let service = service(&home);
        let source = dir.path().join("p.md");
        std::fs::write(&source, "x").unwrap();
        let artifact = service
            .create(
                "s1",
                Kind::Document {
                    file: "p.md".into(),
                },
                "Plan".into(),
                Some(&source),
            )
            .unwrap();
        let temp = dir.path().join("temp-worktree");
        std::fs::create_dir(&temp).unwrap();
        service
            .record_cleanup(
                &artifact.id,
                "view-1",
                Cleanup::RemoveWorktree(temp.clone()),
            )
            .unwrap();
        service
            .record_cleanup(
                &artifact.id,
                "view-2",
                Cleanup::RemoveWorktree(dir.path().join("gone")),
            )
            .unwrap();
        assert_eq!(service.pending_cleanups(&artifact).unwrap().len(), 2);
        assert!(service.doctor(&|_| true).unwrap().is_empty());
        let stale = service.doctor(&|view| view == "view-1").unwrap();
        assert_eq!(stale.len(), 1);
        assert!(stale[0].contains("ended view view-2"), "{stale:?}");

        assert!(!service.close(&artifact.id, "nobody").unwrap());
        assert!(service.close(&artifact.id, "view-1").unwrap());
        assert!(!temp.exists());
        let repaired = service.repair(&|_| false).unwrap();
        assert_eq!(repaired.len(), 1);
        assert!(service.pending_cleanups(&artifact).unwrap().is_empty());
        assert!(
            !service
                .dir(&artifact.id)
                .unwrap()
                .join(CLEANUP_FILE)
                .exists()
        );
        assert_eq!(service.close_all(&artifact.id).unwrap(), 0);
        let log = std::fs::read_to_string(home.log_path()).unwrap();
        assert_eq!(log.matches("artifact-close").count(), 2, "{log}");
    }
}
