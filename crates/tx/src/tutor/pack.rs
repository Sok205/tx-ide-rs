//! A tutorial pack: `tutor/<lang>/` holding `pack.toml`, `lessons.toml`, `skeleton/`,
//! `lessons/` and `solutions/`.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::DeserializeOwned;

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}: {message}", path.display())]
    Toml { path: PathBuf, message: String },
    #[error("{}: no lessons", path.display())]
    Empty { path: PathBuf },
    #[error("lesson id '{0}' appears twice")]
    DuplicateId(String),
    #[error("lesson {lesson}: missing file {}", path.display())]
    MissingFile { lesson: String, path: PathBuf },
    #[error("{}: missing skeleton/ directory", dir.display())]
    NoSkeleton { dir: PathBuf },
    #[error("no tutorial pack '{lang}' (available: {available})")]
    UnknownLang { lang: String, available: String },
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub name: String,
    pub run: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lesson {
    pub id: String,
    pub file: PathBuf,
    #[serde(default)]
    pub solution: Option<PathBuf>,
    /// Binaries the lesson needs beyond the pack's (e.g. `claude`); a warning, never a block.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Check {
    Http(HttpCheck),
    Tx(TxCheck),
    File(FileCheck),
    Command(CommandCheck),
    Confirm(ConfirmCheck),
}

impl Check {
    pub fn label(&self) -> &str {
        match self {
            Check::Http(check) => &check.label,
            Check::Tx(check) => &check.label,
            Check::File(check) => &check.label,
            Check::Command(check) => &check.label,
            Check::Confirm(check) => &check.label,
        }
    }
}

fn get() -> String {
    "GET".to_owned()
}

