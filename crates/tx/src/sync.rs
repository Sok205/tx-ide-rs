//! Port of lib/tx/sync.py — manual `tx sync` over the `Storage` copy boundary (§14, stage S7).
//!
//! Local (`$TX_IDE_HOME`) is always the working set; sync mirrors the reproducible corpus
//! (`sessions/`, `history/`, `log.jsonl`, `config.json`) to/from a remote `Storage`. Records are
//! last-writer-wins by `ended_at` / `last_activity` / `created_at`; every other key is decided by
//! the direction. Sync is additive: it never deletes.
//!
//! Remote selection (`remote_from_config` / `remote_from_spec` / `remote_label`) lives in
//! `storage.rs` (`Config::sync_remote`, `RemoteSpec`, `Storage::label`).

use serde_json::Value;

use crate::storage::{Storage, StorageError};

pub const CORPUS_PREFIXES: [&str; 2] = ["sessions/", "history/"];
pub const CORPUS_FILES: [&str; 2] = ["log.jsonl", "config.json"];

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A record whose bytes are not a JSON object (`json.loads` / `.get` failing in the reference).
    #[error("{key}: not a JSON record: {reason}")]
    BadRecord { key: String, reason: String },
}

pub fn is_corpus_key(key: &str) -> bool {
    CORPUS_PREFIXES.iter().any(|prefix| key.starts_with(prefix)) || CORPUS_FILES.contains(&key)
}

/// A uuid-sharded record (`sessions/<uuid>.json`) — the only last-writer-wins key.
pub fn is_record_key(key: &str) -> bool {
    key.starts_with("sessions/") && key.ends_with(".json")
}

/// The corpus keys present in `storage`, sorted.
pub fn corpus_keys(storage: &dyn Storage) -> Result<Vec<String>, StorageError> {
    let mut keys = storage.list("")?;
    keys.retain(|key| is_corpus_key(key));
    Ok(keys)
}

/// The last-writer-wins clock: `ended_at or last_activity or created_at or 0.0` (truthiness).
fn record_clock(key: &str, data: &[u8]) -> Result<f64, SyncError> {
    let bad = |reason: String| SyncError::BadRecord {
        key: key.to_owned(),
        reason,
    };
    let record: Value = serde_json::from_slice(data).map_err(|error| bad(error.to_string()))?;
    let Value::Object(record) = record else {
        return Err(bad("not an object".into()));
    };
    for field in ["ended_at", "last_activity", "created_at"] {
        match record.get(field) {
            None | Some(Value::Null) | Some(Value::Bool(false)) => {}
            Some(Value::Bool(true)) => return Ok(1.0),
            Some(Value::Number(n)) => match n.as_f64() {
                Some(value) if value != 0.0 => return Ok(value),
                _ => {}
            },
            Some(other) => return Err(bad(format!("'{field}' is not a number: {other}"))),
        }
    }
    Ok(0.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verdict {
    Add,
    Update,
    Unchanged,
    /// The destination holds a strictly newer record.
    Kept,
}

fn verdict(key: &str, data: &[u8], destination: &dyn Storage) -> Result<Verdict, SyncError> {
    if !destination.exists(key)? {
        return Ok(Verdict::Add);
    }
    let current = destination.get(key)?;
    if current == data {
        return Ok(Verdict::Unchanged);
    }
    if is_record_key(key) && record_clock(key, data)? < record_clock(key, &current)? {
        return Ok(Verdict::Kept);
    }
    Ok(Verdict::Update)
}

/// The outcome of one `push` / `pull` direction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncResult {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    /// Destination record was newer — last-writer-wins kept it.
    pub kept: Vec<String>,
}

fn copy_direction(
    source: &dyn Storage,
    destination: &dyn Storage,
) -> Result<SyncResult, SyncError> {
    let mut result = SyncResult::default();
    for key in corpus_keys(source)? {
        let data = source.get(&key)?;
        let bucket = match verdict(&key, &data, destination)? {
            Verdict::Add => {
                destination.put(&key, &data)?;
                &mut result.added
            }
            Verdict::Update => {
                destination.put(&key, &data)?;
                &mut result.updated
            }
            Verdict::Unchanged => &mut result.unchanged,
            Verdict::Kept => &mut result.kept,
        };
        bucket.push(key);
    }
    Ok(result)
}

/// How many corpus keys a copy from `source` to `destination` would add or update (no writes).
pub fn sync_diff(source: &dyn Storage, destination: &dyn Storage) -> Result<usize, SyncError> {
    let mut count = 0;
    for key in corpus_keys(source)? {
        let data = source.get(&key)?;
        if matches!(
            verdict(&key, &data, destination)?,
            Verdict::Add | Verdict::Update
        ) {
            count += 1;
        }
    }
    Ok(count)
}

/// Local → remote (a newer remote record is kept).
pub fn sync_push(local: &dyn Storage, remote: &dyn Storage) -> Result<SyncResult, SyncError> {
    copy_direction(local, remote)
}

/// Remote → local (a newer local record is kept).
pub fn sync_pull(local: &dyn Storage, remote: &dyn Storage) -> Result<SyncResult, SyncError> {
    copy_direction(remote, local)
}

/// A snapshot of one storage's corpus, for `tx sync status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CorpusStatus {
    pub records: usize,
    pub history_files: usize,
    pub has_log: bool,
    pub has_config: bool,
}

