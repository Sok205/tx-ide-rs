//! History ingest + the transcript resolver (history.py).
//!
//! Mirrors a chat's bundle by COPY into `$TX_IDE_HOME/history/<tx-id>/<chat-uuid>/`: the
//! transcript by byte offset (append-only, with a trailing-window prefix check), the engine's
//! sidecar dirs copy-if-absent. One `flock` per (tx-id, chat) coalesces concurrent mirrors: the
//! hook path skips when one is in flight, `tx archive` / chat ops block for a complete mirror.
//! `bundle_path` is stamped back onto the record on a fresh reload (a concurrent write survives).

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use crate::engines::adapter::copy2;
use crate::engines::claude::{BUNDLE_TRANSCRIPT_NAME, bundle_dir};
use crate::engines::{EngineError, EngineRegistry};
use crate::session::Engine;
use crate::storage::Home;
use crate::store::{SessionStore, StoreError};

/// The per-(tx-id, chat) coalescing lock, hidden inside the bundle it guards.
pub const INGEST_LOCK_NAME: &str = ".ingest.lock";

/// Trailing bytes of the existing copy verified against the source before an offset append.
const PREFIX_CHECK_BYTES: u64 = 65536;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(transparent)]
    Copy(#[from] EngineError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> HistoryError + '_ {
    move |source| HistoryError::Io {
        path: path.to_owned(),
        source,
    }
}

/// How an ingest treats a mirror already in flight for the same chat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IngestMode {
    /// The hot Stop/SessionEnd path: skip the chat (the in-flight mirror or the next Stop
    /// catches up).
    Coalesce,
    /// `tx archive` / chat ops: block until the lock is free, then mirror.
    Wait,
}

/// The history component: where bundles live and which adapter resolves each engine.
#[derive(Clone, Copy)]
pub struct History<'a> {
    home: &'a Home,
    engines: &'a EngineRegistry,
}

impl<'a> History<'a> {
    pub fn new(home: &'a Home, engines: &'a EngineRegistry) -> Self {
        Self { home, engines }
    }

    pub fn home(&self) -> &'a Home {
        self.home
    }

    /// A chat's source transcript, or `None` if it is not on disk yet. The engine's fast path from
    /// a non-empty `cwd_hint`, else its own moved-cwd fallback (Q7 FIX: per engine, never the
    /// Claude projects glob for everyone). An unregistered engine resolves nothing.
    pub fn resolve_transcript(
        &self,
        chat_id: &str,
        cwd_hint: Option<&str>,
        engine: Engine,
    ) -> Option<PathBuf> {
        self.engines
            .get(engine)?
            .locate_transcript(chat_id, cwd_hint)
    }

    /// Mirror every captured chat of one tx session; the bundle paths touched, one per chat id in
    /// record order. An unknown session is a no-op. A chat without its own engine (pre-v3) uses the
    /// session's.
    pub fn ingest_session(
        &self,
        store: &SessionStore,
        session_id: &str,
        mode: IngestMode,
    ) -> Result<Vec<String>, HistoryError> {
        let Some(session) = store.load(session_id)? else {
            return Ok(Vec::new());
        };
        let Some(llm) = session.llm() else {
            return Ok(Vec::new());
        };
        let mut ingested: Vec<(String, String)> = Vec::new();
        for chat in &llm.chats {
            let Some(chat_id) = chat.id.as_deref() else {
                continue;
            };
            let engine = chat.engine.unwrap_or(llm.engine);
            let Some(bundle) = self.ingest_chat(&session.id, chat_id, &chat.cwd, engine, mode)?
            else {
                continue;
            };
            let bundle = bundle.to_string_lossy().into_owned();
            match ingested.iter_mut().find(|(id, _)| id == chat_id) {
                Some((_, existing)) => *existing = bundle,
                None => ingested.push((chat_id.to_owned(), bundle)),
            }
        }
        if !ingested.is_empty() {
            stamp_bundle_paths(store, session_id, &ingested)?;
        }
        Ok(ingested.into_iter().map(|(_, bundle)| bundle).collect())
    }

    /// Mirror one chat's bundle, or `None` if its source transcript is not on disk yet. The bundle
    /// dir is returned even when a concurrent mirror made this one skip.
    pub fn ingest_chat(
        &self,
        tx_id: &str,
        chat_id: &str,
        cwd_hint: &str,
        engine: Engine,
        mode: IngestMode,
    ) -> Result<Option<PathBuf>, HistoryError> {
        let Some(adapter) = self.engines.get(engine) else {
            return Ok(None);
        };
        let Some(src_transcript) = adapter.locate_transcript(chat_id, Some(cwd_hint)) else {
            return Ok(None);
        };
        let bundle = bundle_dir(self.home, tx_id, chat_id);
        std::fs::create_dir_all(&bundle).map_err(io_err(&bundle))?;
        let Some(_lock) = IngestLock::acquire(&bundle.join(INGEST_LOCK_NAME), mode)? else {
            return Ok(Some(bundle));
        };
        append_by_offset(&src_transcript, &bundle.join(BUNDLE_TRANSCRIPT_NAME))?;
        for sidecar in adapter.bundle_sidecars(&src_transcript, chat_id) {
            copy_tree_if_absent(&sidecar, &bundle)?;
        }
        Ok(Some(bundle))
    }
}

