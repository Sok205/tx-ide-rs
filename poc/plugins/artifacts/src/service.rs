//! The artifact use cases, ported from main's `artifact_service.rs`.
//!
//! Composes `ArtifactStore` (records) + `ArtifactContent` (files) + the plugin's own
//! `.log.jsonl` under the data dir (the core's `log.jsonl` stays the core's). Every mutation logs
//! one line; reads (`content`, `diff`, `opened`) log a read-visibility line; queries do not log.
//!
//! Writes to one artifact are serialised by a flock on `.locks/<id>.lock`, held from loading the
//! record to saving it, so two `txc` processes modifying at once produce two contiguous revs.
//!
//! A diff's content is its patch (`kinds`): created from the worktree, re-snapshotted as a new
//! revision by `modify` and by `open`. The close step an `open` leaves behind (a temporary
//! worktree) is kept in `<id>/.cleanup.json`, keyed by the view that needs it, until `close`,
//! `doctor --repair`, or the next `open` of that artifact runs it.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Number, Value, json};

use crate::artifact::{Artifact, Kind};
use crate::difflib;
use crate::kinds::{self, Cleanup, DIFF_PATCH, KindError, Opened};
use crate::store::{
    ArtifactContent, ArtifactStore, ArtifactStoreError, ClaimRevError, Scan, atomic_write,
    is_valid_id,
};

pub const LOG_FILE: &str = ".log.jsonl";
pub const LOCK_DIR: &str = ".locks";
/// Under the data dir: a diff's temporary worktrees, one per open of a diff whose own is gone.
pub const WORKTREES_DIR: &str = ".worktrees";
/// Beside an artifact's working copy: its pending close steps, by view name.
pub const CLEANUP_FILE: &str = ".cleanup.json";

