//! `$TX_IDE_HOME` layout, `config.json`, and the `Storage` sync boundary (storage.py).
//!
//! `Home` is resolved once at the edge and passed down. [`Config`] is the one reader of
//! `config.json` (the Python reads it in reconcile.py and sync.py); a malformed file, a non-object
//! file, a bad threshold or a bad `sync` section is a [`ConfigError`] naming the file instead of the
//! reference's traceback (Q26 FIX). `SessionStore` does NOT go through [`Storage`] (OPEN-0a).

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::session::py_repr;

pub const DEFAULT_HOME: &str = "~/.tx-ide";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
}

impl Home {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// `$TX_IDE_HOME` (default `~/.tx-ide`), `~` expanded against `home_dir` (C9).
    pub fn resolve(tx_ide_home: Option<&str>, home_dir: Option<&Path>) -> Self {
        let raw = tx_ide_home.unwrap_or(DEFAULT_HOME);
        Self::new(expand_user(raw, home_dir))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }
    pub fn history_dir(&self) -> PathBuf {
        self.root.join("history")
    }
    pub fn artifacts_dir(&self) -> PathBuf {
        self.root.join("artifacts")
    }
    pub fn worktrees_dir(&self) -> PathBuf {
        self.root.join("worktrees")
    }
    pub fn chat_ops_dir(&self) -> PathBuf {
        self.root.join("chat-ops")
    }
    pub fn log_path(&self) -> PathBuf {
        self.root.join("log.jsonl")
    }
    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.json")
    }
    pub fn agents_dir(&self) -> PathBuf {
        self.root.join("agents")
    }
    pub fn user_agents_dir(&self) -> PathBuf {
        self.root.join("user-agents")
    }
    pub fn hooks_dir(&self) -> PathBuf {
        self.root.join("hooks")
    }
    pub fn engine_capture_shim(&self, engine: &str) -> PathBuf {
        self.hooks_dir().join(engine).join("start.sh")
    }
    pub fn launch_dir(&self) -> PathBuf {
        self.root.join("launch")
    }

    /// Create the skeleton (idempotent): exactly the dirs `ensure_home` makes.
    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [
            self.root.clone(),
            self.sessions_dir(),
            self.history_dir(),
            self.worktrees_dir(),
            self.user_agents_dir(),
            self.artifacts_dir(),
            self.launch_dir(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

/// `os.path.expanduser` for the `~` and `~/…` forms (`~user` is left as is).
pub fn expand_user(raw: &str, home_dir: Option<&Path>) -> PathBuf {
    match (raw.strip_prefix('~'), home_dir) {
        (Some(""), Some(home)) => home.to_path_buf(),
        (Some(rest), Some(home)) if rest.starts_with('/') => {
            py_path(&home.join(rest.trim_start_matches('/')).to_string_lossy())
        }
        _ => py_path(raw),
    }
}

/// `pathlib.PurePosixPath(raw)`: `""` and `"."` are `.`, repeated and trailing slashes and `.`
/// components are dropped (`..` is kept).
pub fn py_path(raw: &str) -> PathBuf {
    let path: PathBuf = Path::new(raw)
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect();
    if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path
    }
}

/// C5 stuck-`WORKING` threshold when `config.json` does not set one (reconcile.py).
pub const DEFAULT_STUCK_WORKING_SECONDS: f64 = 600.0;
pub const STUCK_THRESHOLD_KEY: &str = "stuck_working_threshold_seconds";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {}: {source}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("malformed {}: {source}", path.display())]
    Malformed {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("malformed {}: expected a JSON object, got {found}", path.display())]
    NotAnObject { path: PathBuf, found: String },
    #[error("malformed {}: '{STUCK_THRESHOLD_KEY}' must be a number, got {found}", path.display())]
    BadThreshold { path: PathBuf, found: String },
    #[error("{source} (in {})", path.display())]
    Sync {
        path: PathBuf,
        source: SyncSpecError,
    },
}

/// `$TX_IDE_HOME/config.json`. Load it where the reference reads it (the reconcile pass, `tx
/// sync`), not eagerly at startup: every other verb works with a malformed file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    path: PathBuf,
    data: Map<String, Value>,
}