/// Mirror an append-only file: copy only the bytes past the destination's length; a fresh copy
/// when absent; a full re-copy when the destination is longer or its trailing window no longer
/// matches (Q8 PARITY: an older divergence is not detected).
fn append_by_offset(src: &Path, dst: &Path) -> Result<(), HistoryError> {
    let source_size = std::fs::metadata(src).map_err(io_err(src))?.len();
    if !dst.exists() {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        copy2(src, dst)?;
        return Ok(());
    }
    let destination_size = std::fs::metadata(dst).map_err(io_err(dst))?.len();
    if destination_size > source_size || !is_prefix(src, dst, destination_size)? {
        copy2(src, dst)?;
        return Ok(());
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

/// Whether the trailing window of `dst` equals the same range of `src`.
fn is_prefix(src: &Path, dst: &Path, destination_size: u64) -> Result<bool, HistoryError> {
    if destination_size == 0 {
        return Ok(true);
    }
    let window = destination_size.min(PREFIX_CHECK_BYTES);
    let start = destination_size - window;
    Ok(read_window(dst, start, window)? == read_window(src, start, window)?)
}

fn read_window(path: &Path, start: u64, len: u64) -> Result<Vec<u8>, HistoryError> {
    let mut file = File::open(path).map_err(io_err(path))?;
    file.seek(SeekFrom::Start(start)).map_err(io_err(path))?;
    let mut buffer = Vec::new();
    file.take(len)
        .read_to_end(&mut buffer)
        .map_err(io_err(path))?;
    Ok(buffer)
}

/// Copy every file under `src_dir` into `dst_dir`, skipping destinations that already exist.
/// A missing `src_dir` is a no-op. Like `Path.rglob` (3.13+), symlinked dirs are not descended.
fn copy_tree_if_absent(src_dir: &Path, dst_dir: &Path) -> Result<(), HistoryError> {
    if !src_dir.is_dir() {
        return Ok(());
    }
    let entries = std::fs::read_dir(src_dir).map_err(io_err(src_dir))?;
    for entry in entries {
        let entry = entry.map_err(io_err(src_dir))?;
        let source = entry.path();
        let destination = dst_dir.join(entry.file_name());
        let file_type = entry.file_type().map_err(io_err(&source))?;
        if file_type.is_dir() {
            copy_tree_if_absent(&source, &destination)?;
            continue;
        }
        if !source.is_file() || destination.exists() {
            continue;
        }
        std::fs::create_dir_all(dst_dir).map_err(io_err(dst_dir))?;
        copy2(&source, &destination)?;
    }
    Ok(())
}

/// A held `flock` on the per-chat lock file; released when dropped (the fd closes).
struct IngestLock {
    _file: File,
}

impl IngestLock {
    /// `None` when the lock was not acquired: already held under [`IngestMode::Coalesce`], or any
    /// lock error (the reference treats every `OSError` from `flock` as "held").
    fn acquire(path: &Path, mode: IngestMode) -> Result<Option<Self>, HistoryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
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
        Ok(acquired.then_some(Self { _file: file }))
    }
}

impl Drop for IngestLock {
    /// `LOCK_UN` before the close, as the reference does: a descriptor inherited by a child that is
    /// still between fork and exec must not keep the lock alive.
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

/// Stamp `bundle_path` onto each ingested chat, on a record reloaded right before the save so a
/// concurrent write (a rename) survives. Saves only when a path actually changes.
fn stamp_bundle_paths(
    store: &SessionStore,
    session_id: &str,
    ingested: &[(String, String)],
) -> Result<(), StoreError> {
    let Some(mut fresh) = store.load(session_id)? else {
        return Ok(());
    };
    let Some(llm) = fresh.llm_mut() else {
        return Ok(());
    };
    let mut changed = false;
    for chat in &mut llm.chats {
        let Some(chat_id) = chat.id.as_deref() else {
            continue;
        };
        let Some((_, target)) = ingested.iter().find(|(id, _)| id == chat_id) else {
            continue;
        };
        if chat.bundle_path.as_deref() != Some(target.as_str()) {
            chat.bundle_path = Some(target.clone());
            changed = true;
        }
    }
    if changed {
        store.save(&fresh)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;
    use std::rc::Rc;
    use std::time::{Duration, SystemTime};

    use serde_json::{Value, json};

    use super::*;
    use crate::engines::claude::munge;
    use crate::engines::{ClaudeEngine, CodexEngine, CodexHostEnv};
    use crate::session::Session;
    use crate::store::IgnoreSkips;

    const TRANSCRIPT: &[u8] = b"{\"type\":\"user\",\"text\":\"one\"}\n{\"type\":\"assistant\"}\n";

    struct Fixture {
        base: PathBuf,
        _root: tempfile::TempDir,
        home: Home,
        engines: EngineRegistry,
        store: SessionStore,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let base = root.path().canonicalize().unwrap();
            let home = Home::new(base.join("txhome"));
            let mut engines = EngineRegistry::new();
            let claude_dir = base.join("claude");
            engines.register(
                Engine::Claude,
                Rc::new(ClaudeEngine::new(
                    home.clone(),
                    Some(claude_dir.to_str().unwrap()),
                    None,
                )),
            );
            let host = CodexHostEnv {
                codex_home: Some(base.join("codex").to_string_lossy().into_owned()),
                home_dir: None,
                path: None,
            };
            engines.register(
                Engine::Codex,
                Rc::new(CodexEngine::new(home.clone(), host, PathBuf::from("tx"))),
            );
            let store = SessionStore::new(home.sessions_dir(), Rc::new(IgnoreSkips));
            Self {
                base,
                _root: root,
                home,
                engines,
                store,
            }
        }

        fn history(&self) -> History<'_> {
            History::new(&self.home, &self.engines)
        }

        fn workdir(&self) -> String {
            let dir = self.base.join("w");
            std::fs::create_dir_all(&dir).unwrap();
            dir.to_string_lossy().into_owned()
        }

        fn claude_transcript(&self, cwd: &str, chat_id: &str) -> PathBuf {
            self.base
                .join("claude/projects")
                .join(munge(cwd))
                .join(format!("{chat_id}.jsonl"))
        }

        fn record(&self, id: &str, chats: Value) {
            let record = json!({
                "schema_version": 6, "id": id, "name": "w1", "role": "llm", "state": "idle",
                "cwd": "/w", "cmd": "", "tags": [], "group": null, "env": {}, "parent": null,
                "pid": null, "attached_to": [], "created_at": 900.0, "ended_at": null,
                "engine": "claude", "last_activity": null, "chats": chats,
                "turn_started_at": null,
            });
            self.store
                .save(&Session::from_value(&record).unwrap())
                .unwrap();
        }

        fn load(&self, id: &str) -> Session {
            self.store.load(id).unwrap().unwrap()
        }

        fn ingest(&self, id: &str) -> Vec<String> {
            self.history()
                .ingest_session(&self.store, id, IngestMode::Coalesce)
                .unwrap()
        }

        fn bundle(&self, id: &str, chat_id: &str) -> PathBuf {
            self.home.history_dir().join(id).join(chat_id)
        }
    }

    fn chat(id: Option<&str>, cwd: &str, engine: Option<&str>) -> Value {
        json!({
            "id": id, "role": "original", "cwd": cwd, "transcript_path": "",
            "origin": {"how": "spawn", "session_id": "s", "chat_id": null},
            "bundle_path": null, "started_at": null, "ended_at": null, "summary": "",
            "engine": engine,
        })
    }

    fn write(path: &Path, data: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
    }

    fn big() -> Vec<u8> {
        (0..=255u8).cycle().take(204_800).collect()
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
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

    #[test]
    fn ingest_copies_transcript_and_stamps_bundle_path() {
        let fx = Fixture::new();
        let cwd = fx.workdir();
        fx.record(
            "s1",
            json!([
                chat(None, &cwd, Some("claude")),
                chat(Some("c1"), &cwd, Some("claude"))
            ]),
        );
        write(&fx.claude_transcript(&cwd, "c1"), TRANSCRIPT);

        let bundles = fx.ingest("s1");

        let bundle = fx.bundle("s1", "c1");
        assert_eq!(bundles, vec![bundle.to_string_lossy().into_owned()]);
        assert_eq!(
            std::fs::read(bundle.join("transcript.jsonl")).unwrap(),
            TRANSCRIPT
        );
        assert_eq!(entries(&bundle), [".ingest.lock", "transcript.jsonl"]);
        let chats = fx.load("s1").llm().unwrap().chats.clone();
        assert_eq!(chats[0].bundle_path, None);
        assert_eq!(
            chats[1].bundle_path.as_deref(),
            Some(bundle.to_str().unwrap())
        );
    }

    #[test]
    fn unknown_session_and_missing_transcript_are_no_ops() {
        let fx = Fixture::new();
        assert!(fx.ingest("nope").is_empty());
        let cwd = fx.workdir();
        fx.record("s1", json!([chat(Some("c1"), &cwd, Some("claude"))]));
        assert!(fx.ingest("s1").is_empty());
        assert!(!fx.home.history_dir().join("s1").exists());
        assert_eq!(fx.load("s1").llm().unwrap().chats[0].bundle_path, None);
    }

    #[test]
    fn chat_without_engine_uses_the_session_engine() {
        let fx = Fixture::new();
        let cwd = fx.workdir();
        fx.record("s1", json!([chat(Some("c1"), &cwd, None)]));
        write(&fx.claude_transcript(&cwd, "c1"), TRANSCRIPT);
        assert_eq!(fx.ingest("s1").len(), 1);
    }

    #[test]
    fn glob_fallback_prefers_cwd_munge_then_sorted_first() {
        let fx = Fixture::new();
        let projects = fx.base.join("claude/projects");
        write(&projects.join("-a/c1.jsonl"), b"A");
        write(&projects.join("-b/c1.jsonl"), b"B");
        let history = fx.history();
        let resolve = |cwd| history.resolve_transcript("c1", cwd, Engine::Claude);
        assert_eq!(resolve(Some("/b")), Some(projects.join("-b/c1.jsonl")));
        assert_eq!(resolve(Some("/zzz")), Some(projects.join("-a/c1.jsonl")));
        assert_eq!(resolve(Some("")), Some(projects.join("-a/c1.jsonl")));
        assert_eq!(resolve(None), Some(projects.join("-a/c1.jsonl")));
    }

    #[test]
    fn codex_fallback_does_not_glob_claude_projects() {
        let fx = Fixture::new();
        write(&fx.base.join("claude/projects/-w/r1.jsonl"), b"stray");
        fx.record("s1", json!([chat(Some("r1"), "/w", Some("codex"))]));
        assert!(fx.ingest("s1").is_empty());
        assert!(!fx.bundle("s1", "r1").exists());
    }

    #[test]
    fn codex_rollout_resolved_regardless_of_cwd_and_has_no_sidecars() {
        let fx = Fixture::new();
        let rollout = fx.base.join("codex/sessions/2026/09/23/rollout-x-r1.jsonl");
        write(&rollout, b"{\"type\":\"session_meta\"}\n");
        write(&rollout.parent().unwrap().join("r1/tool-results/x"), b"x");
        fx.record("s1", json!([chat(Some("r1"), "/nope/gone", Some("codex"))]));
        assert_eq!(fx.ingest("s1").len(), 1);
        assert_eq!(
            entries(&fx.bundle("s1", "r1")),
            [".ingest.lock", "transcript.jsonl"]
        );
    }

    #[test]
    fn append_by_offset_appends_only_the_tail_in_place() {
        let fx = Fixture::new();
        let data = big();
        let src = fx.base.join("src.jsonl");
        let dst = fx.base.join("out/dst.jsonl");
        write(&src, &data);
        append_by_offset(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), data);
        let inode = std::fs::metadata(&dst).unwrap().ino();

        let mut flipped = data.clone();
        flipped[10] ^= 0xFF; // outside the trailing window: spliced onto, not detected (Q8)
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
        let fx = Fixture::new();
        let src = fx.base.join("src");
        let dst = fx.base.join("dst");
        write(&src, &big());
        append_by_offset(&src, &dst).unwrap();
        let past = backdate(&dst);
        append_by_offset(&src, &dst).unwrap();
        assert_eq!(std::fs::metadata(&dst).unwrap().modified().unwrap(), past);
    }

    #[test]
    fn shrink_or_window_mismatch_recopies_and_empty_is_a_prefix() {
        let fx = Fixture::new();
        let src = fx.base.join("src");
        let dst = fx.base.join("dst");
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
        let fx = Fixture::new();
        fx.record("s1", json!([chat(Some("c1"), "/nope", Some("claude"))]));
        let transcript = fx.base.join("claude/projects/-elsewhere/c1.jsonl");
        write(&transcript, TRANSCRIPT);
        let sidecar = transcript.parent().unwrap().join("c1");
        write(&sidecar.join("tool-results/a.txt"), b"OLD");
        write(&sidecar.join("subagents/s.jsonl"), b"S");
        fx.ingest("s1");
        let bundle = fx.bundle("s1", "c1");
        let read = |rel: &str| std::fs::read(bundle.join(rel)).unwrap();
        assert_eq!(read("tool-results/a.txt"), b"OLD");
        assert_eq!(read("subagents/s.jsonl"), b"S");

        std::fs::write(sidecar.join("tool-results/a.txt"), b"NEW").unwrap();
        write(&sidecar.join("tool-results/b.txt"), b"B");
        fx.ingest("s1");
        assert_eq!(read("tool-results/a.txt"), b"OLD");
        assert_eq!(read("tool-results/b.txt"), b"B");
    }

    #[test]
    fn coalesce_skips_a_held_lock_but_still_stamps() {
        let fx = Fixture::new();
        let cwd = fx.workdir();
        fx.record("s1", json!([chat(Some("c1"), &cwd, Some("claude"))]));
        write(&fx.claude_transcript(&cwd, "c1"), TRANSCRIPT);
        let bundle = fx.bundle("s1", "c1");
        std::fs::create_dir_all(&bundle).unwrap();
        let holder = File::create(bundle.join(INGEST_LOCK_NAME)).unwrap();
        holder.lock().unwrap();

        assert_eq!(fx.ingest("s1").len(), 1);
        assert!(!bundle.join("transcript.jsonl").exists());
        assert_eq!(
            fx.load("s1").llm().unwrap().chats[0].bundle_path.as_deref(),
            Some(bundle.to_str().unwrap())
        );
        drop(holder);
        // Wait, not Coalesce: a concurrently forked test child may briefly share the lock's
        // open file description until its exec closes it (CLOEXEC).
        fx.history()
            .ingest_session(&fx.store, "s1", IngestMode::Wait)
            .unwrap();
        assert_eq!(
            std::fs::read(bundle.join("transcript.jsonl")).unwrap(),
            TRANSCRIPT
        );
    }

    #[test]
    fn wait_blocks_until_the_lock_is_released() {
        let fx = Fixture::new();
        let cwd = fx.workdir();
        fx.record("s1", json!([chat(Some("c1"), &cwd, Some("claude"))]));
        write(&fx.claude_transcript(&cwd, "c1"), TRANSCRIPT);
        let bundle = fx.bundle("s1", "c1");
        std::fs::create_dir_all(&bundle).unwrap();
        let holder = File::create(bundle.join(INGEST_LOCK_NAME)).unwrap();
        holder.lock().unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            let released = SystemTime::now();
            drop(holder);
            released
        });
        fx.history()
            .ingest_session(&fx.store, "s1", IngestMode::Wait)
            .unwrap();
        let done = SystemTime::now();
        assert!(done >= releaser.join().unwrap());
        assert_eq!(
            std::fs::read(bundle.join("transcript.jsonl")).unwrap(),
            TRANSCRIPT
        );
    }

    #[test]
    fn stamp_reloads_fresh_and_saves_only_on_change() {
        let fx = Fixture::new();
        let cwd = fx.workdir();
        fx.record("s1", json!([chat(Some("c1"), &cwd, Some("claude"))]));
        let ingested = vec![("c1".to_owned(), "/h/b".to_owned())];
        let mut renamed = fx.load("s1");
        renamed.name = "renamed".to_owned();
        fx.store.save(&renamed).unwrap();

        stamp_bundle_paths(&fx.store, "s1", &ingested).unwrap();
        let fresh = fx.load("s1");
        assert_eq!(fresh.name, "renamed");
        assert_eq!(
            fresh.llm().unwrap().chats[0].bundle_path.as_deref(),
            Some("/h/b")
        );

        let path = fx.store.path("s1");
        let past = backdate(&path);
        stamp_bundle_paths(&fx.store, "s1", &ingested).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), past);
    }

    #[test]
    fn duplicate_chat_ids_count_once() {
        let fx = Fixture::new();
        let cwd = fx.workdir();
        fx.record(
            "s1",
            json!([
                chat(Some("c1"), &cwd, Some("claude")),
                chat(Some("c1"), &cwd, Some("claude"))
            ]),
        );
        write(&fx.claude_transcript(&cwd, "c1"), TRANSCRIPT);
        assert_eq!(fx.ingest("s1").len(), 1);
    }
}