/// A precondition failed; the verb prints `txc artifact: <error>`, exit 1.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("artifact '{0}' not found")]
    NotFound(String),
    /// A crash orphan holds the slot the next rev needs.
    #[error(
        "artifact {id}: rev {rev} is already claimed by a file the history does not know (a crashed \
         write left an orphan; `txc artifact doctor --repair` clears it). Retry after the repair."
    )]
    Conflict { id: String, rev: u64 },
    #[error("artifact id prefix '{token}' is ambiguous ({count} matches); use more characters")]
    Ambiguous { token: String, count: usize },
    #[error("artifact {0} has only one revision; nothing to diff")]
    SingleRevision(String),
    #[error("artifact {id} has no revision {rev}")]
    NoRevision { id: String, rev: i64 },
    #[error("artifact {id} revision {rev} is not utf-8 text; cannot diff")]
    NotText { id: String, rev: i64 },
    #[error(
        "artifact {0} is a diff; its patch comes from the worktree, not a file (`txc artifact \
         modify <id>` re-snapshots it)"
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

    /// Where a diff's temporary worktrees go.
    pub fn worktrees_root(&self) -> PathBuf {
        self.dir.join(WORKTREES_DIR)
    }

    /// Every readable record, plus the skipped files' warnings.
    pub fn scan(&self) -> Scan {
        self.store.all()
    }

    /// Every readable record; skipped ones are dropped silently.
    pub fn all(&self) -> Vec<Artifact> {
        self.store.all().artifacts
    }

    fn append(&self, type_: &str, msg: &str, actor: &str) -> Result<()> {
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

    /// The per-artifact write lock, held until the guard drops.
    fn lock(&self, artifact_id: &str) -> Result<File> {
        if !is_valid_id(artifact_id) {
            return Err(ArtifactError::NotFound(artifact_id.to_owned()));
        }
        let dir = self.dir.join(LOCK_DIR);
        fs::create_dir_all(&dir).map_err(io_at(&dir))?;
        let path = dir.join(format!("{artifact_id}.lock"));
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(io_at(&path))?;
        file.lock().map_err(io_at(&path))?;
        Ok(file)
    }

    // ----- mutations -----------------------------------------------------------------------

    /// Register a new document: `revs/0<ext>` + `current<ext>`, the record, one log line.
    pub fn create(
        &self,
        session_id: &str,
        content: &[u8],
        title: Option<String>,
        filename: Option<String>,
        group: Option<String>,
    ) -> Result<Artifact> {
        let filename = filename.unwrap_or_else(|| "artifact".to_owned());
        let kind = Kind::Document {
            file: filename.clone(),
        };
        self.create_with_kind(session_id, kind, content, title, filename, group)
    }

    /// Register a new diff of `worktree` against `base` (a ref or commit; empty means the
    /// merge-base with the default branch), pinned to the commit id it resolves to now.
    pub fn create_diff(
        &self,
        session_id: &str,
        worktree: PathBuf,
        base: &str,
        title: Option<String>,
        group: Option<String>,
    ) -> Result<Artifact> {
        let base = kinds::resolve_base(&worktree, base)?;
        let kind = Kind::Diff { worktree, base };
        let patch = kinds::create_content(&kind, None)?;
        self.create_with_kind(
            session_id,
            kind,
            &patch,
            title,
            DIFF_PATCH.to_owned(),
            group,
        )
    }

    /// Register a new artifact of any kind with `content` as its rev 0 and working copy; the
    /// entry point for plugins owning a kind of their own (a tour's stops file).
    pub fn create_with_kind(
        &self,
        session_id: &str,
        kind: Kind,
        content: &[u8],
        title: Option<String>,
        filename: String,
        group: Option<String>,
    ) -> Result<Artifact> {
        let artifact = Artifact::new(
            uuid::Uuid::new_v4().to_string(),
            kind,
            title,
            filename,
            float(tx_core::events::now()),
            group,
            session_id.to_owned(),
        );
        self.claim(&artifact, 0, content)?;
        self.write_current(&artifact, content)?;
        self.save(&artifact)?;
        self.append(
            "artifact-create",
            &format!(
                "{} {} {}",
                artifact.id,
                artifact.kind.name(),
                artifact.filename
            ),
            session_id,
        )?;
        Ok(artifact)
    }

    /// Snapshot `content` as the next revision; identical to the last rev is a no-op. A diff's
    /// content comes from its worktree, never from a file.
    pub fn modify(
        &self,
        artifact_id: &str,
        session_id: &str,
        content: &[u8],
        changes: Option<String>,
    ) -> Result<Artifact> {
        let _lock = self.lock(artifact_id)?;
        let artifact = self.require(artifact_id)?;
        if artifact.kind.is_diff() {
            return Err(ArtifactError::NotEditable(artifact.id));
        }
        self.apply_modify(artifact, session_id, content, changes)
    }

    /// Snapshot the working copy as the next revision; for a diff, the worktree's patch again.
    pub fn snapshot_current(
        &self,
        artifact_id: &str,
        session_id: &str,
        changes: Option<String>,
    ) -> Result<Artifact> {
        let _lock = self.lock(artifact_id)?;
        let artifact = self.require(artifact_id)?;
        self.refresh(artifact, session_id, changes)
    }

    /// The kind's current content (a diff's patch, else the working copy) as the next revision.
    fn refresh(
        &self,
        artifact: Artifact,
        session_id: &str,
        changes: Option<String>,
    ) -> Result<Artifact> {
        let content = match kinds::refresh(&artifact.kind)? {
            Some(derived) => derived,
            None => {
                let path = self.files.current_path(&artifact);
                self.files.read_current(&artifact).map_err(io_at(&path))?
            }
        };
        self.apply_modify(artifact, session_id, &content, changes)
    }

    fn apply_modify(
        &self,
        mut artifact: Artifact,
        session_id: &str,
        content: &[u8],
        changes: Option<String>,
    ) -> Result<Artifact> {
        if content == self.last_snapshot(&artifact)?.as_slice() {
            return Ok(artifact);
        }
        let next_rev = artifact.next_rev();
        self.claim(&artifact, next_rev, content)?;
        self.write_current(&artifact, content)?;
        artifact.record_touch(
            session_id.to_owned(),
            float(tx_core::events::now()),
            changes,
        );
        self.save(&artifact)?;
        self.append(
            "artifact-modify",
            &format!("{} rev{next_rev}", artifact.id),
            session_id,
        )?;
        Ok(artifact)
    }

    /// Set (or with `None` clear) the explicit group override: metadata only, one log line.
    pub fn set_group(
        &self,
        artifact_id: &str,
        session_id: &str,
        group: Option<String>,
    ) -> Result<Artifact> {
        let _lock = self.lock(artifact_id)?;
        let mut artifact = self.require(artifact_id)?;
        artifact.group = group;
        self.save(&artifact)?;
        let shown = artifact.group.as_deref().unwrap_or("(cleared)");
        self.append(
            "artifact-group",
            &format!("{} {shown}", artifact.id),
            session_id,
        )?;
        Ok(artifact)
    }

    /// Archive (hide from prefix+a and default listings, content kept) or unarchive: metadata
    /// only, no touch, one log line.
    pub fn set_archived(
        &self,
        artifact_id: &str,
        session_id: &str,
        archived: bool,
    ) -> Result<Artifact> {
        let _lock = self.lock(artifact_id)?;
        let mut artifact = self.require(artifact_id)?;
        artifact.archived = archived;
        self.save(&artifact)?;
        let type_ = if archived {
            "artifact-archive"
        } else {
            "artifact-unarchive"
        };
        self.append(type_, &artifact.id, session_id)?;
        Ok(artifact)
    }

    /// Claim `revs/<rev>` exclusively; a taken slot is always a conflict, never overwritten.
    fn claim(&self, artifact: &Artifact, rev: u64, content: &[u8]) -> Result<()> {
        match self.files.claim_rev(artifact, rev, content) {
            Ok(()) => Ok(()),
            Err(ClaimRevError::Taken(rev)) => Err(ArtifactError::Conflict {
                id: artifact.id.clone(),
                rev,
            }),
            Err(ClaimRevError::Io(source)) => Err(ArtifactError::Io {
                path: self.files.rev_path(artifact, rev),
                source,
            }),
        }
    }

    fn write_current(&self, artifact: &Artifact, content: &[u8]) -> Result<()> {
        self.files
            .write_current(artifact, content)
            .map_err(io_at(&self.files.current_path(artifact)))
    }

    fn save(&self, artifact: &Artifact) -> Result<()> {
        self.store
            .save(artifact)
            .map_err(io_at(&self.store.record_path(&artifact.id)))
    }

    fn last_snapshot(&self, artifact: &Artifact) -> Result<Vec<u8>> {
        let latest = artifact.latest_rev();
        self.files
            .read_rev(artifact, latest)
            .map_err(io_at(&self.files.rev_path(artifact, latest)))
    }

    // ----- reads ---------------------------------------------------------------------------

    /// The working copy (default) or one snapshot; logs a read-visibility line.
    pub fn content(&self, artifact_id: &str, rev: Option<i64>) -> Result<Vec<u8>> {
        let artifact = self.require(artifact_id)?;
        let data = match rev {
            None => {
                let path = self.files.current_path(&artifact);
                self.files.read_current(&artifact).map_err(io_at(&path))?
            }
            Some(rev) => self.read_rev(&artifact, rev)?,
        };
        let which = rev.map_or_else(|| "current".to_owned(), |rev| format!("rev{rev}"));
        self.append(
            "artifact-read",
            &format!("{artifact_id} {which}"),
            &self.actor,
        )?;
        Ok(data)
    }

    /// Record that a session opened the artifact's view (read-visibility, not history).
    pub fn opened(&self, artifact_id: &str, session_id: &str) -> Result<()> {
        self.append(
            "artifact-open",
            &format!("{artifact_id} → {session_id}"),
            session_id,
        )
    }

    /// The artifact's directory; neither reads nor logs.
    pub fn dir(&self, artifact_id: &str) -> Result<PathBuf> {
        Ok(self.files.content_dir(&self.require(artifact_id)?))
    }

    /// Decide the view for an artifact: a diff whose worktree exists is re-snapshotted first (so
    /// the view and the record agree), then the kind says where nvim runs and what it opens. The
    /// caller starts the view, records the close step and logs the open.
    pub fn open(&self, artifact_id: &str, session_id: &str) -> Result<(Artifact, Opened)> {
        let _lock = self.lock(artifact_id)?;
        let mut artifact = self.require(artifact_id)?;
        if artifact.kind.worktree().is_some_and(Path::is_dir) {
            artifact = self.refresh(artifact, session_id, None)?;
        }
        let current = self.files.current_path(&artifact);
        let opened = kinds::open(&artifact, &current, &self.worktrees_root())?;
        Ok((artifact, opened))
    }

    // ----- close steps ---------------------------------------------------------------------

    fn cleanup_path(&self, artifact: &Artifact) -> PathBuf {
        self.files.file_path(artifact, CLEANUP_FILE)
    }

    /// The pending close steps of an artifact, by view name.
    pub fn pending_cleanups(&self, artifact: &Artifact) -> Result<BTreeMap<String, Cleanup>> {
        let path = self.cleanup_path(artifact);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
            Err(source) => return Err(io_at(&path)(source)),
        };
        let data: Value = serde_json::from_slice(&bytes).map_err(ArtifactStoreError::from)?;
        let entries = data.as_object().cloned().unwrap_or_default();
        Ok(entries
            .iter()
            .filter_map(|(view, value)| Some((view.clone(), Cleanup::from_value(value)?)))
            .collect())
    }

    fn write_cleanups(
        &self,
        artifact: &Artifact,
        cleanups: &BTreeMap<String, Cleanup>,
    ) -> Result<()> {
        let path = self.cleanup_path(artifact);
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
        let dir = self.files.content_dir(artifact);
        atomic_write(&dir, ".cleanup.", payload.as_bytes(), &path).map_err(io_at(&path))
    }

    /// Remember the close step an `open` left behind, keyed by the view that needs it, so the
    /// close can run from whichever `txc` process sees that view gone.
    pub fn record_cleanup(&self, artifact_id: &str, view: &str, cleanup: Cleanup) -> Result<()> {
        let _lock = self.lock(artifact_id)?;
        let artifact = self.require(artifact_id)?;
        let mut cleanups = self.pending_cleanups(&artifact)?;
        cleanups.insert(view.to_owned(), cleanup);
        self.write_cleanups(&artifact, &cleanups)
    }

    /// Run the close step recorded for `view`, if any; whether one ran. Idempotent: a step
    /// whose worktree is already gone still completes, and is forgotten.
    pub fn close(&self, artifact_id: &str, view: &str) -> Result<bool> {
        let _lock = self.lock(artifact_id)?;
        let artifact = self.require(artifact_id)?;
        let mut cleanups = self.pending_cleanups(&artifact)?;
        let Some(cleanup) = cleanups.remove(view) else {
            return Ok(false);
        };
        kinds::close(&cleanup)?;
        self.write_cleanups(&artifact, &cleanups)?;
        self.append(
            "artifact-close",
            &format!("{artifact_id} {view} {}", cleanup.describe()),
            &self.actor,
        )?;
        Ok(true)
    }

    /// Run every pending close step of an artifact now (`txc artifact close`); how many ran.
    pub fn close_all(&self, artifact_id: &str) -> Result<usize> {
        let artifact = self.require(artifact_id)?;
        let views: Vec<String> = self.pending_cleanups(&artifact)?.into_keys().collect();
        for view in &views {
            self.close(artifact_id, view)?;
        }
        Ok(views.len())
    }

    /// Run the close steps of an artifact's views that have ended; the lines describe what ran.
    pub fn close_stale(
        &self,
        artifact_id: &str,
        view_is_alive: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<String>> {
        let artifact = self.require(artifact_id)?;
        let mut lines = Vec::new();
        for (view, cleanup) in self.stale_cleanups(&artifact, view_is_alive)? {
            self.close(artifact_id, &view)?;
            lines.push(format!(
                "{}: ran the close step of ended view {view} ({})",
                artifact.id,
                cleanup.describe()
            ));
        }
        Ok(lines)
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

    /// `close_stale` across the store (`doctor --repair`).
    pub fn repair_cleanups(&self, view_is_alive: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        for artifact in self.all() {
            lines.extend(self.close_stale(&artifact.id, view_is_alive)?);
        }
        Ok(lines)
    }

    /// A unified diff between two revisions (default: the last two); both must be utf-8.
    pub fn diff(&self, artifact_id: &str, revs: Option<(i64, i64)>) -> Result<String> {
        let artifact = self.require(artifact_id)?;
        let (left_rev, right_rev) = match revs {
            Some(pair) => pair,
            None => {
                let latest = artifact.latest_rev();
                if latest == 0 {
                    return Err(ArtifactError::SingleRevision(artifact.id));
                }
                let latest = i64::try_from(latest).unwrap_or(i64::MAX);
                (latest - 1, latest)
            }
        };
        let left = self.decoded_rev(&artifact, left_rev)?;
        let right = self.decoded_rev(&artifact, right_rev)?;
        self.append(
            "artifact-diff",
            &format!("{artifact_id} rev{left_rev}..rev{right_rev}"),
            &self.actor,
        )?;
        Ok(difflib::unified_diff(
            &difflib::splitlines_keepends(&left),
            &difflib::splitlines_keepends(&right),
            &format!("rev{left_rev}"),
            &format!("rev{right_rev}"),
        ))
    }

    /// A rev by a signed number: a negative rev names a file that never exists.
    fn read_rev(&self, artifact: &Artifact, rev: i64) -> Result<Vec<u8>> {
        let not_found = || ArtifactError::NoRevision {
            id: artifact.id.clone(),
            rev,
        };
        let Ok(unsigned) = u64::try_from(rev) else {
            return Err(not_found());
        };
        match self.files.read_rev(artifact, unsigned) {
            Ok(data) => Ok(data),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(not_found()),
            Err(source) => Err(ArtifactError::Io {
                path: self.files.rev_path(artifact, unsigned),
                source,
            }),
        }
    }

    fn decoded_rev(&self, artifact: &Artifact, rev: i64) -> Result<String> {
        String::from_utf8(self.read_rev(artifact, rev)?).map_err(|_| ArtifactError::NotText {
            id: artifact.id.clone(),
            rev,
        })
    }

    /// Whether the working copy differs from the last snapshot.
    pub fn current_is_dirty(&self, artifact: &Artifact) -> Result<bool> {
        let current_path = self.files.current_path(artifact);
        let current = self
            .files
            .read_current(artifact)
            .map_err(io_at(&current_path))?;
        Ok(current != self.last_snapshot(artifact)?)
    }

    /// Every unarchived artifact `session_id` created or touched (a query, never denormalized).
    pub fn artifacts_for_session(&self, session_id: &str) -> Vec<Artifact> {
        let mut artifacts = self.all();
        artifacts.retain(|artifact| {
            !artifact.archived
                && artifact
                    .history()
                    .iter()
                    .any(|touch| touch.session_id == session_id)
        });
        artifacts
    }

    // ----- diagnostics ---------------------------------------------------------------------

    /// Invariant check across the store (read-only); an empty list means clean. `view_is_alive`
    /// says whether a view a close step waits on still runs.
    pub fn doctor(&self, view_is_alive: &dyn Fn(&str) -> bool) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        let artifacts = self.all();
        let record_ids: HashSet<&str> = artifacts.iter().map(|a| a.id.as_str()).collect();
        let logged = self.logged_mutations()?;
        for artifact in &artifacts {
            for rev in self.orphan_revs(artifact)? {
                problems.push(format!(
                    "{}: orphan rev file {rev} not in history (crash debris, ignored on read; \
                     blocks that slot until `doctor --repair`)",
                    artifact.id
                ));
            }
            let missing = self.files.missing_revs(artifact);
            for rev in &missing {
                problems.push(format!(
                    "{}: history rev {rev} has no file on disk",
                    artifact.id
                ));
            }
            problems.extend(self.current_health(artifact, &missing)?);
            for (view, cleanup) in self.stale_cleanups(artifact, view_is_alive)? {
                problems.push(format!(
                    "{}: close step for ended view {view} never ran ({}); `doctor --repair` \
                     runs it",
                    artifact.id,
                    cleanup.describe()
                ));
            }
            for touch in artifact.history() {
                if !logged.contains(&signature(&artifact.id, touch.rev)) {
                    problems.push(format!(
                        "{}: rev {} touch has no matching log line (suspected out-of-band \
                         write; a truncated log is also possible)",
                        artifact.id, touch.rev
                    ));
                }
            }
        }
        if self.dir.exists() {
            let mut entries: Vec<(String, bool)> = fs::read_dir(&self.dir)
                .map_err(io_at(&self.dir))?
                .filter_map(|entry| entry.ok())
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

    /// Remove every orphan rev file; returns the removed lines. Takes each artifact's write lock,
    /// so a repair never races a modify.
    pub fn repair_orphans(&self) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        for artifact in self.all() {
            let _lock = self.lock(&artifact.id)?;
            let Some(artifact) = self.store.load(&artifact.id)? else {
                continue;
            };
            for rev in self.orphan_revs(&artifact)? {
                self.files
                    .remove_rev(&artifact, rev)
                    .map_err(io_at(&self.files.rev_path(&artifact, rev)))?;
                removed.push(format!("{}: removed orphan rev file {rev}", artifact.id));
            }
        }
        Ok(removed)
    }

    fn orphan_revs(&self, artifact: &Artifact) -> Result<Vec<u64>> {
        self.files
            .orphan_revs(artifact)
            .map_err(io_at(&self.files.revs_dir(artifact)))
    }

    /// The mutation signatures present in the log; unparsable lines are skipped.
    fn logged_mutations(&self) -> Result<HashSet<Signature>> {
        let mut signatures = HashSet::new();
        let path = self.log_path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(signatures),
            Err(source) => return Err(io_at(&path)(source)),
        };
        for line in String::from_utf8_lossy(&bytes).lines() {
            if let Some(signature) = parse_signature(line) {
                signatures.insert(signature);
            }
        }
        Ok(signatures)
    }

    fn current_health(&self, artifact: &Artifact, missing_revs: &[u64]) -> Result<Vec<String>> {
        if missing_revs.contains(&artifact.latest_rev()) {
            return Ok(Vec::new());
        }
        if !self.files.current_path(artifact).exists() {
            return Ok(vec![format!(
                "{}: working copy current{} is missing",
                artifact.id,
                artifact.extension()
            )]);
        }
        if self.current_is_dirty(artifact)? {
            return Ok(vec![format!(
                "{}: working copy differs from last rev {} (dirty: un-snapshotted edits; close \
                 with `txc artifact modify`)",
                artifact.id,
                artifact.latest_rev()
            )]);
        }
        Ok(Vec::new())
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

/// The log signature a touch should carry: a create (rev 0) or a modify (rev n).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Signature {
    Create(String),
    Modify(String, u64),
}

fn signature(artifact_id: &str, rev: u64) -> Signature {
    if rev == 0 {
        Signature::Create(artifact_id.to_owned())
    } else {
        Signature::Modify(artifact_id.to_owned(), rev)
    }
}

/// One log line → its mutation signature, if any. A line that is not JSON is skipped.
fn parse_signature(line: &str) -> Option<Signature> {
    let entry: Value = serde_json::from_str(line).ok()?;
    let kind = entry.get("type").and_then(Value::as_str);
    let msg = entry.get("msg").and_then(Value::as_str).unwrap_or("");
    let fields: Vec<&str> = msg.split_whitespace().collect();
    match (kind, fields.as_slice()) {
        (Some("artifact-create"), [id, ..]) => Some(Signature::Create((*id).to_owned())),
        (Some("artifact-modify"), [id, rev, ..]) => Some(Signature::Modify(
            (*id).to_owned(),
            rev.strip_prefix("rev")?.parse().ok()?,
        )),
        _ => None,
    }
}

fn float(value: f64) -> Number {
    Number::from_f64(value).unwrap_or_else(|| Number::from(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(dir: &Path) -> ArtifactService {
        ArtifactService::new(dir.join("plugins/artifacts"), "user")
    }

    #[test]
    fn create_modify_diff_and_doctor_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path());
        let artifact = service
            .create("s1", b"a\nb\n", None, Some("p.md".into()), None)
            .unwrap();
        let same = service.modify(&artifact.id, "s1", b"a\nb\n", None).unwrap();
        assert_eq!(same.latest_rev(), 0);
        let next = service
            .modify(&artifact.id, "s2", b"a\nc\n", Some("x".into()))
            .unwrap();
        assert_eq!(next.latest_rev(), 1);
        assert_eq!(
            service.diff(&artifact.id, None).unwrap(),
            "--- rev0\n+++ rev1\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n"
        );
        assert!(matches!(
            service.diff(&artifact.id, Some((-1, 0))),
            Err(ArtifactError::NoRevision { rev: -1, .. })
        ));
        assert_eq!(service.doctor(&|_| true).unwrap(), Vec::<String>::new());
        assert!(matches!(
            service.modify(&artifact.id[..4], "s1", b"z", None),
            Err(ArtifactError::NotFound(_))
        ));
        assert_eq!(service.resolve_id(&artifact.id[..4]).unwrap(), artifact.id);
        assert!(matches!(
            service.resolve_id(""),
            Err(ArtifactError::NotFound(_))
        ));
        assert_eq!(
            service.artifacts_for_session("s2")[0].id,
            artifact.id,
            "a toucher finds it"
        );
        assert!(service.artifacts_for_session("s3").is_empty());
    }

    #[test]
    fn concurrent_modifies_in_threads_produce_contiguous_revs() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = service(dir.path())
            .create("s1", b"base\n", None, Some("p.md".into()), None)
            .unwrap();
        std::thread::scope(|scope| {
            for writer in 0..8 {
                let (root, id) = (dir.path(), artifact.id.as_str());
                scope.spawn(move || {
                    service(root)
                        .modify(id, "s1", format!("writer {writer}\n").as_bytes(), None)
                        .unwrap();
                });
            }
        });
        let service = service(dir.path());
        let after = service.require(&artifact.id).unwrap();
        let revs: Vec<u64> = after.history().iter().map(|touch| touch.rev).collect();
        assert_eq!(revs, (0..=8).collect::<Vec<u64>>());
        assert_eq!(service.doctor(&|_| true).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn signatures_follow_the_log_lines() {
        let line = |kind: &str, msg: &str| format!(r#"{{"type":"{kind}","msg":"{msg}"}}"#);
        assert_eq!(
            parse_signature(&line("artifact-create", "a plan.md")),
            Some(Signature::Create("a".into()))
        );
        assert_eq!(
            parse_signature(&line("artifact-modify", "a rev10")),
            Some(Signature::Modify("a".into(), 10))
        );
        assert_eq!(parse_signature(&line("artifact-modify", "x")), None);
        assert_eq!(parse_signature(&line("artifact-modify", "a revx")), None);
        assert_eq!(parse_signature("not json"), None);
    }

    #[test]
    fn cleanups_are_recorded_closed_and_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path());
        let artifact = service
            .create("s1", b"x", None, Some("p.md".into()), None)
            .unwrap();
        let temp = dir.path().join("temp-tree");
        fs::create_dir(&temp).unwrap();
        service
            .record_cleanup(
                &artifact.id,
                "review-diff",
                Cleanup::RemoveWorktree(temp.clone()),
            )
            .unwrap();
        service
            .record_cleanup(
                &artifact.id,
                "review-diff-2",
                Cleanup::RemoveWorktree(dir.path().join("gone")),
            )
            .unwrap();
        assert_eq!(service.pending_cleanups(&artifact).unwrap().len(), 2);
        let problems = service.doctor(&|view| view == "review-diff").unwrap();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("ended view review-diff-2"),
            "{problems:?}"
        );
        assert!(service.doctor(&|_| true).unwrap().is_empty());

        let ran = service
            .repair_cleanups(&|view| view == "review-diff")
            .unwrap();
        assert_eq!(ran.len(), 1);
        assert!(!service.close(&artifact.id, "review-diff-2").unwrap());
        assert!(temp.is_dir(), "the live view's worktree stays");
        assert_eq!(service.close_all(&artifact.id).unwrap(), 1);
        assert!(!temp.exists());
        assert!(service.pending_cleanups(&artifact).unwrap().is_empty());
        assert!(
            !dir.path()
                .join("plugins/artifacts")
                .join(&artifact.id)
                .join(CLEANUP_FILE)
                .exists()
        );
        assert_eq!(service.close_all(&artifact.id).unwrap(), 0);
    }

    #[test]
    fn archive_hides_from_a_sessions_list_and_a_diff_refuses_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path());
        let artifact = service
            .create("s1", b"x", None, Some("p.md".into()), None)
            .unwrap();
        assert_eq!(service.artifacts_for_session("s1").len(), 1);
        let archived = service.set_archived(&artifact.id, "s1", true).unwrap();
        assert!(archived.archived);
        assert_eq!(archived.latest_rev(), 0, "no touch");
        assert!(service.artifacts_for_session("s1").is_empty());
        assert_eq!(service.all().len(), 1, "content kept");
        assert!(
            !service
                .set_archived(&artifact.id, "s1", false)
                .unwrap()
                .archived
        );
        assert_eq!(
            service.dir(&artifact.id).unwrap(),
            dir.path().join("plugins/artifacts").join(&artifact.id)
        );

        let diff_id = {
            let mut diff = service
                .create("s1", b"", None, Some(DIFF_PATCH.into()), None)
                .unwrap();
            diff.kind = Kind::Diff {
                worktree: "/nonexistent".into(),
                base: "abc".into(),
            };
            service.store().save(&diff).unwrap();
            diff.id
        };
        assert!(matches!(
            service.modify(&diff_id, "s1", b"y", None),
            Err(ArtifactError::NotEditable(_))
        ));
        assert!(matches!(
            service.snapshot_current(&diff_id, "s1", None),
            Err(ArtifactError::Kind(KindError::WorktreeMissing(_)))
        ));
    }

    #[test]
    fn doctor_ignores_the_hidden_log_and_locks() {
        let dir = tempfile::tempdir().unwrap();
        let service = service(dir.path());
        let artifact = service
            .create("s1", b"x", None, Some("p.md".into()), None)
            .unwrap();
        service.modify(&artifact.id, "s1", b"y", None).unwrap();
        assert!(service.log_path().is_file());
        assert!(dir.path().join("plugins/artifacts/.locks").is_dir());
        assert_eq!(service.doctor(&|_| true).unwrap(), Vec::<String>::new());
    }
}
