//! Mirrors a chat's bundle by copy into `<history_dir>/<session>/<chat>/`: the transcript by
//! byte offset (append-only, with a trailing-window prefix check), the engine's sidecar dirs
//! copy-if-absent. One `flock` per (session, chat) coalesces concurrent mirrors: the hook path
//! skips when one is in flight, chat ops block for a complete mirror.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use tx_core::{Launch, SessionId, TxCore};

use crate::engines::{BUNDLE_TRANSCRIPT_NAME, bundle_sidecars};
use crate::ext::{chat_ext, history_ext};
use crate::{ChatError, HISTORY_ID, ensure_dir, write_atomic};

/// The per-(session, chat) coalescing lock, hidden inside the bundle it guards.
pub const INGEST_LOCK_NAME: &str = ".ingest.lock";

/// Trailing bytes of the existing copy verified against the source before an offset append.
const PREFIX_CHECK_BYTES: u64 = 65536;

pub const HISTORY_ENTRIES: crate::install::Entries = crate::install::Entries {
    sidecar: ".tx-history-managed.json",
    events: &[("Stop", ""), ("StopFailure", ""), ("SessionEnd", "")],
    verb: "_history-ingest",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngestMode {
    /// The hot Stop path: skip the chat when a mirror is in flight.
    Coalesce,
    /// Chat ops: block until the lock is free, then mirror.
    Wait,
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> ChatError + '_ {
    move |source| ChatError::Io {
        path: path.to_owned(),
        source,
    }
}

/// `<history_dir>/<session>/<chat>/`. Pure.
pub fn bundle_dir(history_dir: &Path, session: &SessionId, chat_id: &str) -> PathBuf {
    history_dir.join(session.to_string()).join(chat_id)
}

/// Mirror one chat's bundle; `None` if `src` is not on disk. The bundle dir is returned even
/// when a concurrent mirror made this one skip.
pub fn ingest_chat(
    history_dir: &Path,
    session: &SessionId,
    chat_id: &str,
    engine: &str,
    src: &Path,
    mode: IngestMode,
) -> Result<Option<PathBuf>, ChatError> {
    if !src.is_file() {
        return Ok(None);
    }
    let bundle = bundle_dir(history_dir, session, chat_id);
    ensure_dir(&bundle).map_err(io_err(&bundle))?;
    let Some(_lock) = IngestLock::acquire(&bundle.join(INGEST_LOCK_NAME), mode)? else {
        return Ok(Some(bundle));
    };
    append_by_offset(src, &bundle.join(BUNDLE_TRANSCRIPT_NAME))?;
    for sidecar in bundle_sidecars(engine, src, chat_id) {
        copy_tree_if_absent(&sidecar, &bundle)?;
    }
    Ok(Some(bundle))
}

/// Mirror every chat in `ext.history`, then stamp `bundle` onto the ones that changed.
/// An unknown session is a no-op.
pub fn ingest_session(
    core: &TxCore,
    history_dir: &Path,
    id: &SessionId,
    mode: IngestMode,
) -> Result<Vec<PathBuf>, ChatError> {
    let session = match core.get_session(id) {
        Ok(session) => session,
        Err(tx_core::CoreError::NotFound(_)) => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let history = history_ext(&session)?;
    let engine = chat_ext(&session)
        .ok()
        .and_then(|ext| ext.engine)
        .or_else(|| match &session.launch {
            Launch::Agent { engine, .. } => Some(engine.clone()),
            Launch::Process { .. } => None,
        })
        .unwrap_or_else(|| "claude".to_owned());
    let mut bundles = Vec::new();
    for (chat_id, chat) in &history.chats {
        let src = Path::new(&chat.transcript);
        if let Some(bundle) = ingest_chat(history_dir, id, chat_id, &engine, src, mode)? {
            bundles.push((chat_id.clone(), bundle));
        }
    }
    if bundles.is_empty() {
        return Ok(Vec::new());
    }
    core.update_ext(id, HISTORY_ID, |value| {
        let Ok(mut ext) = serde_json::from_value::<crate::ext::HistoryExt>(value.clone()) else {
            return;
        };
        let mut changed = false;
        for (chat_id, bundle) in &bundles {
            let bundle = bundle.to_string_lossy();
            if let Some(chat) = ext.chats.get_mut(chat_id)
                && chat.bundle.as_deref() != Some(bundle.as_ref())
            {
                chat.bundle = Some(bundle.into_owned());
                changed = true;
            }
        }
        if changed && let Ok(updated) = serde_json::to_value(&ext) {
            *value = updated;
        }
    })?;
    Ok(bundles.into_iter().map(|(_, bundle)| bundle).collect())
}

/// Mirror an append-only file: copy only the bytes past the destination's length; a fresh copy
/// when absent; a full re-copy when the destination is longer or its trailing window no longer
/// matches the source (an older divergence is not detected).
fn append_by_offset(src: &Path, dst: &Path) -> Result<(), ChatError> {
    let source_size = std::fs::metadata(src).map_err(io_err(src))?.len();
    let Ok(meta) = std::fs::metadata(dst) else {
        return copy_whole(src, dst);
    };
    let destination_size = meta.len();
    if destination_size > source_size || !is_prefix(src, dst, destination_size)? {
        return copy_whole(src, dst);
    }
    if destination_size == source_size {
        return Ok(());
    }
    let mut source = File::open(src).map_err(io_err(src))?;
    source
        .seek(SeekFrom::Start(destination_size))
        .map_err(io_err(src))?;
    let mut destination = OpenOptions::new()
        .append(true)
        .open(dst)
        .map_err(io_err(dst))?;
    io::copy(&mut source, &mut destination).map_err(io_err(dst))?;
    Ok(())
}

/// Temp file plus rename: a reader never sees a half copy.
fn copy_whole(src: &Path, dst: &Path) -> Result<(), ChatError> {
    let bytes = std::fs::read(src).map_err(io_err(src))?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(io_err(parent))?;
    }
    write_atomic(dst, &bytes).map_err(io_err(dst))
}

fn is_prefix(src: &Path, dst: &Path, destination_size: u64) -> Result<bool, ChatError> {
    if destination_size == 0 {
        return Ok(true);
    }
    let window = destination_size.min(PREFIX_CHECK_BYTES);
    let start = destination_size - window;
    Ok(read_window(dst, start, window)? == read_window(src, start, window)?)
}

fn read_window(path: &Path, start: u64, len: u64) -> Result<Vec<u8>, ChatError> {
    let mut file = File::open(path).map_err(io_err(path))?;
    file.seek(SeekFrom::Start(start)).map_err(io_err(path))?;
    let mut buffer = Vec::new();
    file.take(len)
        .read_to_end(&mut buffer)
        .map_err(io_err(path))?;
    Ok(buffer)
}

/// Copy every file under `src_dir` into `dst_dir`, skipping destinations that already exist.
/// A missing `src_dir` is a no-op; symlinked dirs are not descended.
fn copy_tree_if_absent(src_dir: &Path, dst_dir: &Path) -> Result<(), ChatError> {
    if !src_dir.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(src_dir).map_err(io_err(src_dir))? {
        let entry = entry.map_err(io_err(src_dir))?;
        let source = entry.path();
        let destination = dst_dir.join(entry.file_name());
        if entry.file_type().map_err(io_err(&source))?.is_dir() {
            copy_tree_if_absent(&source, &destination)?;
            continue;
        }
        if !source.is_file() || destination.exists() {
            continue;
        }
        std::fs::create_dir_all(dst_dir).map_err(io_err(dst_dir))?;
        std::fs::copy(&source, &destination).map_err(io_err(&destination))?;
    }
    Ok(())
}

/// A held `flock` on the per-chat lock file; released when dropped.
struct IngestLock {
    file: File,
}

impl IngestLock {
    /// `None` when not acquired: held under `Coalesce`, or any lock error.
    fn acquire(path: &Path, mode: IngestMode) -> Result<Option<Self>, ChatError> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o644)
            .open(path)
            .map_err(io_err(path))?;
        let acquired = match mode {
            IngestMode::Wait => file.lock().is_ok(),
            IngestMode::Coalesce => match file.try_lock() {
                Ok(()) => true,
                Err(TryLockError::WouldBlock | TryLockError::Error(_)) => false,
            },
        };
        Ok(acquired.then_some(Self { file }))
    }
}

