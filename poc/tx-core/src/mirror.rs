//! File mirroring for an engine's `archive` and `restore`: an append-only transcript is kept up
//! to date by byte offset, and a sidecar tree is copied file by file where the copy is absent.
//! Shared by every engine; nothing here knows what a transcript is.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::engine::EngineError;

/// Trailing bytes of the existing copy verified against the source before an offset append.
pub const PREFIX_CHECK_BYTES: u64 = 65536;

/// `EngineError::Io` naming `path`.
pub(crate) fn io_error(path: &Path) -> impl FnOnce(io::Error) -> EngineError + '_ {
    move |source| EngineError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Mirror an append-only file: copy only the bytes past the destination's length; a fresh copy
/// when absent; a full re-copy when the destination is longer or its trailing window no longer
/// matches the source (an older divergence is not detected). The destination's parents are
/// created. Idempotent: an up-to-date copy is left alone.
pub fn mirror_file(source: &Path, destination: &Path) -> Result<(), EngineError> {
    let source_size = std::fs::metadata(source).map_err(io_error(source))?.len();
    let Ok(destination_metadata) = std::fs::metadata(destination) else {
        return copy_whole(source, destination);
    };
    let destination_size = destination_metadata.len();
    if destination_size > source_size || !is_prefix(source, destination, destination_size)? {
        return copy_whole(source, destination);
    }
    if destination_size == source_size {
        return Ok(());
    }
    let mut source_file = File::open(source).map_err(io_error(source))?;
    source_file
        .seek(SeekFrom::Start(destination_size))
        .map_err(io_error(source))?;
    let mut destination_file = OpenOptions::new()
        .append(true)
        .open(destination)
        .map_err(io_error(destination))?;
    io::copy(&mut source_file, &mut destination_file).map_err(io_error(destination))?;
    Ok(())
}

/// Temp file in the destination's directory plus rename: a reader never sees a half copy.
fn copy_whole(source: &Path, destination: &Path) -> Result<(), EngineError> {
    let bytes = std::fs::read(source).map_err(io_error(source))?;
    let parent = destination.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(io_error(parent))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".mirror.")
        .suffix(".tmp")
        .tempfile_in(parent)
        .map_err(io_error(parent))?;
    temp.write_all(&bytes).map_err(io_error(temp.path()))?;
    temp.as_file().sync_all().map_err(io_error(temp.path()))?;
    temp.persist(destination)
        .map_err(|error| io_error(destination)(error.error))?;
    Ok(())
}

fn is_prefix(
    source: &Path,
    destination: &Path,
    destination_size: u64,
) -> Result<bool, EngineError> {
    if destination_size == 0 {
        return Ok(true);
    }
    let window = destination_size.min(PREFIX_CHECK_BYTES);
    let start = destination_size - window;
    Ok(read_window(destination, start, window)? == read_window(source, start, window)?)
}

fn read_window(path: &Path, start: u64, length: u64) -> Result<Vec<u8>, EngineError> {
    let mut file = File::open(path).map_err(io_error(path))?;
    file.seek(SeekFrom::Start(start)).map_err(io_error(path))?;
    let mut buffer = Vec::new();
    file.take(length)
        .read_to_end(&mut buffer)
        .map_err(io_error(path))?;
    Ok(buffer)
}

