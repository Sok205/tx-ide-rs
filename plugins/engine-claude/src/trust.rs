//! Seeding Claude Code's per-project trust: `projects["<path>"].hasTrustDialogAccepted` in
//! Claude's JSON config file, so the first launch in a directory does not stop on the trust
//! dialog.

use std::fs;
use std::io;
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
/// preserves everything else, replacing the file atomically. A file that does not exist is left
/// alone; Claude owns its creation.
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

    let mut temporary_name = config_file.file_name().unwrap_or_default().to_os_string();
    temporary_name.push(".tx-trust.tmp");
    let temporary_file = config_file.with_file_name(temporary_name);
    let rendered = serde_json::to_string_pretty(&config).map_err(|source| TrustError::Json {
        path: config_file.to_path_buf(),
        source,
    })?;
    fs::write(&temporary_file, rendered).map_err(io_error)?;
    fs::set_permissions(
        &temporary_file,
        fs::metadata(config_file).map_err(io_error)?.permissions(),
    )
    .map_err(io_error)?;
    fs::rename(&temporary_file, config_file).map_err(io_error)?;
    Ok(Trusted::Added)
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
    fn a_non_object_file_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let config_file = write_config(directory.path(), &json!([]));

        let outcome = trust_project(&config_file, Path::new("/p"));

        assert!(matches!(outcome, Err(TrustError::NotAnObject { .. })));
        assert_eq!(fs::read_to_string(&config_file).unwrap(), "[]");
    }
}