impl Config {
    /// A missing file is an empty config (both Python readers check `path.exists()` first).
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    path: path.to_path_buf(),
                    data: Map::new(),
                });
            }
            Err(source) => {
                return Err(ConfigError::Read {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let value: Value =
            serde_json::from_str(&text).map_err(|source| ConfigError::Malformed {
                path: path.to_path_buf(),
                source,
            })?;
        match value {
            Value::Object(data) => Ok(Self {
                path: path.to_path_buf(),
                data,
            }),
            other => Err(ConfigError::NotAnObject {
                path: path.to_path_buf(),
                found: py_repr(&other),
            }),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The raw top-level object, for keys this module does not model.
    pub fn raw(&self) -> &Map<String, Value> {
        &self.data
    }

    /// `config.get("stuck_working_threshold_seconds", 600)`. Python compares whatever is there
    /// with a float, so an int, float or bool works and anything else is an error.
    pub fn stuck_working_threshold_seconds(&self) -> Result<f64, ConfigError> {
        match self.data.get(STUCK_THRESHOLD_KEY) {
            None => Ok(DEFAULT_STUCK_WORKING_SECONDS),
            Some(Value::Number(n)) => n
                .as_f64()
                .ok_or_else(|| self.bad_threshold(Value::Number(n.clone()))),
            Some(Value::Bool(b)) => Ok(f64::from(u8::from(*b))),
            Some(other) => Err(self.bad_threshold(other.clone())),
        }
    }

    fn bad_threshold(&self, found: Value) -> ConfigError {
        ConfigError::BadThreshold {
            path: self.path.clone(),
            found: py_repr(&found),
        }
    }

    /// `remote_from_config`: the `sync` section, `None` when absent or falsy.
    pub fn sync_remote(&self, home_dir: Option<&Path>) -> Result<Option<RemoteSpec>, ConfigError> {
        match self.data.get("sync") {
            Some(spec) if py_truthy(spec) => RemoteSpec::from_value(spec, home_dir)
                .map(Some)
                .map_err(|source| ConfigError::Sync {
                    path: self.path.clone(),
                    source,
                }),
            _ => Ok(None),
        }
    }
}

fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SyncSpecError {
    #[error("sync config must be a JSON object, got {0}")]
    NotAnObject(String),
    #[error("sync config is missing '{0}'")]
    MissingKey(&'static str),
    #[error("sync config '{0}' must be a string")]
    WrongType(&'static str),
    #[error("unknown sync backend '{0}' (expected 's3' or 'local')")]
    UnknownBackend(String),
}

/// A remote `Storage` selected by a `sync` spec (`remote_from_spec`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteSpec {
    S3 {
        bucket: String,
        prefix: String,
    },
    /// Already `~`-expanded.
    Local {
        path: PathBuf,
    },
}

impl RemoteSpec {
    /// `{"backend": "s3", "bucket": B, "prefix"?: P}` or `{"backend": "local", "path": P}`.
    pub fn from_value(spec: &Value, home_dir: Option<&Path>) -> Result<Self, SyncSpecError> {
        let Value::Object(spec) = spec else {
            return Err(SyncSpecError::NotAnObject(py_repr(spec)));
        };
        let text = |key: &'static str| match spec.get(key) {
            None => Err(SyncSpecError::MissingKey(key)),
            Some(Value::String(s)) => Ok(s.as_str()),
            Some(_) => Err(SyncSpecError::WrongType(key)),
        };
        let backend = spec
            .get("backend")
            .ok_or(SyncSpecError::MissingKey("backend"))?;
        match backend.as_str() {
            Some("s3") => Ok(RemoteSpec::S3 {
                bucket: text("bucket")?.to_owned(),
                prefix: match spec.get("prefix") {
                    None => String::new(),
                    Some(_) => text("prefix")?.to_owned(),
                },
            }),
            Some("local") => Ok(RemoteSpec::Local {
                path: expand_user(text("path")?, home_dir),
            }),
            Some(other) => Err(SyncSpecError::UnknownBackend(other.to_owned())),
            None => Err(SyncSpecError::UnknownBackend(py_repr(backend))),
        }
    }

    pub fn into_storage(self) -> Box<dyn Storage> {
        match self {
            RemoteSpec::S3 { bucket, prefix } => Box::new(S3Storage::new(bucket, prefix)),
            RemoteSpec::Local { path } => Box::new(LocalStorage::new(path)),
        }
    }
}

pub const S3_DEFERRED: &str = "S3 sync: deferred — the S3 backend is not implemented yet (§14). \
                               Use `--remote PATH` for a local archive, or configure it later.";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The S3 stub (`NotImplementedError` in the reference).
    #[error("{S3_DEFERRED}")]
    Deferred,
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

/// The `tx sync` copy boundary: a KV over keys relative to a root. NOT the record-access path.
pub trait Storage {
    fn get(&self, key: &str) -> Result<Vec<u8>, StorageError>;
    /// Creates intermediate directories.
    fn put(&self, key: &str, data: &[u8]) -> Result<(), StorageError>;
    /// Every key starting with `prefix`, sorted.
    fn list(&self, prefix: &str) -> Result<Vec<String>, StorageError>;
    /// No error when absent.
    fn delete(&self, key: &str) -> Result<(), StorageError>;
    fn exists(&self, key: &str) -> Result<bool, StorageError>;
    /// `remote_label`: `s3://bucket[/prefix]` or the root path.
    fn label(&self) -> String;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalStorage {
    root: PathBuf,
}

impl LocalStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, key: &str) -> PathBuf {
        self.root.join(key)
    }

    /// `rglob("*")` + `is_file()`: symlinked directories are not descended (3.13+ default), a
    /// symlink to a file counts, a missing root is empty.
    fn walk(&self, dir: &Path, rel: &str, out: &mut Vec<String>) -> Result<(), StorageError> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io_error(dir, source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| io_error(dir, source))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let key = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|source| io_error(&path, source))?;
            if file_type.is_dir() {
                self.walk(&path, &key, out)?;
            } else if path.is_file() {
                out.push(key);
            }
        }
        Ok(())
    }
}

