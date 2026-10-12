//! Incremental transcript cache: rows per transcript, tail-parsed. JSONL transcripts are
//! append-only in the normal case; a shrink or in-place rewrite (size below the recorded offset)
//! forces a full reparse. `offset` tracks bytes consumed through the last complete newline, so a
//! partially flushed trailing line is left for the next poll rather than half-parsed.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::metadata::Metadata;
use super::rows::{Row, parse_entry};

struct Entry {
    size: u64,
    mtime: SystemTime,
    offset: u64,
    rows: Vec<Row>,
    metadata: Metadata,
}

#[derive(Default)]
pub struct TranscriptCache {
    entries: HashMap<PathBuf, Entry>,
}

impl TranscriptCache {
    /// Metadata collected with the most recent read of this transcript.
    pub fn metadata(&self, path: &Path) -> Option<&Metadata> {
        self.entries.get(path).map(|entry| &entry.metadata)
    }

    /// The rows of `path`, re-parsing only what was appended since the last call.
    pub fn rows(&mut self, path: &Path) -> &[Row] {
        let Ok(metadata) = std::fs::metadata(path) else {
            self.entries.remove(path);
            return &[];
        };
        let size = metadata.len();
        let mtime = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        let fresh = self
            .entries
            .get(path)
            .is_some_and(|entry| entry.size == size && entry.mtime == mtime);
        if fresh {
            return &self.entries[path].rows;
        }
        let (start, mut rows, mut metadata) = match self.entries.remove(path) {
            Some(entry) if size >= entry.offset => (entry.offset, entry.rows, entry.metadata),
            _ => (0, Vec::new(), Metadata::default()),
        };
        let mut chunk = Vec::new();
        let read = File::open(path).and_then(|mut file| {
            file.seek(SeekFrom::Start(start))?;
            file.read_to_end(&mut chunk)
        });
        if read.is_err() {
            return &[];
        }
        let consumed = chunk
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |newline| newline + 1);
        for line in String::from_utf8_lossy(&chunk[..consumed]).lines() {
            if let Ok(entry) = serde_json::from_str(line) {
                parse_entry(&entry, &mut rows);
                metadata.update(&entry);
            }
        }
        self.entries.insert(
            path.to_owned(),
            Entry {
                size,
                mtime,
                offset: start + consumed as u64,
                rows,
                metadata,
            },
        );
        &self.entries[path].rows
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[test]
    fn tail_parses_appended_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let line = |n: u32| {
            format!(
                "{{\"type\":\"user\",\"timestamp\":\"2026-10-11T08:0{n}:00Z\",\"message\":{{\"content\":\"m{n}\"}}}}\n"
            )
        };
        std::fs::write(&path, line(1)).unwrap();
        let mut cache = TranscriptCache::default();
        assert_eq!(cache.rows(&path).len(), 1);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(line(2).as_bytes()).unwrap();
        // a partial trailing line waits for its newline
        file.write_all(b"{\"type\":\"user\"").unwrap();
        file.set_modified(SystemTime::now()).unwrap();
        drop(file);
        assert_eq!(cache.rows(&path).len(), 2);
        std::fs::write(&path, line(3)).unwrap();
        assert_eq!(cache.rows(&path).len(), 1);
    }
}
