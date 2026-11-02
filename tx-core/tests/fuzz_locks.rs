//! Stress and adversarial tests for `tx_core::locks`.
//!
//! Tests marked `#[ignore = "BUG: …"]` document real failures or sharp edges; they stay in the
//! file so the reproduction survives, and the suite stays green.

use std::cell::UnsafeCell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use proptest::prelude::*;

fn sid(id: &str) -> tx_core::AgentId {
    id.parse().unwrap()
}
use tx_core::locks::CREATE_LOCK;
use tx_core::{AgentId, LockGuard, Locks};

const CHILD_DIR_ENV: &str = "TX_FUZZ_LOCKS_CHILD_DIR";
const CHILD_HELPER: &str = "helper_child_hold_create";

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// A unique scratch directory under the OS temp dir, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "tx-fuzz-locks-{tag}-{}-{nanos}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn locks_dir(&self) -> PathBuf {
        self.0.join("locks")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `(enter, exit)` nanoseconds of one critical section.
type Span = (u128, u128);

/// Panics if any two spans overlap. Spans are closed intervals; touching endpoints are fine.
fn assert_disjoint(mut spans: Vec<Span>, what: &str) {
    spans.sort_unstable();
    for pair in spans.windows(2) {
        let (prev, next) = (pair[0], pair[1]);
        assert!(
            next.0 >= prev.1,
            "{what}: critical sections overlap: {prev:?} then {next:?}"
        );
    }
}

/// A plain non-atomic counter. Safe to share only because every access happens under the lock.
struct Racy(UnsafeCell<u64>);

// SAFETY: callers hold the file lock around every access; a data race here is exactly what the
// test is designed to detect (as a wrong final count).
unsafe impl Sync for Racy {}

impl Racy {
    fn bump(&self) {
        // SAFETY: see the `Sync` impl.
        let slot = unsafe { &mut *self.0.get() };
        let seen = *slot;
        thread::yield_now();
        *slot = seen + 1;
    }

    fn get(&self) -> u64 {
        // SAFETY: called after every worker has been joined.
        unsafe { *self.0.get() }
    }
}

