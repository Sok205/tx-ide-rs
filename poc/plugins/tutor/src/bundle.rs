//! The lesson packs as plugin resources: the repo's `tutor/` tree, compiled in under
//! `tutor/<path>`, plus the plugin's own `lesson.lua`. `materialise` writes them into the
//! plugin's data dir, where nvim and the scaffold read them.

use std::io::{self, Write};
use std::path::Path;

/// Every resource name starts with this.
pub const PREFIX: &str = "tutor/";

macro_rules! from_repo {
    ($($path:literal),* $(,)?) => {
        &[$((concat!("tutor/", $path), include_bytes!(concat!("../../../../tutor/", $path)))),*]
    };
}

/// `(resource name, bytes)`; the `tutor/` path in the repo is the name.
pub const FILES: &[(&str, &[u8])] = from_repo![
    "keys.txt",
    "python/pack.toml",
    "python/lessons.toml",
    "python/skeleton/.gitignore",
    "python/skeleton/server.py",
    "python/solutions/04-first-route.py",
    "python/solutions/07-not-found.py",
    "python/lessons/01-what-is-a-server.md",
    "python/lessons/02-request-response.md",
    "python/lessons/03-sessions.md",
    "python/lessons/04-first-route.md",
    "python/lessons/05-spawn-worker.md",
    "python/lessons/06-review-and-merge.md",
    "python/lessons/07-not-found.md",
    "python/lessons/08-ask-from-code.md",
    "python/lessons/09-fork-post.md",
    "python/lessons/10-history-resume.md",
    "python/lessons/11-assistant-tests.md",
    "python/lessons/12-handover.md",
    "python/lessons/13-cleanup.md",
];

/// The lesson-buffer Lua: main's `tutor/lesson.lua`, with its tmux call bound to the core's
/// server (`-S`, `-f`) instead of whatever `$TMUX` names.
pub const LESSON_LUA: (&str, &[u8]) = ("tutor/lesson.lua", include_bytes!("../nvim/lesson.lua"));

/// Writes each `(name, bytes)` under `root` (the `tutor/` prefix stripped), skipping files that
/// already hold those bytes. Each write is temp-file + fsync + rename, so a concurrent reader
/// sees the old file or the new one.
pub fn materialise<'a>(
    root: &Path,
    files: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> io::Result<()> {
    for (name, bytes) in files {
        let Some(relative) = name.strip_prefix(PREFIX) else {
            continue;
        };
        let path = root.join(relative);
        if std::fs::read(&path).is_ok_and(|have| have == bytes) {
            continue;
        }
        let parent = path.parent().unwrap_or(root);
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(bytes)?;
        temp.as_file().sync_all()?;
        temp.persist(&path).map_err(|error| error.error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialise_writes_and_rewrites_only_changed_files() {
        let root = tempfile::tempdir().unwrap();
        materialise(
            root.path(),
            [("tutor/a/b.txt", &b"one"[..]), ("other/x", &b"no"[..])],
        )
        .unwrap();
        assert_eq!(std::fs::read(root.path().join("a/b.txt")).unwrap(), b"one");
        assert!(!root.path().join("x").exists() && !root.path().join("other").exists());
        materialise(root.path(), [("tutor/a/b.txt", &b"two"[..])]).unwrap();
        assert_eq!(std::fs::read(root.path().join("a/b.txt")).unwrap(), b"two");
    }
}