impl CorpusStatus {
    pub fn total(&self) -> usize {
        self.records + self.history_files + usize::from(self.has_log) + usize::from(self.has_config)
    }
}

pub fn corpus_status(storage: &dyn Storage) -> Result<CorpusStatus, StorageError> {
    let keys = corpus_keys(storage)?;
    Ok(CorpusStatus {
        records: keys.iter().filter(|key| is_record_key(key)).count(),
        history_files: keys
            .iter()
            .filter(|key| key.starts_with("history/"))
            .count(),
        has_log: keys.iter().any(|key| key == "log.jsonl"),
        has_config: keys.iter().any(|key| key == "config.json"),
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::storage::{LocalStorage, S3Storage};

    fn write(root: &Path, key: &str, data: &str) {
        LocalStorage::new(root).put(key, data.as_bytes()).unwrap();
    }

    fn read(root: &Path, key: &str) -> String {
        String::from_utf8(LocalStorage::new(root).get(key).unwrap()).unwrap()
    }

    fn counts(result: &SyncResult) -> [usize; 4] {
        [
            result.added.len(),
            result.updated.len(),
            result.unchanged.len(),
            result.kept.len(),
        ]
    }

    #[test]
    fn corpus_filter_and_status() {
        let home = tempfile::tempdir().unwrap();
        let arch = tempfile::tempdir().unwrap();
        for (key, data) in [
            ("sessions/a.json", r#"{"created_at": 1}"#),
            ("sessions/.a.tmp", "tmp"),
            ("history/t1/c1/transcript.jsonl", "{}\n"),
            ("log.jsonl", "{}\n"),
            ("config.json", "{}"),
            ("artifacts/x.json", "{}"),
            ("user-agents/DEV.md", "# Dev\n"),
            ("worktrees/w/file", "w"),
            ("launch/x.sh", "#!/bin/sh\n"),
        ] {
            write(home.path(), key, data);
        }
        let local = LocalStorage::new(home.path());
        let remote = LocalStorage::new(arch.path());
        let status = corpus_status(&local).unwrap();
        assert_eq!(
            status,
            CorpusStatus {
                records: 1,
                history_files: 1,
                has_log: true,
                has_config: true
            }
        );
        assert_eq!(status.total(), 4);
        assert_eq!(sync_diff(&local, &remote).unwrap(), 5);
        assert_eq!(sync_diff(&remote, &local).unwrap(), 0);
        let result = sync_push(&local, &remote).unwrap();
        assert_eq!(counts(&result), [5, 0, 0, 0]);
        assert_eq!(
            corpus_keys(&remote).unwrap(),
            [
                "config.json",
                "history/t1/c1/transcript.jsonl",
                "log.jsonl",
                "sessions/.a.tmp",
                "sessions/a.json"
            ]
        );
    }

    #[test]
    fn record_clock_precedence() {
        let home = tempfile::tempdir().unwrap();
        let arch = tempfile::tempdir().unwrap();
        let pairs = [
            (
                "k1",
                r#"{"ended_at": 5, "last_activity": 9, "created_at": 1, "pad": "l"}"#,
                r#"{"ended_at": null, "last_activity": 6, "created_at": 1, "pad": "r"}"#,
            ),
            (
                "k2",
                r#"{"ended_at": null, "last_activity": 9, "created_at": 1, "pad": "l"}"#,
                r#"{"ended_at": null, "last_activity": null, "created_at": 10, "pad": "r"}"#,
            ),
            (
                "k3",
                r#"{"created_at": 1, "pad": "l"}"#,
                r#"{"created_at": 2, "pad": "r"}"#,
            ),
            ("k4", r#"{"pad": "l"}"#, r#"{"created_at": 1, "pad": "r"}"#),
            (
                "k5",
                r#"{"ended_at": 0, "last_activity": 9, "pad": "l"}"#,
                r#"{"ended_at": null, "last_activity": 8, "pad": "r"}"#,
            ),
        ];
        for (key, local, remote) in pairs {
            write(home.path(), &format!("sessions/{key}.json"), local);
            write(arch.path(), &format!("sessions/{key}.json"), remote);
        }
        let result = sync_push(
            &LocalStorage::new(home.path()),
            &LocalStorage::new(arch.path()),
        )
        .unwrap();
        assert_eq!(counts(&result), [0, 1, 0, 4]);
        for (key, local, remote) in pairs {
            let want = if key == "k5" { local } else { remote };
            assert_eq!(
                read(arch.path(), &format!("sessions/{key}.json")),
                want,
                "{key}"
            );
        }
    }

    #[test]
    fn push_then_pull_buckets_and_dry_count() {
        let home = tempfile::tempdir().unwrap();
        let arch = tempfile::tempdir().unwrap();
        write(
            home.path(),
            "sessions/r1.json",
            r#"{"created_at": 5, "pad": "same"}"#,
        );
        write(
            home.path(),
            "sessions/r2.json",
            r#"{"created_at": 5, "pad": "local"}"#,
        );
        write(home.path(), "history/h1", "local-h1\n");
        write(home.path(), "log.jsonl", "{\"ts\":1}\n");
        write(
            arch.path(),
            "sessions/r1.json",
            r#"{"created_at": 5, "pad": "same"}"#,
        );
        write(
            arch.path(),
            "sessions/r2.json",
            r#"{"created_at": 7, "pad": "remote"}"#,
        );
        write(arch.path(), "history/h1", "remote-h1\n");
        write(
            arch.path(),
            "sessions/r3.json",
            r#"{"created_at": 1, "pad": "extra"}"#,
        );
        let local = LocalStorage::new(home.path());
        let remote = LocalStorage::new(arch.path());
        assert_eq!(sync_diff(&local, &remote).unwrap(), 2);
        assert_eq!(sync_diff(&remote, &local).unwrap(), 3);
        let push = sync_push(&local, &remote).unwrap();
        assert_eq!(counts(&push), [1, 1, 1, 1]);
        assert_eq!(push.kept, ["sessions/r2.json"]);
        assert_eq!(read(arch.path(), "history/h1"), "local-h1\n");
        let pull = sync_pull(&local, &remote).unwrap();
        assert_eq!(counts(&pull), [1, 1, 3, 0]);
        assert_eq!(
            read(home.path(), "sessions/r2.json"),
            r#"{"created_at": 7, "pad": "remote"}"#
        );
    }

    #[test]
    fn verdict_rules_equal_clock_source_wins() {
        let home = tempfile::tempdir().unwrap();
        let arch = tempfile::tempdir().unwrap();
        for (key, local, remote) in [
            (
                "r-same",
                r#"{"created_at": 10, "pad": "same"}"#,
                r#"{"created_at": 10, "pad": "same"}"#,
            ),
            (
                "r-older",
                r#"{"created_at": 9, "pad": "l"}"#,
                r#"{"created_at": 10, "pad": "r"}"#,
            ),
            (
                "r-equal",
                r#"{"created_at": 10, "pad": "l"}"#,
                r#"{"created_at": 10, "pad": "r"}"#,
            ),
            (
                "r-newer",
                r#"{"created_at": 11, "pad": "l"}"#,
                r#"{"created_at": 10, "pad": "r"}"#,
            ),
        ] {
            write(home.path(), &format!("sessions/{key}.json"), local);
            write(arch.path(), &format!("sessions/{key}.json"), remote);
        }
        write(home.path(), "history/h", "local-h\n");
        write(arch.path(), "history/h", "remote-h\n");
        write(home.path(), "log.jsonl", "{}\n");
        let result = sync_push(
            &LocalStorage::new(home.path()),
            &LocalStorage::new(arch.path()),
        )
        .unwrap();
        assert_eq!(counts(&result), [1, 3, 1, 1]);
        assert_eq!(result.kept, ["sessions/r-older.json"]);
        assert_eq!(
            read(arch.path(), "sessions/r-equal.json"),
            r#"{"created_at": 10, "pad": "l"}"#
        );
        assert_eq!(read(arch.path(), "history/h"), "local-h\n");
    }

    #[test]
    fn s3_stub_surfaces_deferred_only_when_reached() {
        let home = tempfile::tempdir().unwrap();
        let local = LocalStorage::new(home.path());
        let s3 = S3Storage::new("mybucket", "pre");
        assert_eq!(counts(&sync_push(&local, &s3).unwrap()), [0, 0, 0, 0]);
        write(home.path(), "sessions/s1.json", r#"{"created_at": 1}"#);
        assert!(matches!(
            sync_push(&local, &s3),
            Err(SyncError::Storage(StorageError::Deferred))
        ));
        assert!(matches!(
            sync_diff(&s3, &local),
            Err(SyncError::Storage(StorageError::Deferred))
        ));
    }

    #[test]
    fn malformed_record_is_an_error_not_a_panic() {
        let home = tempfile::tempdir().unwrap();
        let arch = tempfile::tempdir().unwrap();
        write(home.path(), "sessions/x.json", "not json");
        write(arch.path(), "sessions/x.json", r#"{"created_at": 1}"#);
        let result = sync_push(
            &LocalStorage::new(home.path()),
            &LocalStorage::new(arch.path()),
        );
        assert!(matches!(result, Err(SyncError::BadRecord { .. })));
    }
}
