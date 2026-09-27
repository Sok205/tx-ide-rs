//! `$TX_IDE_HOME/tutor/<lang>.json`: where the project lives and how far the user got.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::storage::Home;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub dir: PathBuf,
    pub current: String,
    #[serde(default)]
    pub passed: Vec<String>,
}

impl Progress {
    pub fn path(home: &Home, lang: &str) -> PathBuf {
        home.root().join("tutor").join(format!("{lang}.json"))
    }

    pub fn load(path: &Path) -> io::Result<Option<Self>> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map(Some).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Atomic: a temp file in the same directory, renamed over `path`.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let parent = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        temp.persist(path).map(drop).map_err(|error| error.error)
    }

    pub fn mark_passed(&mut self, id: &str) {
        if !self.is_passed(id) {
            self.passed.push(id.to_owned());
        }
    }

    pub fn is_passed(&self, id: &str) -> bool {
        self.passed.iter().any(|passed| passed == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips() {
        let home = tempfile::tempdir().unwrap();
        let home = Home::new(home.path());
        let path = Progress::path(&home, "python");
        assert_eq!(path, home.root().join("tutor/python.json"));
        assert_eq!(Progress::load(&path).unwrap(), None);
        let mut progress = Progress {
            dir: "/p".into(),
            current: "02-b".into(),
            passed: vec![],
        };
        progress.mark_passed("01-a");
        progress.mark_passed("01-a");
        assert_eq!(progress.passed, ["01-a"]);
        assert!(progress.is_passed("01-a") && !progress.is_passed("02-b"));
        progress.save(&path).unwrap();
        assert_eq!(Progress::load(&path).unwrap(), Some(progress));
    }

    #[test]
    fn corrupt_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("python.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(Progress::load(&path).is_err());
    }
}
