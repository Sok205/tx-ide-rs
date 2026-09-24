//! Port of lib/tx/artifact_service.py — the artifact use-case core.
//!
//! Composes `ArtifactStore` (records) + `ArtifactContent` (files) + `EventLog`; every mutation
//! logs exactly one line, reads (`content`, `diff`, `opened`) log a read-visibility line, and the
//! cross-store queries (`artifacts_for_session`, `doctor`) do not log.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde_json::{Number, Value};

use crate::artifact::Artifact;
use crate::artifact_store::{
    ArtifactContent, ArtifactStore, ArtifactStoreError, ClaimRevError, Scan,
};
use crate::difflib;
use crate::events::{self, EventLog};
use crate::storage::Home;

/// An artifact use-case precondition failed; the CLI prints `tx artifact: <error>`, exit 1.
#[derive(Debug, thiserror::Error)]
pub enum ArtifactError {
    #[error("artifact '{0}' not found")]
    NotFound(String),
    /// A `modify` lost the linearity race (or a crash orphan holds the slot).
    #[error(
        "artifact {id}: rev {rev} is already claimed — the artifact moved on (a concurrent write), \
         or a crashed write left an orphan (`tx artifact doctor --repair` clears an orphan). \
         Re-read and retry."
    )]
    Conflict { id: String, rev: u64 },
    #[error("artifact id prefix '{token}' is ambiguous ({count} matches) — use more characters")]
    Ambiguous { token: String, count: usize },
    #[error("artifact {0} has only one revision — nothing to diff")]
    SingleRevision(String),
    #[error("artifact {id} has no revision {rev}")]
    NoRevision { id: String, rev: i64 },
    #[error("artifact {id} revision {rev} is not utf-8 text — cannot diff")]
    NotText { id: String, rev: i64 },
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

    /// Register a new artifact: `revs/0<ext>` + `current<ext>`, the record, one log line.
    pub fn create(
        &self,
        session_id: &str,
        content: &[u8],
        title: Option<String>,
        filename: Option<String>,
        group: Option<String>,
    ) -> Result<Artifact> {
        let artifact = Artifact::new(
            uuid::Uuid::new_v4().to_string(),
            title,
            filename.unwrap_or_else(|| "artifact".to_owned()),
            float(events::now()),
            group,
            session_id.to_owned(),
        );
        self.claim(&artifact, 0, content)?;
        self.write_current(&artifact, content)?;
        self.save(&artifact)?;
        self.append(
            "artifact-create",
            &format!("{} {}", artifact.id, artifact.filename),
            session_id,
        )?;
        Ok(artifact)
    }

    /// Snapshot `content` as the next revision; identical to the last rev is a no-op (E3).
    pub fn modify(
        &self,
        artifact_id: &str,
        session_id: &str,
        content: &[u8],
        changes: Option<String>,
    ) -> Result<Artifact> {
        let artifact = self.require(artifact_id)?;
        self.apply_modify(artifact, session_id, content, changes)
    }

    /// Snapshot the working copy (read without a read-log line) as the next revision.
    pub fn snapshot_current(
        &self,
        artifact_id: &str,
        session_id: &str,
        changes: Option<String>,
    ) -> Result<Artifact> {
        let artifact = self.require(artifact_id)?;
        let path = self.files.current_path(&artifact);
        let content = self.files.read_current(&artifact).map_err(io_at(&path))?;
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
        artifact.record_touch(session_id.to_owned(), float(events::now()), changes);
        self.save(&artifact)?;
        self.append(
            "artifact-modify",
            &format!("{} rev{next_rev}", artifact.id),
            session_id,
        )?;
        Ok(artifact)
    }

    /// Set (or with `None` clear) the explicit group override — metadata only, one log line.
    pub fn set_group(
        &self,
        artifact_id: &str,
        session_id: &str,
        group: Option<String>,
    ) -> Result<Artifact> {
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
            &self.env_actor,
        )?;
        Ok(data)
    }

    /// The `current<ext>` path `tx artifact open` hands nvim; neither reads nor logs.
    pub fn content_path(&self, artifact_id: &str) -> Result<PathBuf> {
        Ok(self.files.current_path(&self.require(artifact_id)?))
    }

    /// Record that a session opened the artifact's view (read-visibility, not history).
    pub fn opened(&self, artifact_id: &str, session_id: &str) -> Result<()> {
        self.append(
            "artifact-open",
            &format!("{artifact_id} → {session_id}"),
            session_id,
        )
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
                // History length bounds `latest`; it is far below i64::MAX.
                let latest = i64::try_from(latest).unwrap_or(i64::MAX);
                (latest - 1, latest)
            }
        };
        let left = self.decoded_rev(&artifact, left_rev)?;
        let right = self.decoded_rev(&artifact, right_rev)?;
        self.append(
            "artifact-diff",
            &format!("{artifact_id} rev{left_rev}..rev{right_rev}"),
            &self.env_actor,
        )?;
        Ok(difflib::unified_diff(
            &difflib::splitlines_keepends(&left),
            &difflib::splitlines_keepends(&right),
            &format!("rev{left_rev}"),
            &format!("rev{right_rev}"),
        ))
    }

    /// A rev by a Python int: a negative rev names a file that never exists.
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

    /// Whether the working copy differs from the last snapshot. An unreadable file is an error
    /// naming its path (the reference leaked a traceback — Q26).
    pub fn current_is_dirty(&self, artifact: &Artifact) -> Result<bool> {
        let current_path = self.files.current_path(artifact);
        let current = self
            .files
            .read_current(artifact)
            .map_err(io_at(&current_path))?;
        Ok(current != self.last_snapshot(artifact)?)
    }

    /// Every artifact `session_id` created or touched (a query, never denormalized).
    pub fn artifacts_for_session(&self, session_id: &str) -> Vec<Artifact> {
        self.scan(self.store.query(|artifact| {
            artifact
                .history()
                .iter()
                .any(|touch| touch.session_id == session_id)
        }))
    }

    // ----- diagnostics ---------------------------------------------------------------------

    /// Invariant check across the store (read-only); an empty list means clean.
    pub fn doctor(&self) -> Result<Vec<String>> {
        let mut problems = Vec::new();
        let artifacts = self.all();
        let record_ids: HashSet<&str> = artifacts.iter().map(|a| a.id.as_str()).collect();
        let logged = self.logged_mutations()?;
        for artifact in &artifacts {
            for rev in self.orphan_revs(artifact)? {
                problems.push(format!(
                    "{}: orphan rev file {rev} not in history (crash debris — ignored on read; \
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
            for touch in artifact.history() {
                if !logged.contains(&signature(&artifact.id, touch.rev)) {
                    problems.push(format!(
                        "{}: rev {} touch has no matching EventLog mutation line (suspected \
                         out-of-band write — a truncated log is also possible)",
                        artifact.id, touch.rev
                    ));
                }
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

    /// Remove every orphan rev file (run when quiescent); returns the removed lines.
    pub fn repair_orphans(&self) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        for artifact in self.all() {
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
        let path = self.log.path();
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(signatures),
            Err(source) => return Err(io_at(path)(source)),
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
                "{}: working copy differs from last rev {} (dirty — un-snapshotted edits; close \
                 with `tx artifact modify`)",
                artifact.id,
                artifact.latest_rev()
            )]);
        }
        Ok(Vec::new())
    }

    // ----- resolution ----------------------------------------------------------------------

    /// A full id or a unique id prefix → the full id. The empty token never resolves (the
    /// reference matched every record by the empty prefix — Q26 FIX leg T-ART-22).
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
    Modify(String, i64),
}

fn signature(artifact_id: &str, rev: u64) -> Signature {
    if rev == 0 {
        Signature::Create(artifact_id.to_owned())
    } else {
        Signature::Modify(artifact_id.to_owned(), i64::try_from(rev).unwrap_or(-1))
    }
}

/// One log line → its mutation signature, if any. Mirrors the reference's tolerance: a line that
/// is not JSON is skipped; `msg` split on whitespace; `rev<n>` parsed as a Python int.
fn parse_signature(line: &str) -> Option<Signature> {
    let entry: Value = serde_json::from_str(line).ok()?;
    let entry = entry.as_object()?;
    let kind = entry.get("type").and_then(Value::as_str);
    let msg = entry.get("msg").and_then(Value::as_str).unwrap_or("");
    let fields: Vec<&str> = msg.split_whitespace().collect();
    match (kind, fields.as_slice()) {
        (Some("artifact-create"), [id, ..]) => Some(Signature::Create((*id).to_owned())),
        (Some("artifact-modify"), [id, rev, ..]) => {
            let rev = rev.strip_prefix("rev")?;
            Some(Signature::Modify((*id).to_owned(), py_int(rev)?))
        }
        _ => None,
    }
}

/// `int(text)` for a whitespace-free token: optional sign, `_` between digits.
fn py_int(text: &str) -> Option<i64> {
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.chars().all(|c| c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    let value: i64 = digits.replace('_', "").parse().ok()?;
    Some(if negative { -value } else { value })
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
    fn create_modify_diff_and_doctor_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path());
        let service = service(&home);
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
        assert!(service.doctor().unwrap().is_empty());
        assert!(matches!(
            service.modify(&artifact.id[..4], "s1", b"z", None),
            Err(ArtifactError::NotFound(_))
        ));
        assert_eq!(service.resolve_id(&artifact.id[..4]).unwrap(), artifact.id);
        assert!(matches!(
            service.resolve_id(""),
            Err(ArtifactError::NotFound(_))
        ));
    }

    #[test]
    fn signatures_follow_the_reference_parse() {
        let line = |kind: &str, msg: &str| format!(r#"{{"type":"{kind}","msg":"{msg}"}}"#);
        assert_eq!(
            parse_signature(&line("artifact-create", "a plan.md")),
            Some(Signature::Create("a".into()))
        );
        assert_eq!(
            parse_signature(&line("artifact-modify", "a rev1_0")),
            Some(Signature::Modify("a".into(), 10))
        );
        assert_eq!(parse_signature(&line("artifact-modify", "x")), None);
        assert_eq!(parse_signature(&line("artifact-modify", "a revx")), None);
        assert_eq!(parse_signature("not json"), None);
    }
}