fn one() -> usize {
    1
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpCheck {
    pub label: String,
    #[serde(default = "get")]
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub body: Option<String>,
    pub status: u16,
    /// Header name → value glob (`*` wildcard, ASCII case-insensitive).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub json: bool,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TxCheck {
    pub label: String,
    pub session: SessionFilter,
    /// Minimum number of matching records.
    #[serde(default = "one")]
    pub count: usize,
    /// Pass only when no record matches.
    #[serde(default)]
    pub absent: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFilter {
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    /// Any-of; empty = any state.
    #[serde(default)]
    pub state: Vec<String>,
    #[serde(default)]
    pub has_parent: Option<bool>,
    /// Matches any chat's `origin.how` (`fork`, `handover`, `resume`, …).
    #[serde(default)]
    pub chat_origin: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileCheck {
    pub label: String,
    pub path: PathBuf,
    #[serde(default)]
    pub contains: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandCheck {
    pub label: String,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmCheck {
    pub label: String,
    pub prompt: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LessonsFile {
    lesson: Vec<Lesson>,
}

#[derive(Clone, Debug)]
pub struct Pack {
    pub lang: String,
    pub dir: PathBuf,
    pub meta: Meta,
    pub lessons: Vec<Lesson>,
}

impl Pack {
    /// The pack `<repo_root>/tutor/<lang>/`.
    pub fn find(repo_root: &Path, lang: &str) -> Result<Self, PackError> {
        let dir = repo_root.join("tutor").join(lang);
        if !dir.join("pack.toml").is_file() {
            return Err(PackError::UnknownLang {
                lang: lang.to_owned(),
                available: available(repo_root).join(", "),
            });
        }
        Self::load(lang, &dir)
    }

    pub fn load(lang: &str, dir: &Path) -> Result<Self, PackError> {
        let meta: Meta = read_toml(&dir.join("pack.toml"))?;
        let lessons_path = dir.join("lessons.toml");
        let lessons = read_toml::<LessonsFile>(&lessons_path)?.lesson;
        if lessons.is_empty() {
            return Err(PackError::Empty { path: lessons_path });
        }
        if !dir.join("skeleton").is_dir() {
            return Err(PackError::NoSkeleton {
                dir: dir.to_path_buf(),
            });
        }
        let mut seen = HashSet::new();
        for lesson in &lessons {
            if !seen.insert(lesson.id.as_str()) {
                return Err(PackError::DuplicateId(lesson.id.clone()));
            }
            for relative in std::iter::once(&lesson.file).chain(lesson.solution.as_ref()) {
                let path = dir.join(relative);
                if !path.is_file() {
                    return Err(PackError::MissingFile {
                        lesson: lesson.id.clone(),
                        path,
                    });
                }
            }
        }
        Ok(Self {
            lang: lang.to_owned(),
            dir: dir.to_path_buf(),
            meta,
            lessons,
        })
    }

    pub fn skeleton(&self) -> PathBuf {
        self.dir.join("skeleton")
    }

    pub fn lesson_path(&self, index: usize) -> PathBuf {
        self.dir.join(&self.lessons[index].file)
    }

    pub fn solution_path(&self, index: usize) -> Option<PathBuf> {
        self.lessons[index]
            .solution
            .as_ref()
            .map(|relative| self.dir.join(relative))
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.lessons.iter().position(|lesson| lesson.id == id)
    }

    /// A `goto` target: a 1-based number, an exact id, or a unique id prefix.
    pub fn resolve(&self, key: &str) -> Option<usize> {
        if let Ok(number) = key.parse::<usize>() {
            return (1..=self.lessons.len())
                .contains(&number)
                .then(|| number - 1);
        }
        if let Some(index) = self.index_of(key) {
            return Some(index);
        }
        let mut hits = self
            .lessons
            .iter()
            .enumerate()
            .filter(|(_, lesson)| lesson.id.starts_with(key));
        match (hits.next(), hits.next()) {
            (Some((index, _)), None) => Some(index),
            _ => None,
        }
    }
}

/// Pack names under `<repo_root>/tutor/`, sorted.
fn available(repo_root: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo_root.join("tutor"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join("pack.toml").is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn read_toml<T: DeserializeOwned>(path: &Path) -> Result<T, PackError> {
    let text = std::fs::read_to_string(path).map_err(|source| PackError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|error| PackError::Toml {
        path: path.to_path_buf(),
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str = "name = \"Test\"\nrun = [\"python3\", \"server.py\"]\nrequires = [\"python3\"]\n";

    fn pack_dir(lessons_toml: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pack.toml"), META).unwrap();
        std::fs::write(dir.path().join("lessons.toml"), lessons_toml).unwrap();
        std::fs::create_dir_all(dir.path().join("skeleton")).unwrap();
        std::fs::create_dir_all(dir.path().join("lessons")).unwrap();
        for name in ["01-a.md", "02-b.md"] {
            std::fs::write(dir.path().join("lessons").join(name), "# lesson\n").unwrap();
        }
        dir
    }

    const TWO: &str = r#"
[[lesson]]
id = "01-a"
file = "lessons/01-a.md"
checks = [
  { kind = "http", label = "root", path = "/", status = 200 },
  { kind = "tx", label = "worker", session = { tag = "tutor", state = ["idle", "working"] } },
]

[[lesson]]
id = "02-b"
file = "lessons/02-b.md"
checks = [
  { kind = "file", label = "server", path = "server.py", contains = "def" },
  { kind = "command", label = "tests", argv = ["python3", "-m", "unittest"] },
  { kind = "confirm", label = "moved", prompt = "Did you move?" },
]
"#;

    #[test]
    fn loads_a_valid_pack() {
        let dir = pack_dir(TWO);
        let pack = Pack::load("test", dir.path()).unwrap();
        assert_eq!(pack.meta.run, ["python3", "server.py"]);
        assert_eq!(pack.lessons.len(), 2);
        let Check::Http(http) = &pack.lessons[0].checks[0] else { panic!("not http") };
        assert_eq!((http.method.as_str(), http.path.as_str(), http.status), ("GET", "/", 200));
        let Check::Tx(tx) = &pack.lessons[0].checks[1] else { panic!("not tx") };
        assert_eq!(tx.session.tag.as_deref(), Some("tutor"));
        assert_eq!(tx.session.state, ["idle", "working"]);
        assert_eq!((tx.count, tx.absent), (1, false));
        assert_eq!(pack.lessons[1].checks[2].label(), "moved");
        assert_eq!(pack.lesson_path(1), dir.path().join("lessons/02-b.md"));
    }

    #[test]
    fn rejects_unknown_kind_and_unknown_field() {
        let dir = pack_dir(&TWO.replace("kind = \"confirm\"", "kind = \"telepathy\""));
        assert!(matches!(Pack::load("t", dir.path()), Err(PackError::Toml { .. })));
        let dir = pack_dir(&TWO.replace("contains = \"def\"", "matches = \"def\""));
        let error = Pack::load("t", dir.path()).unwrap_err().to_string();
        assert!(error.contains("matches"), "{error}");
    }

    #[test]
    fn rejects_missing_file_and_duplicate_id() {
        let dir = pack_dir(&TWO.replace("lessons/02-b.md", "lessons/99-x.md"));
        assert!(matches!(
            Pack::load("t", dir.path()),
            Err(PackError::MissingFile { lesson, .. }) if lesson == "02-b"
        ));
        let dir = pack_dir(&TWO.replace("id = \"02-b\"", "id = \"01-a\""));
        assert!(matches!(Pack::load("t", dir.path()), Err(PackError::DuplicateId(id)) if id == "01-a"));
    }

    #[test]
    fn resolves_numbers_ids_and_prefixes() {
        let dir = pack_dir(TWO);
        let pack = Pack::load("t", dir.path()).unwrap();
        assert_eq!(pack.resolve("1"), Some(0));
        assert_eq!(pack.resolve("2"), Some(1));
        assert_eq!(pack.resolve("3"), None);
        assert_eq!(pack.resolve("0"), None);
        assert_eq!(pack.resolve("02-b"), Some(1));
        assert_eq!(pack.resolve("01-"), Some(0));
        assert_eq!(pack.resolve("0-nope"), None);
    }

    #[test]
    fn find_names_available_packs_on_a_miss() {
        let root = tempfile::tempdir().unwrap();
        let dir = pack_dir(TWO);
        std::fs::create_dir_all(root.path().join("tutor")).unwrap();
        std::fs::rename(dir.path(), root.path().join("tutor/python")).unwrap();
        assert!(Pack::find(root.path(), "python").is_ok());
        let error = Pack::find(root.path(), "cobol").unwrap_err().to_string();
        assert_eq!(error, "no tutorial pack 'cobol' (available: python)");
    }
}