fn io_error(path: &Path, source: io::Error) -> StorageError {
    StorageError::Io {
        path: path.to_path_buf(),
        source,
    }
}

impl Storage for LocalStorage {
    fn get(&self, key: &str) -> Result<Vec<u8>, StorageError> {
        let path = self.path(key);
        std::fs::read(&path).map_err(|source| io_error(&path, source))
    }

    fn put(&self, key: &str, data: &[u8]) -> Result<(), StorageError> {
        let path = self.path(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        }
        std::fs::write(&path, data).map_err(|source| io_error(&path, source))
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, StorageError> {
        let mut keys = Vec::new();
        self.walk(&self.root, "", &mut keys)?;
        keys.retain(|key| key.starts_with(prefix));
        keys.sort();
        Ok(keys)
    }

    fn delete(&self, key: &str) -> Result<(), StorageError> {
        let path = self.path(key);
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io_error(&path, error)),
            _ => Ok(()),
        }
    }

    fn exists(&self, key: &str) -> Result<bool, StorageError> {
        Ok(self.path(key).exists())
    }

    fn label(&self) -> String {
        self.root.display().to_string()
    }
}

/// Remote-storage stub: every operation is [`StorageError::Deferred`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct S3Storage {
    pub bucket: String,
    pub prefix: String,
}

impl S3Storage {
    pub fn new(bucket: impl Into<String>, prefix: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            prefix: prefix.into(),
        }
    }
}

impl Storage for S3Storage {
    fn get(&self, _key: &str) -> Result<Vec<u8>, StorageError> {
        Err(StorageError::Deferred)
    }

    fn put(&self, _key: &str, _data: &[u8]) -> Result<(), StorageError> {
        Err(StorageError::Deferred)
    }

    fn list(&self, _prefix: &str) -> Result<Vec<String>, StorageError> {
        Err(StorageError::Deferred)
    }

    fn delete(&self, _key: &str) -> Result<(), StorageError> {
        Err(StorageError::Deferred)
    }

    fn exists(&self, _key: &str) -> Result<bool, StorageError> {
        Err(StorageError::Deferred)
    }

