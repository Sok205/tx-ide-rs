//! Role-file resolution for `tx spawn --role` (roles.py): names → concatenated system-prompt
//! contents, following COMMON.md's override semantics (`user-agents/NAME.md` replaces,
//! `NAME.local.md` extends). A role file may open with a frontmatter block granting skills:
//!
//! ```text
//! ---
//! tx:
//!   skills: [tx-sessions, tx-artifacts]
//! ---
//! ```
//!
//! The block is tx metadata: [`load_role_priming`] injects only the body.

use std::io;
use std::path::{Path, PathBuf};

use crate::storage::Home;

pub const COMMON_ROLE: &str = "COMMON";
pub const ROLE_SUFFIX: &str = ".md";
pub const LOCAL_SUFFIX: &str = ".local.md";
pub const FRONTMATTER_DELIMITER: &str = "---";

/// A bad role or skill grant — fail loudly, never guess. Shared with `skills` (the Python raises
/// `RoleError` from both, and the CLI maps it to an argparse error).
#[derive(Debug, thiserror::Error)]
pub enum RoleError {
    #[error("--role requires at least one role name")]
    EmptyRoleList,
    #[error("invalid role name '{0}' (must be a bare name, no path components)")]
    InvalidName(String),
    #[error("unknown role '{name}' (no {ROLE_SUFFIX} file under {} or {})", user_agents_dir.display(), agents_dir.display())]
    UnknownRole {
        name: String,
        user_agents_dir: PathBuf,
        agents_dir: PathBuf,
    },
    #[error("{}: unclosed frontmatter block", .0.display())]
    UnclosedFrontmatter(PathBuf),
    #[error("unknown skill '{name}' (no SKILL.md under {} or {})", user_skills_dir.display(), skills_dir.display())]
    UnknownSkill {
        name: String,
        user_skills_dir: PathBuf,
        skills_dir: PathBuf,
    },
    #[error(
        "skill '{name}': frontmatter name '{found}' must equal the directory name (the engines' discovery contract)"
    )]
    SkillNameMismatch { name: String, found: String },
    #[error(
        "skill '{0}': frontmatter needs a description — it is the only part always in an agent's context, and the entire routing signal"
    )]
    SkillMissingDescription(String),
    #[error("{}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// The ordered files backing `names`: COMMON auto-prepended, deduplicated, user override replaces
/// shipped, `.local.md` extends.
pub fn resolve_role_files(home: &Home, names: &[String]) -> Result<Vec<PathBuf>, RoleError> {
    let mut ordered: Vec<&str> = vec![COMMON_ROLE];
    for name in names {
        // A separator or dot-name would escape the role directories (`--role ../secret`).
        if !is_bare_name(name) {
            return Err(RoleError::InvalidName(name.clone()));
        }
        if !ordered.contains(&name.as_str()) {
            ordered.push(name);
        }
    }
    let user_agents_dir = home.user_agents_dir();
    let agents_dir = home.agents_dir();
    let mut files = Vec::new();
    for name in ordered {
        let file_name = format!("{name}{ROLE_SUFFIX}");
        let base = [
            user_agents_dir.join(&file_name),
            agents_dir.join(&file_name),
        ]
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| RoleError::UnknownRole {
            name: name.to_owned(),
            user_agents_dir: user_agents_dir.clone(),
            agents_dir: agents_dir.clone(),
        })?;
        files.push(base);
        let local = user_agents_dir.join(format!("{name}{LOCAL_SUFFIX}"));
        if local.is_file() {
            files.push(local);
        }
    }
    Ok(files)
}

/// The concatenated contents injected into the engine's system prompt (files carry their own `#`
/// titles). Frontmatter is tx metadata, never priming.
pub fn load_role_priming(home: &Home, names: &[String]) -> Result<String, RoleError> {
    let bodies = resolve_role_files(home, names)?
        .iter()
        .map(|path| Ok(py_strip(&split_frontmatter(path)?.1).to_owned()))
        .collect::<Result<Vec<_>, RoleError>>()?;
    Ok(bodies.join("\n\n"))
}

