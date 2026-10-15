//! Profiles: a list of role names resolved against search dirs into one system-prompt text plus
//! the skill directories the roles grant. `<dir>/<NAME>.md` from the last dir that has it is the
//! base; every dir's `<NAME>.local.md` extends it, in dir order. A role file may open with a
//! frontmatter block whose `skills: [a, b]` line (at any indent, so `tx:` nesting works) grants
//! skills; each resolves to `<skill_dir>/<name>/` from the last skill dir that has it.

use std::io;
use std::path::{Path, PathBuf};

pub const ROLE_SUFFIX: &str = ".md";
pub const LOCAL_SUFFIX: &str = ".local.md";
pub const SKILL_FILE: &str = "SKILL.md";
const FRONTMATTER_DELIMITER: &str = "---";

/// A bad role or skill grant. Fail loudly, never guess.
#[derive(Debug, thiserror::Error)]
pub enum RoleError {
    #[error("invalid role or skill name {0:?} (must be a bare name: [A-Za-z0-9_-]+)")]
    InvalidName(String),
    #[error("unknown role {name:?} (no {name}{ROLE_SUFFIX} under {})", dirs(searched))]
    UnknownRole { name: String, searched: Vec<PathBuf> },
    #[error("unknown skill {name:?} (no {name}/{SKILL_FILE} under {})", dirs(searched))]
    UnknownSkill { name: String, searched: Vec<PathBuf> },
    #[error("{}: unclosed frontmatter block", .0.display())]
    UnclosedFrontmatter(PathBuf),
    #[error("{}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn dirs(searched: &[PathBuf]) -> String {
    if searched.is_empty() {
        return "no search dirs".to_owned();
    }
    searched
        .iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Role and skill search dirs, in order; a later dir overrides an earlier one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profiles {
    profile_dirs: Vec<PathBuf>,
    skill_dirs: Vec<PathBuf>,
}

/// A resolved profile: the combined text and the granted skill directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub text: String,
    pub skill_dirs: Vec<PathBuf>,
}

/// What an engine adapter receives: the text, a file holding it, and the skill dirs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    text: String,
    file: PathBuf,
    skill_dirs: Vec<PathBuf>,
}

