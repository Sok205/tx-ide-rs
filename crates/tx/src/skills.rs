//! Skill resolution + per-worktree materialisation for `tx spawn` (skills.py).
//!
//! A skill is `agents/skills/<name>/SKILL.md`. Role files grant skills via frontmatter (see
//! `roles`); the resolved grant travels on the spawn environment ([`SKILLS_ENV`]) so the engine's
//! workspace preparation can symlink each skill into its per-worktree discovery directory.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::roles::{RoleError, parse_skill_grants, py_strip, read_text, resolve_role_files};
use crate::storage::Home;
use crate::worktree::WorktreeManager;

pub const SKILLS_ENV: &str = "TX_SKILLS";
pub const SKILLS_SUBDIR: &str = "skills";
pub const SKILL_FILE_NAME: &str = "SKILL.md";

/// Linking a skill grant into a workspace failed.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error(transparent)]
    Role(#[from] RoleError),
    #[error("cannot link skill {} -> {}: {source}", link.display(), target.display())]
    Link {
        link: PathBuf,
        target: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io_error(path: &Path) -> impl FnOnce(io::Error) -> SkillError + '_ {
    move |source| SkillError::Io {
        path: path.to_owned(),
        source,
    }
}

/// The directory backing a skill name — `user-agents/skills/<name>` replaces
/// `agents/skills/<name>` — validated against the engines' contract: a SKILL.md whose frontmatter
/// `name` equals the directory name and which carries a description.
pub fn resolve_skill_directory(home: &Home, name: &str) -> Result<PathBuf, RoleError> {
    let user_skills_dir = home.user_agents_dir().join(SKILLS_SUBDIR);
    let skills_dir = home.agents_dir().join(SKILLS_SUBDIR);
    let mut directory = user_skills_dir.join(name);
    if !directory.is_dir() {
        directory = skills_dir.join(name);
    }
    let skill_file = directory.join(SKILL_FILE_NAME);
    if !skill_file.is_file() {
        return Err(RoleError::UnknownSkill {
            name: name.to_owned(),
            user_skills_dir,
            skills_dir,
        });
    }
    let frontmatter = skill_frontmatter(&skill_file)?;
    let found = frontmatter.get("name").map(String::as_str);
    if found != Some(name) {
        return Err(RoleError::SkillNameMismatch {
            name: name.to_owned(),
            found: found.unwrap_or_default().to_owned(),
        });
    }
    if frontmatter
        .get("description")
        .is_none_or(|description| description.is_empty())
    {
        return Err(RoleError::SkillMissingDescription(name.to_owned()));
    }
    Ok(directory)
}

/// Top-level `key: value` pairs of a SKILL.md frontmatter block (later keys win).
fn skill_frontmatter(skill_file: &Path) -> Result<HashMap<String, String>, RoleError> {
    let text = read_text(skill_file).map_err(|source| RoleError::Read {
        path: skill_file.to_owned(),
        source,
    })?;
    let mut pairs = HashMap::new();
    if !text.starts_with("---\n") {
        return Ok(pairs);
    }
    for line in text.split('\n').skip(1) {
        if py_strip(line) == "---" {
            return Ok(pairs);
        }
        if let Some((key, value)) = line.split_once(':')
            && !key.starts_with([' ', '\t'])
        {
            pairs.insert(py_strip(key).to_owned(), py_strip(value).to_owned());
        }
    }
    Err(RoleError::UnclosedFrontmatter(skill_file.to_owned()))
}

/// The ordered skill grant for a `--role` list: the union of every resolved role file's
/// `skills:` list (COMMON first), deduplicated, each name validated.
pub fn resolve_role_skills(home: &Home, role_names: &[String]) -> Result<Vec<String>, RoleError> {
    let mut skills: Vec<String> = Vec::new();
    for path in resolve_role_files(home, role_names)? {
        for name in parse_skill_grants(&path)? {
            if !skills.contains(&name) {
                skills.push(name);
            }
        }
    }
    for name in &skills {
        resolve_skill_directory(home, name)?;
    }
    Ok(skills)
}

/// The skill names to materialise for a launch, from the launch env's [`SKILLS_ENV`] value.
/// Without the entry (a hand-written `--cmd` worker) the COMMON grant applies.
pub fn granted_skills(home: &Home, skills_env: Option<&str>) -> Result<Vec<String>, RoleError> {
    match skills_env {
        None => resolve_role_skills(home, &[]),
        Some(value) => Ok(value
            .split(',')
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()),
    }
}

/// Symlink each granted skill into `<cwd>/<engine_directory>/<name>` (`.claude/skills` for
/// claude, `.agents/skills` for codex/antigravity) and hide the dir from `git status`.
/// Idempotent: a correct link is kept, a stale one re-pointed.
pub fn link_skills(
    home: &Home,
    cwd: &Path,
    skills_env: Option<&str>,
    engine_directory: &str,
) -> Result<(), SkillError> {
    let names = granted_skills(home, skills_env)?;
    if names.is_empty() {
        return Ok(());
    }
    let target_root = cwd.join(engine_directory);
    std::fs::create_dir_all(&target_root).map_err(io_error(&target_root))?;
    for name in &names {
        let source = resolve_skill_directory(home, name)?;
        let link = target_root.join(name);
        if link.is_symlink() {
            if std::fs::read_link(&link).is_ok_and(|current| current == source) {
                continue;
            }
            std::fs::remove_file(&link).map_err(io_error(&link))?;
        }
        std::os::unix::fs::symlink(&source, &link).map_err(|error| SkillError::Link {
            link: link.clone(),
            target: source.clone(),
            source: error,
        })?;
    }
    exclude_from_git(home, cwd, &format!("{engine_directory}/"))
}

/// Hide a generated path from `git status` via the repository's shared `info/exclude` (a linked
/// worktree has none of its own). A non-git cwd has nothing to hide.
pub fn exclude_from_git(home: &Home, cwd: &Path, line: &str) -> Result<(), SkillError> {
    let Ok(common_directory) = WorktreeManager::new(home.worktrees_dir()).git_common_directory(cwd)
    else {
        return Ok(());
    };
    let info = common_directory.join("info");
    let exclude = info.join("exclude");
    let existing = if exclude.is_file() {
        read_text(&exclude).map_err(io_error(&exclude))?
    } else {
        String::new()
    };
    if py_splitlines(&existing).any(|existing_line| existing_line == line) {
        return Ok(());
    }
    if let Err(error) = std::fs::create_dir(&info)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(io_error(&info)(error));
    }
    let separator = if existing.ends_with('\n') || existing.is_empty() {
        ""
    } else {
        "\n"
    };
    std::fs::write(&exclude, format!("{existing}{separator}{line}\n")).map_err(io_error(&exclude))
}

