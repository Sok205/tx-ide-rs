//! Advisory file locks under `<state_dir>/locks/`: `create.lock` serializes name checks and
//! `<id>.lock` serializes writes to (and keystrokes into) one session. `std::fs::File::lock`, no
//! extra dependency. Lock order is always `create.lock` before `<id>.lock`.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use crate::session::SessionId;

pub const CREATE_LOCK: &str = "create.lock";
const LOCK_SUFFIX: &str = ".lock";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locks {
    dir: PathBuf,
}

/// An exclusive lock, released on drop.
#[derive(Debug)]
pub struct LockGuard {
    file: File,
    path: PathBuf,
}

impl LockGuard {
    /// Whether the lock file at the path is still the inode this guard holds. Lock files are
    /// never removed by core; if something else removes one, a holder cannot be protected (a
    /// fresh file is a fresh lock), but an acquirer can at least refuse to settle on a ghost.
    pub fn is_current(&self) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt;
        let held = self.file.metadata()?;
        match std::fs::metadata(&self.path) {
            Ok(at_path) => Ok(at_path.dev() == held.dev() && at_path.ino() == held.ino()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Closing the descriptor releases the lock anyway; an unlock error has nowhere to go.
        let _ = self.file.unlock();
    }
}

impl Locks {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Serializes name checks against session creation and renames.
    pub fn create(&self) -> io::Result<LockGuard> {
        self.acquire(&self.dir.join(CREATE_LOCK))
    }

    /// Serializes record writes and keystroke delivery for one session.
    pub fn session(&self, id: &SessionId) -> io::Result<LockGuard> {
        self.acquire(&self.dir.join(format!("{id}{LOCK_SUFFIX}")))
    }

    /// Open, lock, then confirm the path still names the locked inode; otherwise the file was
    /// replaced between open and lock and the guard would be exclusive on a ghost — retry.
    fn acquire(&self, path: &Path) -> io::Result<LockGuard> {
        std::fs::create_dir_all(&self.dir)?;
        const ATTEMPTS: usize = 8;
        for _ in 0..ATTEMPTS {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?;
            file.lock()?;
            let guard = LockGuard {
                file,
                path: path.to_owned(),
            };
            if guard.is_current()? {
                return Ok(guard);
            }
        }
        Err(io::Error::other(format!(
            "lock file {} keeps being replaced",
            path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(id: &str) -> SessionId {
        id.parse().unwrap()
    }
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn lock_files_live_under_the_dir() {
        let dir = tempfile::tempdir().unwrap();
        let locks = Locks::new(dir.path().join("locks"));
        let _create = locks.create().unwrap();
        let _session = locks.session(&sid("abc")).unwrap();
        assert!(dir.path().join("locks/create.lock").is_file());
        assert!(dir.path().join("locks/abc.lock").is_file());
    }

    #[test]
    fn same_lock_serializes_two_threads() {
        let dir = tempfile::tempdir().unwrap();
        let locks = Locks::new(dir.path().join("locks"));
        let (tx, rx) = mpsc::channel();
        let guard = locks.create().unwrap();
        let other = Locks::new(dir.path().join("locks"));
        let waiter = std::thread::spawn(move || {
            let started = Instant::now();
            let _guard = other.create().unwrap();
            tx.send(started.elapsed()).unwrap();
        });
        std::thread::sleep(Duration::from_millis(200));
        drop(guard);
        let waited = rx.recv().unwrap();
        waiter.join().unwrap();
        assert!(waited >= Duration::from_millis(150), "{waited:?}");
    }

    #[test]
    fn different_session_locks_do_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let locks = Locks::new(dir.path().join("locks"));
        let _a = locks.session(&sid("a")).unwrap();
        let started = Instant::now();
        let _b = locks.session(&sid("b")).unwrap();
        let _create = locks.create().unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn dropping_the_guard_releases_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let locks = Locks::new(dir.path().join("locks"));
        drop(locks.session(&sid("a")).unwrap());
        let started = Instant::now();
        let _again = locks.session(&sid("a")).unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn a_lock_file_replaced_under_an_acquirer_is_reacquired_on_the_new_inode() {
        let dir = tempfile::tempdir().unwrap();
        let locks = Locks::new(dir.path().join("locks"));
        let path = dir.path().join("locks/create.lock");
        drop(locks.create().unwrap());
        std::fs::remove_file(&path).unwrap();
        let guard = locks.create().unwrap();
        assert!(path.is_file());
        assert!(guard.is_current().unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(!guard.is_current().unwrap());
    }

    #[test]
    fn locks_dir_that_is_a_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("locks");
        std::fs::write(&file, "").unwrap();
        let locks = Locks::new(&file);
        assert!(locks.create().is_err());
        assert!(locks.session(&sid("a")).is_err());
    }

    #[test]
    fn read_only_locks_dir_is_an_error() {
        if unsafe_is_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locks");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o500)).unwrap();
        let locks = Locks::new(&path);
        assert!(locks.create().is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn unsafe_is_root() -> bool {
        std::fs::metadata("/").is_ok_and(|_| {
            std::process::Command::new("id")
                .arg("-u")
                .output()
                .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
        })
    }
}