/// Copy every file under `source_directory` into `destination_directory`, skipping destinations
/// that already exist. A missing `source_directory` is a no-op; symlinked directories are not
/// descended; directories are created only where a file is copied.
pub fn mirror_tree(
    source_directory: &Path,
    destination_directory: &Path,
) -> Result<(), EngineError> {
    if !source_directory.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(source_directory).map_err(io_error(source_directory))? {
        let entry = entry.map_err(io_error(source_directory))?;
        let source = entry.path();
        let destination = destination_directory.join(entry.file_name());
        if entry.file_type().map_err(io_error(&source))?.is_dir() {
            mirror_tree(&source, &destination)?;
            continue;
        }
        if !source.is_file() || destination.exists() {
            continue;
        }
        std::fs::create_dir_all(destination_directory).map_err(io_error(destination_directory))?;
        std::fs::copy(&source, &destination).map_err(io_error(&destination))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, SystemTime};

    use super::*;

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

    #[test]
    fn mirror_file_appends_only_the_tail_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let data = big();
        let source = dir.path().join("source.jsonl");
        let destination = dir.path().join("out/destination.jsonl");
        write(&source, &data);
        mirror_file(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), data);
        let inode = std::fs::metadata(&destination).unwrap().ino();

        let mut flipped = data.clone();
        flipped[10] ^= 0xFF; // outside the trailing window: spliced onto, not detected
        std::fs::write(&destination, &flipped).unwrap();
        let mut grown = data.clone();
        grown.extend_from_slice(&[b'T'; 50]);
        std::fs::write(&source, &grown).unwrap();
        mirror_file(&source, &destination).unwrap();

        assert_eq!(std::fs::metadata(&destination).unwrap().ino(), inode);
        flipped.extend_from_slice(&[b'T'; 50]);
        assert_eq!(std::fs::read(&destination).unwrap(), flipped);
        assert_eq!(
            std::fs::read_dir(dir.path().join("out")).unwrap().count(),
            1,
            "no temp file left"
        );
    }

    #[test]
    fn current_copy_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        write(&source, &big());
        mirror_file(&source, &destination).unwrap();
        let past = backdate(&destination);
        mirror_file(&source, &destination).unwrap();
        assert_eq!(
            std::fs::metadata(&destination).unwrap().modified().unwrap(),
            past
        );
    }

    #[test]
    fn shrink_or_window_mismatch_recopies() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        let data = big();
        write(&source, &data);
        mirror_file(&source, &destination).unwrap();

        std::fs::write(&source, [b'x'; 80]).unwrap();
        mirror_file(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), [b'x'; 80]);

        std::fs::write(&source, &data).unwrap();
        mirror_file(&source, &destination).unwrap();
        let mut modified = data.clone();
        modified[data.len() - 100] ^= 0xFF;
        std::fs::write(&source, &modified).unwrap();
        mirror_file(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), modified);

        std::fs::write(&destination, b"").unwrap();
        mirror_file(&source, &destination).unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), modified);
    }

    #[test]
    fn missing_source_names_the_source() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("nope");
        let error = mirror_file(&source, &dir.path().join("destination")).unwrap_err();
        assert!(
            matches!(&error, EngineError::Io { path, .. } if *path == source),
            "{error:?}"
        );
        assert!(error.to_string().contains("nope"), "{error}");
        assert!(!dir.path().join("destination").exists());
    }

    #[test]
    fn mirror_tree_copies_absent_files_and_keeps_existing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        write(&source.join("tool-results/a.txt"), b"OLD");
        write(&source.join("subagents/s.jsonl"), b"S");
        std::fs::create_dir_all(source.join("empty")).unwrap();
        mirror_tree(&source, &destination).unwrap();
        let read = |relative: &str| std::fs::read(destination.join(relative)).unwrap();
        assert_eq!(read("tool-results/a.txt"), b"OLD");
        assert_eq!(read("subagents/s.jsonl"), b"S");
        assert!(!destination.join("empty").exists());

        std::fs::write(source.join("tool-results/a.txt"), b"NEW").unwrap();
        write(&source.join("tool-results/b.txt"), b"B");
        mirror_tree(&source, &destination).unwrap();
        assert_eq!(read("tool-results/a.txt"), b"OLD");
        assert_eq!(read("tool-results/b.txt"), b"B");
    }

    #[test]
    fn mirror_tree_of_a_missing_source_creates_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("destination");
        mirror_tree(&dir.path().join("nope"), &destination).unwrap();
        assert!(!destination.exists());
    }
}