    fn label(&self) -> String {
        if self.prefix.is_empty() {
            format!("s3://{}", self.bucket)
        } else {
            format!("s3://{}/{}", self.bucket, self.prefix)
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn home_paths_normalise_like_pathlib() {
        // Expected values: python3.14 -c 'from pathlib import Path; print(Path(x).expanduser())'
        let home = Path::new("/u");
        let cases = [
            ("", "."),
            (".", "."),
            ("/tmp/x/", "/tmp/x"),
            ("/tmp//x/./y", "/tmp/x/y"),
            ("./rel/", "rel"),
            ("a/../b", "a/../b"),
            ("~", "/u"),
            ("~/h//", "/u/h"),
            // Python raises "Could not determine home directory" here; the port keeps it literal.
            ("~other/x", "~other/x"),
        ];
        for (raw, want) in cases {
            assert_eq!(
                expand_user(raw, Some(home)),
                PathBuf::from(want),
                "raw: {raw:?}"
            );
        }
    }

    use super::*;
    use serde_json::json;

    fn config(text: &str) -> (tempfile::TempDir, Result<Config, ConfigError>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, text).unwrap();
        let loaded = Config::load(&path);
        (dir, loaded)
    }

    #[test]
    fn missing_config_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("config.json")).unwrap();
        assert_eq!(config.stuck_working_threshold_seconds().unwrap(), 600.0);
        assert_eq!(config.sync_remote(None).unwrap(), None);
    }

    #[test]
    fn threshold() {
        for (text, want) in [
            ("{}", 600.0),
            (r#"{"stuck_working_threshold_seconds": 60}"#, 60.0),
            (r#"{"stuck_working_threshold_seconds": 0}"#, 0.0),
            (r#"{"stuck_working_threshold_seconds": 1.5}"#, 1.5),
            (r#"{"stuck_working_threshold_seconds": true}"#, 1.0),
        ] {
            let (_dir, config) = config(text);
            assert_eq!(
                config.unwrap().stuck_working_threshold_seconds().unwrap(),
                want
            );
        }
        let (_dir, bad) = config(r#"{"stuck_working_threshold_seconds": "5"}"#);
        let err = bad.unwrap().stuck_working_threshold_seconds().unwrap_err();
        assert!(
            err.to_string().ends_with(
                "config.json: 'stuck_working_threshold_seconds' must be a number, got '5'"
            )
        );
    }

    #[test]
    fn malformed_config_names_the_file() {
        let (_dir, err) = config("{oops");
        let text = err.unwrap_err().to_string();
        assert!(
            text.starts_with("malformed ") && text.contains("config.json: "),
            "{text}"
        );
        let (_dir, err) = config("[1]");
        assert!(
            err.unwrap_err()
                .to_string()
                .ends_with("config.json: expected a JSON object, got [1]")
        );
    }

    #[test]
    fn sync_section() {
        let home = Path::new("/u");
        let remote = |text: &str| config(text).1.unwrap().sync_remote(Some(home));
        for empty in [
            r#"{}"#,
            r#"{"sync": null}"#,
            r#"{"sync": {}}"#,
            r#"{"sync": 0}"#,
        ] {
            assert_eq!(remote(empty).unwrap(), None, "{empty}");
        }
        assert_eq!(
            remote(r#"{"sync": {"backend": "s3", "bucket": "b", "prefix": "tx/"}}"#).unwrap(),
            Some(RemoteSpec::S3 {
                bucket: "b".into(),
                prefix: "tx/".into()
            })
        );
        assert_eq!(
            remote(r#"{"sync": {"backend": "local", "path": "~/arch"}}"#).unwrap(),
            Some(RemoteSpec::Local {
                path: "/u/arch".into()
            })
        );
        let err = remote(r#"{"sync": {"backend": "gcs"}}"#)
            .unwrap_err()
            .to_string();
        assert!(
            err.starts_with("unknown sync backend 'gcs' (expected 's3' or 'local') (in "),
            "{err}"
        );
        let err = remote(r#"{"sync": {"backend": "local"}}"#)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("sync config is missing 'path'"), "{err}");
        assert_eq!(
            RemoteSpec::from_value(&json!({"backend": 5}), None).unwrap_err(),
            SyncSpecError::UnknownBackend("5".into())
        );
    }

    #[test]
    fn local_storage_round_trip_and_list() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        let ext = dir.path().join("ext");
        std::fs::create_dir_all(&ext).unwrap();
        std::fs::write(ext.join("f"), "x").unwrap();
        let storage = LocalStorage::new(&root);
        assert_eq!(storage.list("").unwrap(), Vec::<String>::new());
        storage.put("a/.h", b"1").unwrap();
        storage.put("top", b"2").unwrap();
        std::os::unix::fs::symlink(&ext, root.join("link")).unwrap();
        std::os::unix::fs::symlink(ext.join("f"), root.join("flink")).unwrap();
        std::os::unix::fs::symlink("nowhere", root.join("broken")).unwrap();
        // Same keys as Python's `LocalStorage.list()` on this tree.
        assert_eq!(storage.list("").unwrap(), ["a/.h", "flink", "top"]);
        assert_eq!(storage.list("a").unwrap(), ["a/.h"]);
        assert_eq!(storage.get("top").unwrap(), b"2");
        assert!(storage.exists("top").unwrap());
        storage.delete("top").unwrap();
        storage.delete("top").unwrap();
        assert!(!storage.exists("top").unwrap());
        assert_eq!(storage.label(), root.display().to_string());
    }

    #[test]
    fn s3_is_deferred() {
        let s3 = S3Storage::new("bucket", "p/");
        assert_eq!(s3.label(), "s3://bucket/p/");
        assert_eq!(S3Storage::new("bucket", "").label(), "s3://bucket");
        assert_eq!(
            s3.list("").unwrap_err().to_string(),
            "S3 sync: deferred — the S3 backend is not implemented yet (§14). Use `--remote PATH` \
             for a local archive, or configure it later."
        );
    }
}