/// The skill names a role file grants: its frontmatter `skills: [a, b]` line; empty when the file
/// has no frontmatter or no grant.
pub fn parse_skill_grants(path: &Path) -> Result<Vec<String>, RoleError> {
    let (frontmatter, _) = split_frontmatter(path)?;
    Ok(frontmatter
        .iter()
        .find_map(|line| skills_list(line))
        .map(|list| {
            list.split(',')
                .map(py_strip)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default())
}

/// `^\s*skills:\s*\[(.*)\]\s*$` on one line (no newlines inside): the captured list text.
fn skills_list(line: &str) -> Option<&str> {
    let rest = line
        .trim_start_matches(py_is_space)
        .strip_prefix("skills:")?;
    let rest = rest.trim_start_matches(py_is_space).strip_prefix('[')?;
    rest.trim_end_matches(py_is_space).strip_suffix(']')
}

/// A role file's `(frontmatter lines, body)` — `([], whole text)` when it has none.
fn split_frontmatter(path: &Path) -> Result<(Vec<String>, String), RoleError> {
    let text = read_text(path).map_err(|source| RoleError::Read {
        path: path.to_owned(),
        source,
    })?;
    if !text.starts_with("---\n") {
        return Ok((Vec::new(), text));
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let close = (1..lines.len())
        .find(|&index| py_strip(lines[index]) == FRONTMATTER_DELIMITER)
        .ok_or_else(|| RoleError::UnclosedFrontmatter(path.to_owned()))?;
    let frontmatter = lines[1..close]
        .iter()
        .map(|line| (*line).to_owned())
        .collect();
    Ok((frontmatter, lines[close + 1..].join("\n")))
}

/// `Path(name).name == name and name not in (".", "..")`: no separator, not a dot-name. The empty
/// string passes (it then fails as an unknown role, as in the Python).
fn is_bare_name(name: &str) -> bool {
    !name.contains('/') && name != "." && name != ".."
}

/// `Path.read_text()`: UTF-8 with universal newlines (`\r\n` and `\r` read as `\n`).
pub(crate) fn read_text(path: &Path) -> io::Result<String> {
    let raw = std::fs::read_to_string(path)?;
    Ok(if raw.contains('\r') {
        raw.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        raw
    })
}

/// `str.isspace()` for one character (Unicode whitespace plus the `\x1c`–`\x1f` separators).
pub(crate) fn py_is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.strip()`.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_is_space)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture {
        _dir: tempfile::TempDir,
        home: Home,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = Home::new(dir.path());
            fs::create_dir_all(home.agents_dir()).unwrap();
            fs::create_dir_all(home.user_agents_dir()).unwrap();
            Self { _dir: dir, home }
        }
        fn agent(&self, name: &str, text: &str) -> PathBuf {
            let path = self.home.agents_dir().join(name);
            fs::write(&path, text).unwrap();
            path
        }
        fn user(&self, name: &str, text: &str) -> PathBuf {
            let path = self.home.user_agents_dir().join(name);
            fs::write(&path, text).unwrap();
            path
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn common_prepended_deduplicated_override_and_local() {
        let fx = Fixture::new();
        fx.agent("COMMON.md", "# Common\n");
        fx.agent("a.md", "shipped a");
        let user_a = fx.user("a.md", "  user a  \n");
        let local_a = fx.user("a.local.md", "local a\n");
        fx.agent("b.md", "---\ntx:\n  skills: [x]\n---\n\n# B\n");
        let files = resolve_role_files(&fx.home, &names(&["a", "COMMON", "b", "a"])).unwrap();
        assert_eq!(
            files,
            vec![
                fx.home.agents_dir().join("COMMON.md"),
                user_a,
                local_a,
                fx.home.agents_dir().join("b.md"),
            ]
        );
        // Python: "# Common\n\nuser a\n\nlocal a\n\n# B"
        assert_eq!(
            load_role_priming(&fx.home, &names(&["a", "b"])).unwrap(),
            "# Common\n\nuser a\n\nlocal a\n\n# B"
        );
    }

    #[test]
    fn error_texts() {
        let fx = Fixture::new();
        let err = resolve_role_files(&fx.home, &[]).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "unknown role 'COMMON' (no .md file under {} or {})",
                fx.home.user_agents_dir().display(),
                fx.home.agents_dir().display()
            )
        );
        for bad in ["../secret", "a/b", ".", "..", "x/"] {
            assert_eq!(
                resolve_role_files(&fx.home, &names(&[bad]))
                    .unwrap_err()
                    .to_string(),
                format!("invalid role name '{bad}' (must be a bare name, no path components)")
            );
        }
        let common = fx.agent("COMMON.md", "---\nskills: [a]\n");
        assert_eq!(
            load_role_priming(&fx.home, &[]).unwrap_err().to_string(),
            format!("{}: unclosed frontmatter block", common.display())
        );
    }

    #[test]
    fn skill_grants() {
        let fx = Fixture::new();
        let cases: [(&str, &[&str]); 7] = [
            (
                "---\ntx:\n  skills: [tx-sessions, tx-artifacts]\n---\nbody",
                &["tx-sessions", "tx-artifacts"],
            ),
            ("---\nskills: [ a ,, b , ]  \n---\n", &["a", "b"]),
            ("---\nskills: [a] x\n---\n", &[]),
            ("---\nskills: [a], [b]\n---\n", &["a]", "[b"]),
            ("---\r\nskills: [a]\r\n---\r\n", &["a"]),
            ("skills: [a]\n", &[]),
            ("---\n  --- \nskills: [a]\n", &[]),
        ];
        for (text, expected) in cases {
            let path = fx.agent("r.md", text);
            assert_eq!(
                parse_skill_grants(&path).unwrap(),
                names(expected),
                "{text:?}"
            );
        }
    }

    #[test]
    fn frontmatter_body_split() {
        let fx = Fixture::new();
        fx.agent("COMMON.md", "---\na\n---\n\n body \n\n");
        assert_eq!(load_role_priming(&fx.home, &[]).unwrap(), "body");
        fx.agent("COMMON.md", "--- \nnot frontmatter\n");
        assert_eq!(
            load_role_priming(&fx.home, &[]).unwrap(),
            "--- \nnot frontmatter"
        );
    }
}