impl Drop for IngestLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, SystemTime};

    use super::*;

    const TRANSCRIPT: &[u8] = b"{\"type\":\"user\",\"text\":\"one\"}\n{\"type\":\"assistant\"}\n";

    fn write(path: &Path, data: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    fn big() -> Vec<u8> {
        (0..=255u8).cycle().take(204_800).collect()
    }

    fn backdate(path: &Path) -> SystemTime {
        let past = SystemTime::now() - Duration::from_secs(100);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(past)
            .unwrap();
        past
    }

    fn sid() -> SessionId {
        "0f6b3b8a-1111-4222-8333-444444444444".parse().unwrap()
    }

    #[test]
    fn append_by_offset_appends_only_the_tail_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let data = big();
        let src = dir.path().join("src.jsonl");
        let dst = dir.path().join("out/dst.jsonl");
        write(&src, &data);
        append_by_offset(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), data);
        let inode = std::fs::metadata(&dst).unwrap().ino();

        let mut flipped = data.clone();
        flipped[10] ^= 0xFF; // outside the trailing window: spliced onto, not detected
        std::fs::write(&dst, &flipped).unwrap();
        let mut grown = data.clone();
        grown.extend_from_slice(&[b'T'; 50]);
        std::fs::write(&src, &grown).unwrap();
        append_by_offset(&src, &dst).unwrap();

        assert_eq!(std::fs::metadata(&dst).unwrap().ino(), inode);
        flipped.extend_from_slice(&[b'T'; 50]);
        assert_eq!(std::fs::read(&dst).unwrap(), flipped);
    }

    #[test]
    fn current_copy_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        write(&src, &big());
        append_by_offset(&src, &dst).unwrap();
        let past = backdate(&dst);
        append_by_offset(&src, &dst).unwrap();
        assert_eq!(std::fs::metadata(&dst).unwrap().modified().unwrap(), past);
    }

    #[test]
    fn shrink_or_window_mismatch_recopies() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        let data = big();
        write(&src, &data);
        append_by_offset(&src, &dst).unwrap();

        std::fs::write(&src, [b'x'; 80]).unwrap();
        append_by_offset(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), [b'x'; 80]);

        std::fs::write(&src, &data).unwrap();
        append_by_offset(&src, &dst).unwrap();
        let mut modified = data.clone();
        modified[data.len() - 100] ^= 0xFF;
        std::fs::write(&src, &modified).unwrap();
        append_by_offset(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), modified);

        std::fs::write(&dst, b"").unwrap();
        append_by_offset(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), modified);
    }

    #[test]
    fn sidecars_copied_if_absent_relative_to_resolved_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("h");
        let transcript = dir.path().join("claude/projects/-elsewhere/c1.jsonl");
        write(&transcript, TRANSCRIPT);
        let sidecar = transcript.parent().unwrap().join("c1");
        write(&sidecar.join("tool-results/a.txt"), b"OLD");
        write(&sidecar.join("subagents/s.jsonl"), b"S");
        let run = || {
            ingest_chat(
                &history,
                &sid(),
                "c1",
                "claude",
                &transcript,
                IngestMode::Wait,
            )
            .unwrap()
            .unwrap()
        };
        let bundle = run();
        let read = |rel: &str| std::fs::read(bundle.join(rel)).unwrap();
        assert_eq!(read("tool-results/a.txt"), b"OLD");
        assert_eq!(read("subagents/s.jsonl"), b"S");

        std::fs::write(sidecar.join("tool-results/a.txt"), b"NEW").unwrap();
        write(&sidecar.join("tool-results/b.txt"), b"B");
        run();
        assert_eq!(read("tool-results/a.txt"), b"OLD");
        assert_eq!(read("tool-results/b.txt"), b"B");
    }

    #[test]
    fn missing_source_is_none_and_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("h");
        let got = ingest_chat(
            &history,
            &sid(),
            "c1",
            "claude",
            &dir.path().join("nope.jsonl"),
            IngestMode::Wait,
        )
        .unwrap();
        assert!(got.is_none());
        assert!(!history.exists());
    }

    #[test]
    fn coalesce_skips_a_held_lock() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("h");
        let src = dir.path().join("c1.jsonl");
        write(&src, TRANSCRIPT);
        let bundle = bundle_dir(&history, &sid(), "c1");
        std::fs::create_dir_all(&bundle).unwrap();
        let holder = File::create(bundle.join(INGEST_LOCK_NAME)).unwrap();
        holder.lock().unwrap();

        let got = ingest_chat(&history, &sid(), "c1", "claude", &src, IngestMode::Coalesce);
        assert_eq!(got.unwrap(), Some(bundle.clone()));
        assert!(!bundle.join(BUNDLE_TRANSCRIPT_NAME).exists());
        drop(holder);
        // Wait, not Coalesce: a concurrently forked test child may briefly share the lock's
        // open file description until its exec closes it.
        ingest_chat(&history, &sid(), "c1", "claude", &src, IngestMode::Wait).unwrap();
        assert_eq!(
            std::fs::read(bundle.join(BUNDLE_TRANSCRIPT_NAME)).unwrap(),
            TRANSCRIPT
        );
    }

    #[test]
    fn wait_blocks_until_the_lock_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let history = dir.path().join("h");
        let src = dir.path().join("c1.jsonl");
        write(&src, TRANSCRIPT);
        let bundle = bundle_dir(&history, &sid(), "c1");
        std::fs::create_dir_all(&bundle).unwrap();
        let holder = File::create(bundle.join(INGEST_LOCK_NAME)).unwrap();
        holder.lock().unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let released = SystemTime::now();
            drop(holder);
            released
        });
        ingest_chat(&history, &sid(), "c1", "claude", &src, IngestMode::Wait).unwrap();
        let done = SystemTime::now();
        assert!(done >= releaser.join().unwrap());
        assert_eq!(
            std::fs::read(bundle.join(BUNDLE_TRANSCRIPT_NAME)).unwrap(),
            TRANSCRIPT
        );
    }
}
