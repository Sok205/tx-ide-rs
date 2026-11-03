//! Seeding Claude Code's per-project trust: `projects["<path>"].hasTrustDialogAccepted` in
//! Claude's JSON config file, so the first launch in a directory does not stop on the trust
//! dialog.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

const TRUST_KEY: &str = "hasTrustDialogAccepted";

/// What [`trust_project`] found and did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trusted {
    Added,
    AlreadyTrusted,
    NoConfigFile,
}

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("{}: {source}", path.display())]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("{}: `{key}` is not a JSON object", path.display())]
    NotAnObject { path: PathBuf, key: &'static str },
}

/// Marks `project` as trusted in the Claude config file at `config_file`: adds the one key and
/// preserves everything else, replacing the file atomically (the file a symlink points to, so the
/// link stays). A file that does not exist is left alone; Claude owns its creation.
pub fn trust_project(config_file: &Path, project: &Path) -> Result<Trusted, TrustError> {
    let io_error = |source| TrustError::Io {
        path: config_file.to_path_buf(),
        source,
    };
    let text = match fs::read_to_string(config_file) {
        Ok(text) => text,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(Trusted::NoConfigFile);
        }
        Err(source) => return Err(io_error(source)),
    };
    let mut config: Value = serde_json::from_str(&text).map_err(|source| TrustError::Json {
        path: config_file.to_path_buf(),
        source,
    })?;
    let not_an_object = |key| TrustError::NotAnObject {
        path: config_file.to_path_buf(),
        key,
    };
    let projects = config
        .as_object_mut()
        .ok_or_else(|| not_an_object("<root>"))?
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| not_an_object("projects"))?;
    let entry = projects
        .entry(project.to_string_lossy())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| not_an_object("projects.<path>"))?;
    if entry.get(TRUST_KEY) == Some(&Value::Bool(true)) {
        return Ok(Trusted::AlreadyTrusted);
    }
    entry.insert(TRUST_KEY.to_owned(), Value::Bool(true));

    let rendered = serde_json::to_string_pretty(&config).map_err(|source| TrustError::Json {
        path: config_file.to_path_buf(),
        source,
    })?;
    // The file itself, not a symlink to it: a symlinked config stays one.
    let resolved = config_file.canonicalize().map_err(io_error)?;
    replace_file(&resolved, &rendered).map_err(io_error)?;
    Ok(Trusted::Added)
}