/// `str.splitlines()`: split on every Python line boundary, no trailing empty piece.
fn py_splitlines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let boundary = rest.find(|c: char| {
            matches!(
                c,
                '\n' | '\r'
                    | '\x0b'
                    | '\x0c'
                    | '\x1c'
                    | '\x1d'
                    | '\x1e'
                    | '\u{85}'
                    | '\u{2028}'
                    | '\u{2029}'
            )
        });
        let Some(start) = boundary else {
            return Some(std::mem::take(&mut rest));
        };
        let line = &rest[..start];
        let after = &rest[start..];
        let width = if after.starts_with("\r\n") {
            2
        } else {
            after.chars().next().map_or(1, char::len_utf8)
        };
        rest = &after[width..];
        Some(line)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktree::tests::git_repo;
    use std::fs;

    struct Fixture {
        _dir: tempfile::TempDir,
        base: PathBuf,
        home: Home,
    }

    impl Fixture {
        fn new(common: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let base = fs::canonicalize(dir.path()).unwrap();
            let home = Home::new(base.join("home"));
            fs::create_dir_all(home.agents_dir().join(SKILLS_SUBDIR)).unwrap();
            fs::create_dir_all(home.user_agents_dir()).unwrap();
            fs::write(home.agents_dir().join("COMMON.md"), common).unwrap();
            Self {
                _dir: dir,
                base,
                home,
            }
        }
        fn skill(&self, root: &Path, name: &str, text: &str) {
            let dir = root.join(SKILLS_SUBDIR).join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(SKILL_FILE_NAME), text).unwrap();
        }
        fn shipped(&self, name: &str) {
            self.skill(
                &self.home.agents_dir(),
                name,
                &format!("---\nname: {name}\ndescription: d\n---\nbody\n"),
            );
        }
    }

    const COMMON: &str = "---\ntx:\n  skills: [a, b, a]\n---\n# Common\n";

    #[test]
    fn role_grant_and_env_grant() {
        let fx = Fixture::new(COMMON);
        fx.shipped("a");
        fx.shipped("b");
        assert_eq!(resolve_role_skills(&fx.home, &[]).unwrap(), ["a", "b"]);
        assert_eq!(granted_skills(&fx.home, None).unwrap(), ["a", "b"]);
        assert_eq!(granted_skills(&fx.home, Some(",x,,y")).unwrap(), ["x", "y"]);
        assert!(granted_skills(&fx.home, Some("")).unwrap().is_empty());
    }

    #[test]
    fn skill_validation_error_texts() {
        let fx = Fixture::new(COMMON);
        let agents = fx.home.agents_dir();
        let user_skills = fx.home.user_agents_dir().join(SKILLS_SUBDIR);
        let skills = agents.join(SKILLS_SUBDIR);
        fx.skill(&agents, "bad", "---\nname: other\ndescription: d\n---\n");
        fx.skill(&agents, "nodesc", "---\nname: nodesc\ndescription:\n---\n");
        fx.skill(&agents, "nofm", "name: nofm\n");
        fx.skill(
            &agents,
            "indented",
            "---\n name: indented\ndescription: d\n---\n",
        );
        fx.skill(&agents, "unclosed", "---\nname: unclosed\ndescription: d\n");
        let cases = [
            (
                "nofile",
                format!(
                    "unknown skill 'nofile' (no SKILL.md under {} or {})",
                    user_skills.display(),
                    skills.display()
                ),
            ),
            (
                "bad",
                "skill 'bad': frontmatter name 'other' must equal the directory name (the engines' discovery contract)".to_owned(),
            ),
            (
                "nodesc",
                "skill 'nodesc': frontmatter needs a description — it is the only part always in an agent's context, and the entire routing signal".to_owned(),
            ),
            (
                "nofm",
                "skill 'nofm': frontmatter name '' must equal the directory name (the engines' discovery contract)".to_owned(),
            ),
            (
                "indented",
                "skill 'indented': frontmatter name '' must equal the directory name (the engines' discovery contract)".to_owned(),
            ),
            (
                "unclosed",
                format!(
                    "{}: unclosed frontmatter block",
                    skills.join("unclosed/SKILL.md").display()
                ),
            ),
        ];
        for (name, message) in cases {
            assert_eq!(
                resolve_skill_directory(&fx.home, name)
                    .unwrap_err()
                    .to_string(),
                message
            );
        }
    }

    #[test]
    fn link_skills_symlinks_overrides_and_excludes() {
        let fx = Fixture::new(COMMON);
        fx.shipped("a");
        fx.shipped("b");
        let user_agents = fx.home.user_agents_dir();
        fx.skill(&user_agents, "a", "---\nname: a\ndescription: u\n---\n");
        let repo = git_repo(&fx.base, "repo");
        let exclude = repo.join(".git/info/exclude");
        fs::write(&exclude, "foo").unwrap();
        let skills = repo.join(".claude/skills");
        fs::create_dir_all(&skills).unwrap();
        std::os::unix::fs::symlink("/stale", skills.join("a")).unwrap();

        link_skills(&fx.home, &repo, None, ".claude/skills").unwrap();
        assert_eq!(
            fs::read_link(skills.join("a")).unwrap(),
            user_agents.join("skills/a")
        );
        assert_eq!(
            fs::read_link(skills.join("b")).unwrap(),
            fx.home.agents_dir().join("skills/b")
        );
        assert_eq!(
            fs::read_to_string(&exclude).unwrap(),
            "foo\n.claude/skills/\n"
        );
        link_skills(&fx.home, &repo, None, ".claude/skills").unwrap();
        assert_eq!(
            fs::read_to_string(&exclude).unwrap(),
            "foo\n.claude/skills/\n"
        );

        // Empty grant: nothing created, exclude untouched.
        link_skills(&fx.home, &repo, Some(""), ".agents/skills").unwrap();
        assert!(!repo.join(".agents").exists());

        // A regular directory in the link's place fails with the link path in the message.
        fs::create_dir_all(repo.join(".x/skills/b")).unwrap();
        let err = link_skills(&fx.home, &repo, Some("b"), ".x/skills").unwrap_err();
        assert!(err.to_string().contains(".x/skills/b"), "{err}");
    }

    #[test]
    fn exclude_rules() {
        let fx = Fixture::new(COMMON);
        let line = ".claude/skills/";
        for (label, before, expected) in [
            ("a", None, ".claude/skills/\n"),
            ("b", Some("foo\n"), "foo\n.claude/skills/\n"),
            ("c", Some("foo"), "foo\n.claude/skills/\n"),
            (
                "d",
                Some("foo\r\n.claude/skills/\rbar\n"),
                "foo\r\n.claude/skills/\rbar\n",
            ),
            ("e", Some("x\r\n"), "x\n.claude/skills/\n"),
        ] {
            let repo = git_repo(&fx.base, &format!("repo-{label}"));
            let info = repo.join(".git/info");
            let _ = fs::remove_dir_all(&info);
            if let Some(before) = before {
                fs::create_dir_all(&info).unwrap();
                fs::write(info.join("exclude"), before).unwrap();
            }
            exclude_from_git(&fx.home, &repo, line).unwrap();
            assert_eq!(
                fs::read_to_string(info.join("exclude")).unwrap(),
                expected,
                "{label}"
            );
        }
        // Not a git directory: nothing to hide.
        exclude_from_git(&fx.home, &fx.base, line).unwrap();
    }

    #[test]
    fn splitlines_matches_python() {
        let lines: Vec<&str> = py_splitlines("a\r\nb\rc\u{2028}d\n\ne\n").collect();
        assert_eq!(lines, ["a", "b", "c", "d", "", "e"]);
        assert_eq!(py_splitlines("").count(), 0);
    }
}
