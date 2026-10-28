//! Durable file replacement.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Replaces `path` with `bytes` so a reader never sees half a file: a `.<name>.*.tmp` file
/// beside it (mode 0600, or the replaced file's mode), fsynced, renamed over `path`, then the
/// directory fsynced so the rename survives a crash. The temp file is removed on any failure.
/// New files are 0600 on purpose: records, transcripts and settings are the user's alone.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_atomic_naming(path, bytes).map_err(|(_, error)| error)
}

/// [`write_atomic`], naming the path each failure happened on: the directory (creating the temp
/// file, the final sync), the temp file (writing it), or `path` (the rename).
pub(crate) fn write_atomic_naming(path: &Path, bytes: &[u8]) -> Result<(), (PathBuf, io::Error)> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let at = |path: &Path| {
        let path = path.to_path_buf();
        move |error| (path, error)
    };
    let name = path.file_name().map(|name| name.to_string_lossy());
    let mut temp = tempfile::Builder::new()
        .prefix(&format!(".{}.", name.unwrap_or_default()))
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(at(dir))?;
    if let Ok(meta) = std::fs::metadata(path) {
        temp.as_file()
            .set_permissions(meta.permissions())
            .map_err(at(temp.path()))?;
    }
    temp.write_all(bytes).map_err(at(temp.path()))?;
    temp.as_file().sync_all().map_err(at(temp.path()))?;
    temp.persist(path).map_err(|error| at(path)(error.error))?;
    std::fs::File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(at(dir))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn replaces_keeps_the_mode_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.json");
        write_atomic(&file, b"one").unwrap();
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic(&file, b"two").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"two");
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