impl Profile {
    /// Writes `resolved.text` to `path` (creating parents) and returns the profile.
    pub fn write(resolved: ResolvedProfile, path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &resolved.text)?;
        Ok(Self {
            text: resolved.text,
            file: path.to_owned(),
            skill_dirs: resolved.skill_dirs,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    pub fn skill_dirs(&self) -> &[PathBuf] {
        &self.skill_dirs
    }
}

impl Profiles {
    pub fn new(profile_dirs: Vec<PathBuf>, skill_dirs: Vec<PathBuf>) -> Self {
        Self {
            profile_dirs,
            skill_dirs,
        }
    }

    /// Resolves `roles` (deduplicated, first position kept) into one text and the skill dirs
    /// they grant (deduplicated, in grant order).
    pub fn resolve(&self, roles: &[String]) -> Result<ResolvedProfile, RoleError> {
        let mut ordered: Vec<&str> = Vec::new();
        for role in roles {
            if !is_bare_name(role) {
                return Err(RoleError::InvalidName(role.clone()));
            }
            if !ordered.contains(&role.as_str()) {
                ordered.push(role);
            }
        }
        let mut bodies = Vec::new();
        let mut skills: Vec<String> = Vec::new();
        for role in ordered {
            for path in self.role_files(role)? {
                let (frontmatter, body) = split_frontmatter(&path)?;
                let body = body.trim();
                if !body.is_empty() {
                    bodies.push(body.to_owned());
                }
                let lines: Vec<&str> = frontmatter.iter().map(String::as_str).collect();
                for skill in skills_in(&lines) {
                    if !skills.contains(&skill) {
                        skills.push(skill);
                    }
                }
            }
        }
        let skill_dirs = skills
            .iter()
            .map(|skill| self.skill_dir(skill))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ResolvedProfile {
            text: bodies.join("\n\n"),
            skill_dirs,
        })
    }

    /// The base file from the last dir holding `<role>.md`, then every dir's `<role>.local.md`.
    fn role_files(&self, role: &str) -> Result<Vec<PathBuf>, RoleError> {
        let base_name = format!("{role}{ROLE_SUFFIX}");
        let base = self
            .profile_dirs
            .iter()
            .rev()
            .map(|dir| dir.join(&base_name))
            .find(|path| path.is_file())
            .ok_or_else(|| RoleError::UnknownRole {
                name: role.to_owned(),
                searched: self.profile_dirs.clone(),
            })?;
        let local_name = format!("{role}{LOCAL_SUFFIX}");
        let mut files = vec![base];
        files.extend(
            self.profile_dirs
                .iter()
                .map(|dir| dir.join(&local_name))
                .filter(|path| path.is_file()),
        );
        Ok(files)
    }

    /// `<skill_dir>/<name>/` from the last skill dir holding a `SKILL.md` for it.
    fn skill_dir(&self, name: &str) -> Result<PathBuf, RoleError> {
        if !is_bare_name(name) {
            return Err(RoleError::InvalidName(name.to_owned()));
        }
        self.skill_dirs
            .iter()
            .rev()
            .map(|dir| dir.join(name))
            .find(|dir| dir.join(SKILL_FILE).is_file())
            .ok_or_else(|| RoleError::UnknownSkill {
                name: name.to_owned(),
                searched: self.skill_dirs.clone(),
            })
    }
}

/// Role and skill names are path fragments; the session-name grammar keeps them inside the
/// search dirs (`../x`, `a/b`, `.`, `..` are all refused).
fn is_bare_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A role file's `(frontmatter lines, body)`; `([], whole text)` when it has none. Universal
/// newlines (`\r\n` and `\r` read as `\n`), a leading BOM ignored, and the opener is a line
/// that trims to `---`, like the closer.
fn split_frontmatter(path: &Path) -> Result<(Vec<String>, String), RoleError> {
    let raw = std::fs::read_to_string(path).map_err(|source| RoleError::Read {
        path: path.to_owned(),
        source,
    })?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let text = if raw.contains('\r') {
        raw.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        raw.to_owned()
    };
    let lines: Vec<&str> = text.split('\n').collect();
    if lines[0].trim() != FRONTMATTER_DELIMITER {
        return Ok((Vec::new(), text));
    }
    let close = (1..lines.len())
        .find(|&index| lines[index].trim() == FRONTMATTER_DELIMITER)
        .ok_or_else(|| RoleError::UnclosedFrontmatter(path.to_owned()))?;
    let frontmatter = lines[1..close].iter().map(|line| (*line).to_owned()).collect();
    Ok((frontmatter, lines[close + 1..].join("\n")))
}

/// Every `skills: [a, b]` line in a frontmatter block, at any indent, in order.
pub(crate) fn skills_in(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("skills:")?;
            let rest = rest.trim_start().strip_prefix('[')?;
            rest.trim_end().strip_suffix(']')
        })
        .flat_map(|list| list.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Tree {
        dir: tempfile::TempDir,
    }

    impl Tree {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn write(&self, rel: &str, text: &str) -> PathBuf {
            let path = self.dir.path().join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            path
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.dir.path().join(rel)
        }

        fn profiles(&self, roles: &[&str], skills: &[&str]) -> Profiles {
            Profiles::new(
                roles.iter().map(|d| self.path(d)).collect(),
                skills.iter().map(|d| self.path(d)).collect(),
            )
        }
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn single_dir_concatenates_in_order() {
        let tree = Tree::new();
        tree.write("roles/COMMON.md", "# Common\n\nbe nice\n");
        tree.write("roles/dev.md", "# Dev\n\nwrite code\n");
        let resolved = tree
            .profiles(&["roles"], &[])
            .resolve(&names(&["COMMON", "dev"]))
            .unwrap();
        assert_eq!(resolved.text, "# Common\n\nbe nice\n\n# Dev\n\nwrite code");
        assert!(resolved.skill_dirs.is_empty());
    }

    #[test]
    fn later_dir_replaces_base_and_every_local_extends() {
        let tree = Tree::new();
        tree.write("shipped/dev.md", "shipped");
        tree.write("shipped/dev.local.md", "shipped-local");
        tree.write("user/dev.md", "user");
        tree.write("user/dev.local.md", "user-local");
        let resolved = tree
            .profiles(&["shipped", "user"], &[])
            .resolve(&names(&["dev"]))
            .unwrap();
        assert_eq!(resolved.text, "user\n\nshipped-local\n\nuser-local");
    }

    #[test]
    fn local_only_in_an_earlier_dir_still_extends() {
        let tree = Tree::new();
        tree.write("a/dev.md", "a");
        tree.write("a/dev.local.md", "a-local");
        tree.write("b/other.md", "unrelated");
        let resolved = tree
            .profiles(&["a", "b"], &[])
            .resolve(&names(&["dev"]))
            .unwrap();
        assert_eq!(resolved.text, "a\n\na-local");
    }

    #[test]
    fn duplicate_roles_keep_their_first_position() {
        let tree = Tree::new();
        tree.write("r/a.md", "A");
        tree.write("r/b.md", "B");
        let resolved = tree
            .profiles(&["r"], &[])
            .resolve(&names(&["a", "b", "a"]))
            .unwrap();
        assert_eq!(resolved.text, "A\n\nB");
    }

    #[test]
    fn frontmatter_is_stripped_and_grants_skills() {
        let tree = Tree::new();
        tree.write(
            "r/a.md",
            "---\ntx:\n  skills: [one, two]\n---\n# A\nbody\n",
        );
        tree.write("r/b.md", "---\nskills: [two, three]\n---\nB");
        tree.write("skills/one/SKILL.md", "---\nname: one\ndescription: d\n---\n");
        tree.write("skills/two/SKILL.md", "x");
        tree.write("skills/three/SKILL.md", "x");
        tree.write("user-skills/two/SKILL.md", "override");
        let resolved = tree
            .profiles(&["r"], &["skills", "user-skills"])
            .resolve(&names(&["a", "b"]))
            .unwrap();
        assert_eq!(resolved.text, "# A\nbody\n\nB");
        assert_eq!(
            resolved.skill_dirs,
            vec![
                tree.path("skills/one"),
                tree.path("user-skills/two"),
                tree.path("skills/three")
            ]
        );
    }

    #[test]
    fn unknown_skill_lists_searched_dirs() {
        let tree = Tree::new();
        tree.write("r/a.md", "---\nskills: [ghost]\n---\nA");
        let error = tree
            .profiles(&["r"], &["s1", "s2"])
            .resolve(&names(&["a"]))
            .unwrap_err();
        assert!(
            matches!(&error, RoleError::UnknownSkill { name, searched } if name == "ghost" && *searched == vec![tree.path("s1"), tree.path("s2")]),
            "{error:?}"
        );
        assert!(error.to_string().contains("ghost"), "{error}");
    }

    #[test]
    fn skill_dir_must_hold_a_skill_file() {
        let tree = Tree::new();
        tree.write("r/a.md", "---\nskills: [bare]\n---\nA");
        std::fs::create_dir_all(tree.path("skills/bare")).unwrap();
        let error = tree
            .profiles(&["r"], &["skills"])
            .resolve(&names(&["a"]))
            .unwrap_err();
        assert!(matches!(error, RoleError::UnknownSkill { .. }), "{error:?}");
    }

    #[test]
    fn unknown_role_lists_searched_dirs() {
        let tree = Tree::new();
        let error = tree
            .profiles(&["r1", "r2"], &[])
            .resolve(&names(&["nope"]))
            .unwrap_err();
        assert!(
            matches!(&error, RoleError::UnknownRole { name, searched } if name == "nope" && *searched == vec![tree.path("r1"), tree.path("r2")]),
            "{error:?}"
        );
    }

    #[test]
    fn unclosed_frontmatter_is_an_error() {
        let tree = Tree::new();
        let path = tree.write("r/a.md", "---\nskills: [x]\nno close");
        let error = tree
            .profiles(&["r"], &[])
            .resolve(&names(&["a"]))
            .unwrap_err();
        assert!(matches!(&error, RoleError::UnclosedFrontmatter(p) if *p == path), "{error:?}");
    }

    #[test]
    fn role_names_must_be_bare() {
        let tree = Tree::new();
        tree.write("r/a.md", "A");
        for bad in ["../a", "a/b", ".", "..", "", "a b", "ä", "a.local", "a.md"] {
            let error = tree
                .profiles(&["r"], &[])
                .resolve(&names(&[bad]))
                .unwrap_err();
            assert!(matches!(&error, RoleError::InvalidName(n) if n == bad), "{bad:?}: {error:?}");
        }
    }

    #[test]
    fn empty_role_list_is_an_empty_profile() {
        let tree = Tree::new();
        let resolved = tree.profiles(&["r"], &[]).resolve(&[]).unwrap();
        assert_eq!(resolved.text, "");
        assert!(resolved.skill_dirs.is_empty());
    }

    #[test]
    fn crlf_files_read_as_lf_and_bodies_are_trimmed() {
        let tree = Tree::new();
        tree.write("r/a.md", "---\r\nskills: [ ]\r\n---\r\n\r\n  A line\r\n\r\n");
        let resolved = tree.profiles(&["r"], &[]).resolve(&names(&["a"])).unwrap();
        assert_eq!(resolved.text, "A line");
    }

    #[test]
    fn unreadable_role_file_is_a_read_error() {
        use std::os::unix::fs::PermissionsExt;
        let tree = Tree::new();
        let path = tree.write("r/a.md", "A");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = tree.profiles(&["r"], &[]).resolve(&names(&["a"]));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        if std::fs::read_to_string("/etc/master.passwd").is_ok() {
            return; // root reads anything; nothing to assert
        }
        let error = result.unwrap_err();
        assert!(matches!(&error, RoleError::Read { path: p, .. } if *p == path), "{error:?}");
    }

    #[test]
    fn a_directory_named_like_a_role_is_not_a_role() {
        let tree = Tree::new();
        std::fs::create_dir_all(tree.path("r/a.md")).unwrap();
        let error = tree.profiles(&["r"], &[]).resolve(&names(&["a"])).unwrap_err();
        assert!(matches!(error, RoleError::UnknownRole { .. }), "{error:?}");
    }

    #[test]
    fn profile_write_creates_the_file_and_parent() {
        let tree = Tree::new();
        let resolved = ResolvedProfile {
            text: "hello".into(),
            skill_dirs: vec![tree.path("s/one")],
        };
        let path = tree.path("profiles/abc.md");
        let profile = Profile::write(resolved, &path).unwrap();
        assert_eq!(profile.file(), path);
        assert_eq!(profile.text(), "hello");
        assert_eq!(profile.skill_dirs(), [tree.path("s/one")]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn bom_and_opener_whitespace_do_not_hide_frontmatter() {
        let tree = Tree::new();
        tree.write("s/a/SKILL.md", "");
        tree.write("r/bom.md", "\u{feff}---\nskills: [a]\n---\nbody");
        tree.write("r/sp.md", "--- \nskills: [a]\n---\nbody");
        tree.write("r/bare.md", "---");
        let profiles = tree.profiles(&["r"], &["s"]);
        for role in ["bom", "sp"] {
            let resolved = profiles.resolve(&names(&[role])).unwrap();
            assert_eq!(resolved.text, "body", "{role}");
            assert_eq!(resolved.skill_dirs, [tree.path("s/a")], "{role}");
        }
        assert!(matches!(
            profiles.resolve(&names(&["bare"])).unwrap_err(),
            RoleError::UnclosedFrontmatter(_)
        ));
    }

    #[test]
    fn empty_bodies_leave_no_gaps() {
        let tree = Tree::new();
        tree.write("r/a.md", "A");
        tree.write("r/e.md", "");
        tree.write("r/b.md", "B");
        tree.write("r/b.local.md", "---\nskills: []\n---\n");
        let profiles = tree.profiles(&["r"], &[]);
        assert_eq!(profiles.resolve(&names(&["a", "e", "b"])).unwrap().text, "A\n\nB");
        assert_eq!(profiles.resolve(&names(&["e", "a"])).unwrap().text, "A");
        assert_eq!(profiles.resolve(&names(&["b"])).unwrap().text, "B");
        assert_eq!(profiles.resolve(&names(&["e"])).unwrap().text, "");
    }

    #[test]
    fn skills_line_shapes() {
        assert_eq!(skills_in(&["skills: [a, b]"]), ["a", "b"]);
        assert_eq!(skills_in(&["tx:", "  skills: [ a ,b ,, ]"]), ["a", "b"]);
        assert_eq!(skills_in(&["skills: []"]), Vec::<String>::new());
        assert_eq!(skills_in(&["skillset: [a]", "other: 1"]), Vec::<String>::new());
        assert_eq!(skills_in(&["skills: [a]", "skills: [b]"]), ["a", "b"]);
        assert_eq!(skills_in(&["skills:[a]"]), ["a"]);
        assert_eq!(skills_in(&["skills: [a] # c"]), Vec::<String>::new());
    }

    #[test]
    fn skill_names_must_be_bare_too() {
        let tree = Tree::new();
        tree.write("r/a.md", "---\nskills: [../evil]\n---\nA");
        let error = tree.profiles(&["r"], &["s"]).resolve(&names(&["a"])).unwrap_err();
        assert!(matches!(&error, RoleError::InvalidName(n) if n == "../evil"), "{error:?}");
    }
}