/// Replaces `file` with `contents` through a sibling temporary file that is created, before any
/// byte is written, with the original's mode (so a private config is never briefly readable) and
/// removed again if anything after its creation fails. A temporary file that already exists is
/// someone else's: creation fails and it stays.
fn replace_file(file: &Path, contents: &str) -> io::Result<()> {
    let mut temporary_name = file.file_name().unwrap_or_default().to_os_string();
    temporary_name.push(".tx-trust.tmp");
    let temporary_file = file.with_file_name(temporary_name);
    let mode = fs::metadata(file)?.permissions().mode();
    let mut temporary = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&temporary_file)?;
    let written = temporary
        .write_all(contents.as_bytes())
        .and_then(|()| fs::rename(&temporary_file, file));
    if written.is_err() {
        let _ = fs::remove_file(&temporary_file);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_config(directory: &Path, config: &Value) -> PathBuf {
        let config_file = directory.join(".claude.json");
        fs::write(&config_file, config.to_string()).unwrap();
        config_file
    }

    fn read_config(config_file: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(config_file).unwrap()).unwrap()
    }

    #[test]
    fn a_missing_file_is_left_alone() {
        let directory = tempfile::tempdir().unwrap();
        let config_file = directory.path().join(".claude.json");

        let outcome = trust_project(&config_file, Path::new("/p")).unwrap();

        assert_eq!(outcome, Trusted::NoConfigFile);
        assert!(!config_file.exists());
    }

    #[test]
    fn trust_is_added_under_projects_and_nothing_else_changes() {
        let directory = tempfile::tempdir().unwrap();
        let original = json!({"numStartups": 3, "projects": {"/other": {"allowedTools": []}}});
        let config_file = write_config(directory.path(), &original);

        let outcome = trust_project(&config_file, Path::new("/p")).unwrap();

        assert_eq!(outcome, Trusted::Added);
        let mut expected = original;
        expected["projects"]["/p"] = json!({"hasTrustDialogAccepted": true});
        let actual = read_config(&config_file);
        assert_eq!(actual, expected);
        assert_eq!(actual["numStartups"], 3);
        let siblings = fs::read_dir(directory.path()).unwrap().count();
        assert_eq!(siblings, 1, "no temporary file is left behind");
    }

    #[test]
    fn an_existing_project_entry_gains_only_the_key() {
        let directory = tempfile::tempdir().unwrap();
        let config_file = write_config(
            directory.path(),
            &json!({"projects": {"/p": {"allowedTools": ["Bash"]}}}),
        );

        let outcome = trust_project(&config_file, Path::new("/p")).unwrap();

        assert_eq!(outcome, Trusted::Added);
        assert_eq!(
            read_config(&config_file)["projects"]["/p"],
            json!({"allowedTools": ["Bash"], "hasTrustDialogAccepted": true})
        );
    }

    #[test]
    fn a_second_call_is_a_no_op() {
        let directory = tempfile::tempdir().unwrap();
        let config_file = write_config(directory.path(), &json!({"projects": {}}));
        trust_project(&config_file, Path::new("/p")).unwrap();
        let after_first = fs::read(&config_file).unwrap();

        let outcome = trust_project(&config_file, Path::new("/p")).unwrap();

        assert_eq!(outcome, Trusted::AlreadyTrusted);
        assert_eq!(fs::read(&config_file).unwrap(), after_first);
    }

    #[test]
    fn a_symlinked_config_stays_a_symlink_and_its_target_gains_the_key() {
        let directory = tempfile::tempdir().unwrap();
        let dotfiles = directory.path().join("dotfiles");
        fs::create_dir(&dotfiles).unwrap();
        let target = write_config(&dotfiles, &json!({"projects": {}}));
        let link = directory.path().join("claude.json");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let outcome = trust_project(&link, Path::new("/p")).unwrap();

        assert_eq!(outcome, Trusted::Added);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            read_config(&target)["projects"]["/p"],
            json!({"hasTrustDialogAccepted": true})
        );
        assert_eq!(fs::read_dir(&dotfiles).unwrap().count(), 1);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn a_non_object_file_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let config_file = write_config(directory.path(), &json!([]));

        let outcome = trust_project(&config_file, Path::new("/p"));

        assert!(matches!(outcome, Err(TrustError::NotAnObject { .. })));
        assert_eq!(fs::read_to_string(&config_file).unwrap(), "[]");
    }

    #[test]
    fn a_0600_config_stays_0600() {
        let directory = tempfile::tempdir().unwrap();
        let config_file = write_config(directory.path(), &json!({}));
        fs::set_permissions(&config_file, fs::Permissions::from_mode(0o600)).unwrap();

        trust_project(&config_file, Path::new("/p")).unwrap();

        let mode = fs::metadata(&config_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn a_pre_existing_temporary_file_is_not_ours_to_delete_so_it_stays_and_the_call_fails() {
        let directory = tempfile::tempdir().unwrap();
        let config_file = write_config(directory.path(), &json!({"projects": {}}));
        let original = fs::read(&config_file).unwrap();
        let temporary_file = directory.path().join(".claude.json.tx-trust.tmp");
        fs::write(&temporary_file, "someone else's").unwrap();

        let outcome = trust_project(&config_file, Path::new("/p"));

        assert!(matches!(outcome, Err(TrustError::Io { .. })));
        assert_eq!(fs::read(&config_file).unwrap(), original);
        assert_eq!(
            fs::read_to_string(&temporary_file).unwrap(),
            "someone else's"
        );
    }

    #[test]
    fn the_temporary_file_is_removed_when_the_rename_fails() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("occupant"), "x").unwrap();
        let temporary_file = directory.path().join("target.tx-trust.tmp");

        // A non-empty directory cannot be replaced by a file, so the rename fails after the
        // temporary file exists.
        let outcome = replace_file(&target, "{}");

        assert!(outcome.is_err());
        assert!(!temporary_file.exists());
    }
}