/// Runs `threads × iters` critical sections, each taking the lock via `acquire`, and checks both
/// the non-overlap of timestamps and the plain counter.
fn hammer(
    threads: usize,
    iters: usize,
    acquire: impl Fn(usize) -> LockGuard + Send + Sync + 'static,
    what: &str,
) {
    let epoch = Instant::now();
    let counter = Arc::new(Racy(UnsafeCell::new(0)));
    let spans = Arc::new(Mutex::new(Vec::with_capacity(threads * iters)));
    let acquire = Arc::new(acquire);
    let workers: Vec<_> = (0..threads)
        .map(|t| {
            let counter = Arc::clone(&counter);
            let spans = Arc::clone(&spans);
            let acquire = Arc::clone(&acquire);
            thread::spawn(move || {
                let mut local = Vec::with_capacity(iters);
                for _ in 0..iters {
                    let guard = acquire(t);
                    let enter = epoch.elapsed().as_nanos();
                    counter.bump();
                    let exit = epoch.elapsed().as_nanos();
                    drop(guard);
                    local.push((enter, exit));
                }
                spans.lock().unwrap().extend(local);
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(
        counter.get(),
        (threads * iters) as u64,
        "{what}: lost updates"
    );
    let spans = Arc::try_unwrap(spans).unwrap().into_inner().unwrap();
    assert_eq!(spans.len(), threads * iters);
    assert_disjoint(spans, what);
}

fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

// ---------------------------------------------------------------------------------------------
// 1. Mutual exclusion under load, in-process
// ---------------------------------------------------------------------------------------------

#[test]
fn separate_locks_values_serialize_session_lock() {
    let scratch = Scratch::new("sep");
    let dir = scratch.locks_dir();
    hammer(
        16,
        200,
        move |_| Locks::new(&dir).session(&sid("x")).unwrap(),
        "separate Locks per acquisition",
    );
}

#[test]
fn shared_locks_value_serializes_session_lock() {
    let scratch = Scratch::new("shared");
    let locks = Arc::new(Locks::new(scratch.locks_dir()));
    hammer(
        16,
        200,
        move |_| locks.agent(&sid("x")).unwrap(),
        "one shared Locks",
    );
}

#[test]
fn per_thread_locks_values_serialize_session_lock() {
    let scratch = Scratch::new("perthread");
    let dir = scratch.locks_dir();
    let per_thread: Vec<Locks> = (0..16).map(|_| Locks::new(&dir)).collect();
    hammer(
        16,
        200,
        move |t| per_thread[t].session(&sid("x")).unwrap(),
        "one Locks per thread",
    );
}

#[test]
fn shared_locks_value_serializes_create_lock() {
    let scratch = Scratch::new("create");
    let locks = Arc::new(Locks::new(scratch.locks_dir()));
    hammer(
        8,
        200,
        move |_| locks.create().unwrap(),
        "create() on one shared Locks",
    );
}

#[test]
fn different_sessions_run_concurrently_under_load() {
    // Sanity: distinct ids must not serialize, or the exclusion tests above prove nothing.
    let scratch = Scratch::new("distinct");
    let locks = Arc::new(Locks::new(scratch.locks_dir()));
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|t| {
            let locks = Arc::clone(&locks);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let _guard = locks.agent(&sid(format!("s{t}").as_str())).unwrap();
                // Every thread holds its own lock here; a wrongly-serialized lock would deadlock.
                barrier.wait();
            })
        })
        .collect();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for worker in workers {
            worker.join().unwrap();
        }
        tx.send(()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("distinct session locks blocked each other");
}

// ---------------------------------------------------------------------------------------------
// 2. Cross-process mutual exclusion
// ---------------------------------------------------------------------------------------------

/// Child body for `cross_process_create_lock_is_exclusive`; a no-op unless re-executed with the
/// marker env var.
#[test]
fn helper_child_hold_create() {
    let Ok(dir) = std::env::var(CHILD_DIR_ENV) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let locks = Locks::new(dir.join("locks"));
    let guard = locks.create().unwrap();
    let enter = now_nanos();
    thread::sleep(Duration::from_millis(100));
    let exit = now_nanos();
    // Written while still holding the lock, so the log itself needs no extra serialization.
    let mut log = OpenOptions::new()
        .append(true)
        .create(true)
        .open(dir.join("log"))
        .unwrap();
    writeln!(log, "{} {enter} {exit}", std::process::id()).unwrap();
    log.sync_all().unwrap();
    drop(guard);
}

#[test]
fn cross_process_create_lock_is_exclusive() {
    let scratch = Scratch::new("xproc");
    let exe = std::env::current_exe().unwrap();
    let started = Instant::now();
    let children: Vec<_> = (0..8)
        .map(|_| {
            Command::new(&exe)
                .args([CHILD_HELPER, "--exact", "--nocapture"])
                .env(CHILD_DIR_ENV, scratch.path())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let elapsed = started.elapsed();
    let log = fs::read_to_string(scratch.path().join("log")).unwrap();
    let spans: Vec<Span> = log
        .lines()
        .map(|line| {
            let mut fields = line.split_whitespace().skip(1);
            let enter = fields.next().unwrap().parse().unwrap();
            let exit = fields.next().unwrap().parse().unwrap();
            (enter, exit)
        })
        .collect();
    assert_eq!(spans.len(), 8, "log:\n{log}");
    assert_disjoint(spans, "8 processes holding create.lock");
    assert!(
        elapsed >= Duration::from_millis(800),
        "8 × 100 ms holds finished in {elapsed:?}; they ran concurrently"
    );
}

// ---------------------------------------------------------------------------------------------
// 3. Lock order / no deadlock
// ---------------------------------------------------------------------------------------------

#[test]
fn create_then_session_order_never_deadlocks() {
    let scratch = Scratch::new("order");
    let locks = Arc::new(Locks::new(scratch.locks_dir()));
    let (tx, rx) = mpsc::channel();
    for t in 0..8 {
        let locks = Arc::clone(&locks);
        let tx = tx.clone();
        thread::spawn(move || {
            for i in 0..50 {
                let id = sid(format!("s{}", (t + i) % 3).as_str());
                let create = locks.create().unwrap();
                let session = locks.agent(&id).unwrap();
                thread::yield_now();
                drop(session);
                drop(create);
                // A sibling that only takes the session lock, out of band from create().
                let _only = locks.agent(&id).unwrap();
            }
            tx.send(()).unwrap();
        });
    }
    drop(tx);
    let deadline = Instant::now() + Duration::from_secs(10);
    for _ in 0..8 {
        let left = deadline.saturating_duration_since(Instant::now());
        rx.recv_timeout(left)
            .expect("deadlock: a worker did not finish within 10 s");
    }
}

// ---------------------------------------------------------------------------------------------
// 4. Weird ids
// ---------------------------------------------------------------------------------------------

/// Every entry directly under `dir`, as file names.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Ids outside `[A-Za-z0-9_-]{1,64}` cannot be constructed, so no lock file can leave the dir.
#[test]
fn ids_that_could_escape_the_locks_dir_cannot_be_built() {
    for bad in [
        "",
        ".",
        "..",
        "a\nb",
        "a/b",
        "../x",
        "/tmp/evil",
        &"x".repeat(65),
    ] {
        assert!(bad.parse::<AgentId>().is_err(), "{bad:?}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, .. ProptestConfig::default() })]

    /// Every constructible id lands directly in the locks dir.
    #[test]
    fn every_id_stays_inside_locks_dir(id in "[A-Za-z0-9_-]{1,64}") {
        let scratch = Scratch::new("prop");
        let locks = Locks::new(scratch.locks_dir());
        let _guard = locks.agent(&sid(id.as_str())).unwrap();
        let names = entries(&scratch.locks_dir());
        prop_assert_eq!(names, vec![format!("{id}.lock")]);
        prop_assert_eq!(entries(scratch.path()), vec!["locks".to_owned()]);
    }

    /// Distinct ids never block each other.
    #[test]
    fn distinct_ids_do_not_contend(a in "[A-Za-z0-9_-]{1,32}", b in "[A-Za-z0-9_-]{1,32}") {
        // Case-insensitive filesystems (macOS APFS default) make `a` and `A` the same lock.
        prop_assume!(a.to_lowercase() != b.to_lowercase());
        let scratch = Scratch::new("prop-distinct");
        let locks = Locks::new(scratch.locks_dir());
        let _a = locks.agent(&sid(a.as_str())).unwrap();
        let started = Instant::now();
        let _b = locks.agent(&sid(b.as_str())).unwrap();
        prop_assert!(started.elapsed() < Duration::from_millis(100));
    }
}

#[test]
#[cfg(target_os = "macos")]
fn ids_differing_only_by_case_observed_to_share_a_lock_on_macos() {
    let scratch = Scratch::new("case");
    let locks = Locks::new(scratch.locks_dir());
    let _lower = locks.agent(&sid("abc")).unwrap();
    let (tx, rx) = mpsc::channel();
    let other = locks.clone();
    thread::spawn(move || {
        let _upper = other.session(&sid("ABC")).unwrap();
        tx.send(()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "\"abc\" and \"ABC\" did not contend; is this volume case-sensitive?"
    );
    assert_eq!(entries(&scratch.locks_dir()), vec!["abc.lock".to_owned()]);
}

// ---------------------------------------------------------------------------------------------
// 5. Dropping a guard while another thread waits
// ---------------------------------------------------------------------------------------------

#[test]
fn waiter_proceeds_promptly_after_drop() {
    let scratch = Scratch::new("prompt");
    let locks = Locks::new(scratch.locks_dir());
    let id = sid("x");
    let guard = locks.agent(&id).unwrap();
    let (tx, rx) = mpsc::channel();
    let other = locks.clone();
    let waiter = thread::spawn(move || {
        let _guard = other.session(&sid("x")).unwrap();
        tx.send(Instant::now()).unwrap();
    });
    thread::sleep(Duration::from_millis(100));
    let released = Instant::now();
    drop(guard);
    let acquired = rx.recv().unwrap();
    waiter.join().unwrap();
    let latency = acquired.saturating_duration_since(released);
    assert!(latency < Duration::from_millis(50), "{latency:?}");
}

#[test]
fn guard_outlives_the_locks_value_and_still_excludes() {
    let scratch = Scratch::new("outlive");
    let dir = scratch.locks_dir();
    let guard = Locks::new(&dir).create().unwrap();
    let (tx, rx) = mpsc::channel();
    let other_dir = dir.clone();
    thread::spawn(move || {
        let _guard = Locks::new(other_dir).create().unwrap();
        tx.send(()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "lock was not held"
    );
    drop(guard);
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
}

#[test]
fn same_thread_reacquire_is_not_reentrant() {
    // A second `session("x")` on the same thread opens a new descriptor and blocks on itself.
    let scratch = Scratch::new("reentrant");
    let locks = Locks::new(scratch.locks_dir());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _outer = locks.agent(&sid("x")).unwrap();
        let _inner = locks.agent(&sid("x")).unwrap();
        tx.send(()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(300)).is_err(),
        "nested acquire returned; the lock is reentrant"
    );
    // The thread stays blocked forever; the test process exits and reaps it.
}

// ---------------------------------------------------------------------------------------------
// 6. Lock file permissions and content preservation
// ---------------------------------------------------------------------------------------------

#[test]
fn lock_files_are_never_truncated_or_deleted() {
    let scratch = Scratch::new("preserve");
    let locks = Locks::new(scratch.locks_dir());
    let id = sid("x");
    // First acquisition creates the files; then plant content and a distinctive mode.
    drop(locks.create().unwrap());
    drop(locks.agent(&id).unwrap());
    let create_path = scratch.locks_dir().join(CREATE_LOCK);
    let session_path = scratch.locks_dir().join("x.lock");
    let payload = b"do not touch\n".repeat(5);
    for path in [&create_path, &session_path] {
        fs::write(path, &payload).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let before: Vec<_> = [&create_path, &session_path]
        .into_iter()
        .map(|p| fs::metadata(p).unwrap())
        .collect();
    for _ in 0..1000 {
        let _create = locks.create().unwrap();
        let _session = locks.agent(&id).unwrap();
    }
    for (path, meta) in [&create_path, &session_path].into_iter().zip(before) {
        let after = fs::metadata(path).unwrap();
        assert_eq!(fs::read(path).unwrap(), payload, "{path:?} content changed");
        assert_eq!(after.len(), payload.len() as u64);
        assert_eq!(
            after.permissions().mode() & 0o777,
            0o640,
            "{path:?} mode changed"
        );
        assert_eq!(after.ino(), meta.ino(), "{path:?} was replaced");
    }
    assert_eq!(
        entries(&scratch.locks_dir()),
        vec![CREATE_LOCK.to_owned(), "x.lock".to_owned()]
    );
}

use std::os::unix::fs::MetadataExt;

#[test]
fn fresh_lock_files_have_default_umask_mode() {
    let scratch = Scratch::new("mode");
    let locks = Locks::new(scratch.locks_dir());
    let _guard = locks.create().unwrap();
    let mode = fs::metadata(scratch.locks_dir().join(CREATE_LOCK))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    // OpenOptions::create uses 0o666 & !umask; whatever the umask, the owner can read and write
    // and nothing is executable.
    assert_eq!(mode & 0o600, 0o600, "{mode:o}");
    assert_eq!(mode & 0o111, 0, "{mode:o}");
    let file = fs::metadata(scratch.locks_dir().join(CREATE_LOCK)).unwrap();
    assert_eq!(file.len(), 0);
}

#[test]
fn read_only_lock_file_cannot_be_acquired() {
    // acquire opens read+write; a 0o444 lock file left behind by another user breaks the API.
    let scratch = Scratch::new("ro-file");
    let locks = Locks::new(scratch.locks_dir());
    drop(locks.create().unwrap());
    let path = scratch.locks_dir().join(CREATE_LOCK);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    let result = locks.create();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        result.unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
}

// ---------------------------------------------------------------------------------------------
// Extra: a lock file removed under a holder silently breaks exclusion
// ---------------------------------------------------------------------------------------------

#[test]
fn deleting_lock_file_under_a_holder_observed_to_break_exclusion() {
    let scratch = Scratch::new("unlink");
    let locks = Locks::new(scratch.locks_dir());
    let _held = locks.create().unwrap();
    fs::remove_file(scratch.locks_dir().join(CREATE_LOCK)).unwrap();
    let started = Instant::now();
    let _second = locks.create().unwrap();
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "a new inode does not contend"
    );
}

#[test]
#[ignore = "LIMITATION: flock is per inode; core never deletes lock files, so an operator removing locks/ while agents run is unsupported"]
fn deleting_lock_file_under_a_holder_keeps_exclusion() {
    let scratch = Scratch::new("unlink-ideal");
    let locks = Locks::new(scratch.locks_dir());
    let _held = locks.create().unwrap();
    fs::remove_file(scratch.locks_dir().join(CREATE_LOCK)).unwrap();
    let (tx, rx) = mpsc::channel();
    let other = locks.clone();
    thread::spawn(move || {
        let _second = other.create().unwrap();
        tx.send(()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "exclusion lost"
    );
}
