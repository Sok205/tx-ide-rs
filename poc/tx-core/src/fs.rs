//! Durable file replacement.

use std::io::{self, Write};
use std::path::Path;

/// Replaces `path` with `bytes` so a reader never sees half a file: a `.<name>.*.tmp` file
/// beside it (mode 0600, or the replaced file's mode), fsynced, renamed over `path`, then the
/// directory fsynced so the rename survives a crash. The temp file is removed on any failure.
/// New files are 0600 on purpose: records, transcripts and settings are the user's alone.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path.file_name().map(|name| name.to_string_lossy());
    let mut temp = tempfile::Builder::new()
        .prefix(&format!(".{}.", name.unwrap_or_default()))
        .suffix(".tmp")
        .tempfile_in(dir)?;
    if let Ok(meta) = std::fs::metadata(path) {
        temp.as_file().set_permissions(meta.permissions())?;
    }
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|error| error.error)?;
    std::fs::File::open(dir)?.sync_all()
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
