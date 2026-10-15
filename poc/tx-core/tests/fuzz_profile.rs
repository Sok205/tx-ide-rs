//! Property-based and adversarial tests for `Profiles` / `Profile`.

use std::path::{Path, PathBuf};

use proptest::prelude::*;
use tx_core::{Profile, Profiles, ResolvedProfile, RoleError};

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
        let path = self.path(rel);
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

fn resolve_one(tree: &Tree, text: &str) -> Result<ResolvedProfile, RoleError> {
    tree.write("r/a.md", text);
    tree.profiles(&["r"], &["s"]).resolve(&names(&["a"]))
}

fn is_invalid(err: &RoleError, name: &str) -> bool {
    matches!(err, RoleError::InvalidName(n) if n == name)
}

// ---- 1. model-based ----------------------------------------------------------------------------

const ROLES: [&str; 3] = ["a", "b", "c"];
const SKILLS: [&str; 2] = ["s1", "s2"];

#[derive(Clone, Debug)]
struct RoleFile {
    body: String,
    skills: [bool; 2],
    nested: bool,
}

impl RoleFile {
    fn skill_names(&self) -> Vec<String> {
        SKILLS
            .iter()
            .zip(self.skills)
            .filter(|(_, on)| *on)
            .map(|(s, _)| (*s).to_owned())
            .collect()
    }

    fn render(&self) -> String {
        let mut out = String::new();
        let skills = self.skill_names();
        if !skills.is_empty() || self.nested {
            out.push_str("---\n");
            if self.nested {
                out.push_str("tx:\n  ");
            }
            out.push_str(&format!("skills: [{}]\n", skills.join(", ")));
            out.push_str("---\n");
        }
        out.push_str(&format!("\n  {}  \n\n", self.body));
        out
    }
}

#[derive(Clone, Debug)]
struct ProfileDir {
    /// `[role][0]` = `<role>.md`, `[role][1]` = `<role>.local.md`.
    files: [[Option<RoleFile>; 2]; 3],
}

fn role_file(tag: String) -> impl Strategy<Value = RoleFile> {
    ("[a-z]{1,8}", any::<[bool; 2]>(), any::<bool>()).prop_map(move |(body, skills, nested)| RoleFile {
        body: format!("{tag}-{body}"),
        skills,
        nested,
    })
}

fn profile_dir(index: usize) -> impl Strategy<Value = ProfileDir> {
    let slot = |role: &'static str, kind: &'static str| prop::option::of(role_file(format!("d{index}-{role}-{kind}")));
    (
        (slot("a", "md"), slot("a", "local")),
        (slot("b", "md"), slot("b", "local")),
        (slot("c", "md"), slot("c", "local")),
    )
        .prop_map(|(a, b, c)| ProfileDir {
            files: [[a.0, a.1], [b.0, b.1], [c.0, c.1]],
        })
}

fn profile_dirs() -> impl Strategy<Value = Vec<ProfileDir>> {
    prop_oneof![
        profile_dir(0).prop_map(|d| vec![d]),
        (profile_dir(0), profile_dir(1)).prop_map(|(a, b)| vec![a, b]),
        (profile_dir(0), profile_dir(1), profile_dir(2)).prop_map(|(a, b, c)| vec![a, b, c]),
    ]
}

fn skill_dirs() -> impl Strategy<Value = Vec<[bool; 2]>> {
    prop::collection::vec(any::<[bool; 2]>(), 0..=2)
}

fn role_list() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(
        prop::sample::select(vec!["a", "b", "c", "zz", "bad name", "c"]).prop_map(str::to_owned),
        0..=6,
    )
}

#[derive(Debug, PartialEq)]
enum Expected {
    Ok { text: String, skill_dirs: Vec<PathBuf> },
    Invalid(String),
    UnknownRole(String),
    UnknownSkill(String),
}

fn model(tree: &Tree, dirs: &[ProfileDir], skill_dirs: &[[bool; 2]], roles: &[String]) -> Expected {
    if let Some(bad) = roles.iter().find(|r| r.contains(' ')) {
        return Expected::Invalid(bad.clone());
    }
    let mut ordered: Vec<&String> = Vec::new();
    for role in roles {
        if !ordered.contains(&role) {
            ordered.push(role);
        }
    }
    let mut bodies = Vec::new();
    let mut skills: Vec<String> = Vec::new();
    for role in ordered {
        let Some(ri) = ROLES.iter().position(|r| r == role) else {
            return Expected::UnknownRole(role.clone());
        };
        let Some(base) = dirs.iter().rev().find_map(|d| d.files[ri][0].as_ref()) else {
            return Expected::UnknownRole(role.clone());
        };
        let locals = dirs.iter().filter_map(|d| d.files[ri][1].as_ref());
        for file in std::iter::once(base).chain(locals) {
            bodies.push(file.body.clone());
            for s in file.skill_names() {
                if !skills.contains(&s) {
                    skills.push(s);
                }
            }
        }
    }
    let mut out = Vec::new();
    for skill in skills {
        let si = SKILLS.iter().position(|s| *s == skill).unwrap();
        let Some(di) = (0..skill_dirs.len()).rev().find(|&di| skill_dirs[di][si]) else {
            return Expected::UnknownSkill(skill);
        };
        out.push(tree.path(&format!("s{di}/{skill}")));
    }
    Expected::Ok {
        text: bodies.join("\n\n"),
        skill_dirs: out,
    }
}

fn observed(result: Result<ResolvedProfile, RoleError>) -> Expected {
    match result {
        Ok(r) => Expected::Ok {
            text: r.text,
            skill_dirs: r.skill_dirs,
        },
        Err(RoleError::InvalidName(n)) => Expected::Invalid(n),
        Err(RoleError::UnknownRole { name, .. }) => Expected::UnknownRole(name),
        Err(RoleError::UnknownSkill { name, .. }) => Expected::UnknownSkill(name),
        Err(other) => panic!("unexpected error {other:?}"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn resolve_matches_reference_model(
        dirs in profile_dirs(),
        skill_dirs in skill_dirs(),
        role_lists in prop::collection::vec(role_list(), 1..=4),
    ) {
        let tree = Tree::new();
        let mut pdirs = Vec::new();
        for (di, dir) in dirs.iter().enumerate() {
            let name = format!("p{di}");
            std::fs::create_dir_all(tree.path(&name)).unwrap();
            for (ri, role) in ROLES.iter().enumerate() {
                if let Some(f) = &dir.files[ri][0] {
                    tree.write(&format!("{name}/{role}.md"), &f.render());
                }
                if let Some(f) = &dir.files[ri][1] {
                    tree.write(&format!("{name}/{role}.local.md"), &f.render());
                }
            }
            pdirs.push(name);
        }
        let mut sdirs = Vec::new();
        for (di, present) in skill_dirs.iter().enumerate() {
            let name = format!("s{di}");
            std::fs::create_dir_all(tree.path(&name)).unwrap();
            for (si, skill) in SKILLS.iter().enumerate() {
                if present[si] {
                    tree.write(&format!("{name}/{skill}/SKILL.md"), "x");
                }
            }
            sdirs.push(name);
        }
        let pd: Vec<&str> = pdirs.iter().map(String::as_str).collect();
        let sd: Vec<&str> = sdirs.iter().map(String::as_str).collect();
        let profiles = tree.profiles(&pd, &sd);
        for roles in &role_lists {
            let expected = model(&tree, &dirs, &skill_dirs, roles);
            let got = observed(profiles.resolve(roles));
            prop_assert_eq!(&got, &expected, "roles {:?}", roles);
            // resolve is pure: same answer twice.
            prop_assert_eq!(observed(profiles.resolve(roles)), expected);
        }
    }

    #[test]
    fn bare_names_are_accepted_and_others_refused(name in "[A-Za-z0-9_-]{1,32}", bad in "[^A-Za-z0-9_-]{1,8}") {
        let tree = Tree::new();
        let profiles = tree.profiles(&["missing"], &["missing"]);
        let ok = matches!(profiles.resolve(std::slice::from_ref(&name)), Err(RoleError::UnknownRole { .. }));
        prop_assert!(ok, "{:?} should be a bare name", name);
        let with_bad = format!("{name}{bad}");
        let err = profiles.resolve(std::slice::from_ref(&with_bad)).unwrap_err();
        let invalid = is_invalid(&err, &with_bad);
        prop_assert!(invalid, "{:?}: {:?}", with_bad, err);
    }

    #[test]
    fn frontmatter_round_trip(
        body in "[ -~\n]{0,64}",
        skills in prop::collection::vec("[a-z0-9]{1,4}", 0..=3),
        indent in "[ \t]{0,4}",
    ) {
        let tree = Tree::new();
        for s in &skills {
            tree.write(&format!("s/{s}/SKILL.md"), "");
        }
        let text = format!("---\n{indent}skills: [{}]\n---\n{body}", skills.join(","));
        let resolved = resolve_one(&tree, &text).unwrap();
        prop_assert_eq!(resolved.text, body.trim());
        let mut want: Vec<PathBuf> = Vec::new();
        for s in &skills {
            let p = tree.path(&format!("s/{s}"));
            if !want.contains(&p) {
                want.push(p);
            }
        }
        prop_assert_eq!(resolved.skill_dirs, want);
    }
}

// ---- 2. frontmatter edge cases -----------------------------------------------------------------

#[test]
fn file_of_only_an_opener_is_unclosed() {
    let tree = Tree::new();
    let err = resolve_one(&tree, "---\n").unwrap_err();
    assert!(matches!(err, RoleError::UnclosedFrontmatter(_)), "{err:?}");
}

#[test]
fn opener_without_newline_is_unclosed_frontmatter() {
    let tree = Tree::new();
    let err = resolve_one(&tree, "---").unwrap_err();
    assert!(matches!(err, RoleError::UnclosedFrontmatter(_)), "{err:?}");
}

#[test]
fn opener_with_trailing_space_is_frontmatter() {
    let tree = Tree::new();
    tree.write("s/s1/SKILL.md", "");
    let r = resolve_one(&tree, "--- \nskills: [s1]\n---\nbody").unwrap();
    assert_eq!(r.text, "body");
    assert_eq!(r.skill_dirs, vec![tree.path("s/s1")]);
}

#[test]
fn empty_frontmatter_yields_empty_text() {
    let tree = Tree::new();
    let r = resolve_one(&tree, "---\n---\n").unwrap();
    assert_eq!(r.text, "");
    assert!(r.skill_dirs.is_empty());
}

#[test]
fn closer_with_trailing_spaces_closes() {
    let tree = Tree::new();
    assert_eq!(resolve_one(&tree, "---\nx: 1\n---   \nbody\n").unwrap().text, "body");
    assert_eq!(resolve_one(&tree, "---\nx: 1\n\t---\t\nbody\n").unwrap().text, "body");
}

#[test]
fn four_dashes_do_not_close() {
    let tree = Tree::new();
    let err = resolve_one(&tree, "---\nx: 1\n----\nbody\n").unwrap_err();
    assert!(matches!(err, RoleError::UnclosedFrontmatter(_)), "{err:?}");
    assert_eq!(resolve_one(&tree, "---\nx: 1\n----\n---\nbody\n").unwrap().text, "body");
}

#[test]
fn flat_and_nested_skills_lines_both_grant() {
    let tree = Tree::new();
    tree.write("s/a/SKILL.md", "");
    tree.write("s/b/SKILL.md", "");
    let r = resolve_one(&tree, "---\nskills: [a]\ntx:\n  skills: [b]\n---\nbody").unwrap();
    assert_eq!(r.skill_dirs, vec![tree.path("s/a"), tree.path("s/b")]);
}

#[test]
fn crlf_skills_line_grants() {
    let tree = Tree::new();
    tree.write("s/a/SKILL.md", "");
    tree.write("s/b/SKILL.md", "");
    let r = resolve_one(&tree, "---\r\nskills: [a, b]\r\n---\r\nbody\r\n").unwrap();
    assert_eq!(r.skill_dirs, vec![tree.path("s/a"), tree.path("s/b")]);
    assert_eq!(r.text, "body");
    // lone CR too
    let r = resolve_one(&tree, "---\rskills: [a]\r---\rbody\r").unwrap();
    assert_eq!(r.skill_dirs, vec![tree.path("s/a")]);
    assert_eq!(r.text, "body");
}

#[test]
fn bom_before_frontmatter_is_still_frontmatter() {
    let tree = Tree::new();
    tree.write("s/a/SKILL.md", "");
    let r = resolve_one(&tree, "\u{feff}---\nskills: [a]\n---\nbody").unwrap();
    assert_eq!(r.text, "body");
    assert_eq!(r.skill_dirs, vec![tree.path("s/a")]);
}

#[test]
fn body_may_contain_delimiter_lines() {
    let tree = Tree::new();
    assert_eq!(resolve_one(&tree, "---\n---\nfoo\n---\nbar\n---\n").unwrap().text, "foo\n---\nbar\n---");
    assert_eq!(resolve_one(&tree, "body\n---\nmore").unwrap().text, "body\n---\nmore");
}

#[test]
fn no_trailing_newline() {
    let tree = Tree::new();
    assert_eq!(resolve_one(&tree, "---\n---\nbody").unwrap().text, "body");
    assert_eq!(resolve_one(&tree, "---\nx\n---").unwrap().text, "");
    assert_eq!(resolve_one(&tree, "body").unwrap().text, "body");
}

#[test]
fn whitespace_only_file_is_empty_text() {
    let tree = Tree::new();
    assert_eq!(resolve_one(&tree, " \n\t\n  \r\n").unwrap().text, "");
    assert_eq!(resolve_one(&tree, "").unwrap().text, "");
}

#[test]
fn one_megabyte_body_round_trips() {
    let tree = Tree::new();
    let body: String = "x".repeat(1 << 20);
    let text = format!("---\nskills: []\n---\n{body}\n");
    let r = resolve_one(&tree, &text).unwrap();
    assert_eq!(r.text.len(), 1 << 20);
    assert_eq!(r.text, body);
}

#[test]
fn skills_line_with_comment_or_nested_brackets_is_ignored_or_refused() {
    let tree = Tree::new();
    tree.write("s/a/SKILL.md", "");
    // trailing comment: whole line ignored (no `]` at end)
    assert!(resolve_one(&tree, "---\nskills: [a] # c\n---\nb").unwrap().skill_dirs.is_empty());
    // nested bracket yields the name "[a]" which is refused
    let err = resolve_one(&tree, "---\nskills: [[a]]\n---\nb").unwrap_err();
    assert!(is_invalid(&err, "[a]"), "{err:?}");
    // quoted names are not unquoted
    let err = resolve_one(&tree, "---\nskills: [\"a\"]\n---\nb").unwrap_err();
    assert!(is_invalid(&err, "\"a\""), "{err:?}");
    // `skills:` inside the body is not a grant
    assert!(resolve_one(&tree, "---\n---\nskills: [a]").unwrap().skill_dirs.is_empty());
}

#[test]
fn skills_are_deduplicated_across_files_and_case_sensitive() {
    let tree = Tree::new();
    tree.write("s/a/SKILL.md", "");
    tree.write("s/A/SKILL.md", "");
    tree.write("r/a.md", "---\nskills: [a, a]\n---\nA");
    tree.write("r/a.local.md", "---\nskills: [A, a]\n---\nL");
    let r = tree.profiles(&["r"], &["s"]).resolve(&names(&["a"])).unwrap();
    // On a case-insensitive filesystem (macOS default) `s/a` and `s/A` are the same dir; only the
    // order and dedup of names is asserted here.
    assert_eq!(r.skill_dirs.len(), 2);
    assert_eq!(r.skill_dirs[0], tree.path("s/a"));
    assert_eq!(r.skill_dirs[1], tree.path("s/A"));
}

// ---- 3. names ----------------------------------------------------------------------------------

#[test]
fn invalid_role_name_wins_over_unknown_role_without_touching_the_fs() {
    let tree = Tree::new();
    let missing = tree.path("does-not-exist");
    let profiles = Profiles::new(vec![missing.clone()], vec![missing]);
    for bad in ["", " ", "a b", "ä", "日本", "a.b", "a/b", "/a", "a\\b", "a\0", "\0", "..", "a\n", "a-", "-"] {
        let result = profiles.resolve(&names(&[bad]));
        if bad == "a-" || bad == "-" {
            assert!(matches!(result, Err(RoleError::UnknownRole { .. })), "{bad:?}: {result:?}");
            continue;
        }
        let err = result.unwrap_err();
        assert!(is_invalid(&err, bad), "{bad:?}: {err:?}");
    }
    // invalid later in the list still wins over an unknown-but-valid earlier one
    let err = profiles.resolve(&names(&["nope", "a/b"])).unwrap_err();
    assert!(is_invalid(&err, "a/b"), "{err:?}");
    // and over a real role
    tree.write("r/a.md", "A");
    let err = tree.profiles(&["r"], &[]).resolve(&names(&["a", "a/b"])).unwrap_err();
    assert!(is_invalid(&err, "a/b"), "{err:?}");
}

#[test]
fn invalid_skill_name_in_frontmatter_is_refused_before_skill_fs_access() {
    let tree = Tree::new();
    for bad in ["a b", "ä", "s.1", "s/1", "../s", "s\\1"] {
        tree.write("r/a.md", &format!("---\nskills: [{bad}]\n---\nA"));
        let err = tree.profiles(&["r"], &["nonexistent"]).resolve(&names(&["a"])).unwrap_err();
        assert!(is_invalid(&err, bad), "{bad:?}: {err:?}");
    }
}

#[test]
fn unknown_skill_before_invalid_skill_reports_unknown() {
    // Sharp edge: skill names are checked in grant order, so an unknown valid skill earlier in the
    // list masks an invalid one later. Not a safety issue (nothing is read), just error priority.
    let tree = Tree::new();
    tree.write("r/a.md", "---\nskills: [ghost, ../x]\n---\nA");
    let err = tree.profiles(&["r"], &["s"]).resolve(&names(&["a"])).unwrap_err();
    assert!(matches!(&err, RoleError::UnknownSkill { name, .. } if name == "ghost"), "{err:?}");
}

#[test]
fn skill_name_matching_a_file_not_a_dir_is_unknown() {
    let tree = Tree::new();
    tree.write("r/a.md", "---\nskills: [x]\n---\nA");
    tree.write("s/x", "a file, not a dir");
    let err = tree.profiles(&["r"], &["s"]).resolve(&names(&["a"])).unwrap_err();
    assert!(matches!(err, RoleError::UnknownSkill { .. }), "{err:?}");
}

// ---- 4. symlinks -------------------------------------------------------------------------------

#[test]
fn role_file_symlink_to_a_file_works() {
    let tree = Tree::new();
    tree.write("real/base.md", "---\nskills: [k]\n---\nREAL");
    tree.write("s/k/SKILL.md", "");
    std::fs::create_dir_all(tree.path("r")).unwrap();
    std::os::unix::fs::symlink(tree.path("real/base.md"), tree.path("r/a.md")).unwrap();
    let r = tree.profiles(&["r"], &["s"]).resolve(&names(&["a"])).unwrap();
    assert_eq!(r.text, "REAL");
    assert_eq!(r.skill_dirs, vec![tree.path("s/k")]);
}

#[test]
fn symlink_loop_role_file_is_an_error_not_a_hang() {
    // Sharp edge: `is_file()` fails on ELOOP, so the loop is reported as UnknownRole (the name is
    // "not found") rather than a Read error naming the loop.
    let tree = Tree::new();
    std::fs::create_dir_all(tree.path("r")).unwrap();
    std::os::unix::fs::symlink(tree.path("r/b.md"), tree.path("r/a.md")).unwrap();
    std::os::unix::fs::symlink(tree.path("r/a.md"), tree.path("r/b.md")).unwrap();
    let err = tree.profiles(&["r"], &[]).resolve(&names(&["a"])).unwrap_err();
    assert!(
        matches!(err, RoleError::UnknownRole { .. } | RoleError::Read { .. }),
        "{err:?}"
    );
    assert!(matches!(err, RoleError::UnknownRole { .. }), "documented behaviour: {err:?}");
}

#[test]
fn dangling_symlink_role_file_is_unknown_role() {
    let tree = Tree::new();
    std::fs::create_dir_all(tree.path("r")).unwrap();
    std::os::unix::fs::symlink(tree.path("nowhere.md"), tree.path("r/a.md")).unwrap();
    let err = tree.profiles(&["r"], &[]).resolve(&names(&["a"])).unwrap_err();
    assert!(matches!(err, RoleError::UnknownRole { .. }), "{err:?}");
}

#[test]
fn symlinked_skill_dir_is_returned_as_the_link_path() {
    let tree = Tree::new();
    tree.write("r/a.md", "---\nskills: [k, j]\n---\nA");
    tree.write("real/k/SKILL.md", "");
    tree.write("real-skills/j/SKILL.md", "");
    std::fs::create_dir_all(tree.path("s")).unwrap();
    std::os::unix::fs::symlink(tree.path("real/k"), tree.path("s/k")).unwrap();
    // whole skill root is a symlink
    std::os::unix::fs::symlink(tree.path("real-skills"), tree.path("linked-root")).unwrap();
    let r = tree.profiles(&["r"], &["linked-root", "s"]).resolve(&names(&["a"])).unwrap();
    assert_eq!(r.skill_dirs, vec![tree.path("s/k"), tree.path("linked-root/j")]);
    assert_ne!(r.skill_dirs[0], tree.path("real/k").canonicalize().unwrap());
}

#[test]
fn symlink_loop_skill_dir_is_unknown_skill() {
    let tree = Tree::new();
    tree.write("r/a.md", "---\nskills: [k]\n---\nA");
    std::fs::create_dir_all(tree.path("s")).unwrap();
    std::os::unix::fs::symlink(tree.path("s/k"), tree.path("s/k")).unwrap();
    let err = tree.profiles(&["r"], &["s"]).resolve(&names(&["a"])).unwrap_err();
    assert!(matches!(err, RoleError::UnknownSkill { .. }), "{err:?}");
}

#[test]
fn profile_dir_that_is_a_file_is_just_skipped() {
    let tree = Tree::new();
    tree.write("not-a-dir", "x");
    tree.write("r/a.md", "A");
    let r = tree.profiles(&["not-a-dir", "r"], &[]).resolve(&names(&["a"])).unwrap();
    assert_eq!(r.text, "A");
}

// ---- 5. Profile::write -------------------------------------------------------------------------

fn resolved(text: &str) -> ResolvedProfile {
    ResolvedProfile {
        text: text.to_owned(),
        skill_dirs: vec![PathBuf::from("/skills/one")],
    }
}

#[test]
fn write_under_a_file_parent_fails() {
    let tree = Tree::new();
    tree.write("file", "x");
    let err = Profile::write(resolved("t"), &tree.path("file/child.md")).unwrap_err();
    assert!(
        matches!(err.kind(), std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotADirectory),
        "{err:?}"
    );
    assert_eq!(std::fs::read_to_string(tree.path("file")).unwrap(), "x");
}

#[test]
fn write_overwrites_and_truncates() {
    let tree = Tree::new();
    let path = tree.path("p/x.md");
    Profile::write(resolved("a much longer first text"), &path).unwrap();
    let p = Profile::write(resolved("short"), &path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "short");
    assert_eq!(p.text(), "short");
    assert_eq!(p.file(), path);
    assert_eq!(p.skill_dirs(), [Path::new("/skills/one")]);
}

#[test]
fn write_empty_text_writes_an_empty_file() {
    let tree = Tree::new();
    let path = tree.path("x.md");
    let p = Profile::write(resolved(""), &path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    assert_eq!(p.text(), "");
}

#[test]
fn write_to_a_directory_path_fails() {
    let tree = Tree::new();
    let err = Profile::write(resolved("t"), tree.dir.path()).unwrap_err();
    assert!(err.kind() != std::io::ErrorKind::NotFound, "{err:?}");
}

#[test]
fn write_deep_nested_parents_are_created() {
    let tree = Tree::new();
    let path = tree.path("a/b/c/d/e.md");
    Profile::write(resolved("deep"), &path).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), "deep");
}

#[test]
fn write_text_with_crlf_and_nul_is_byte_exact() {
    let tree = Tree::new();
    let path = tree.path("x.md");
    let text = "a\r\nb\0c\u{feff}";
    Profile::write(resolved(text), &path).unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), text);
}

#[test]
fn write_empty_path_fails() {
    Profile::write(resolved("t"), Path::new("")).unwrap_err();
}

// ---- 6. concatenation --------------------------------------------------------------------------

#[test]
fn bodies_are_trimmed_and_joined_with_blank_line() {
    let tree = Tree::new();
    tree.write("r/a.md", "\n\n  A  \n\n\n");
    tree.write("r/b.md", "B");
    let r = tree.profiles(&["r"], &[]).resolve(&names(&["a", "b"])).unwrap();
    assert_eq!(r.text, "A\n\nB");
}

#[test]
fn empty_bodies_contribute_no_slot() {
    let tree = Tree::new();
    tree.write("r/a.md", "A");
    tree.write("r/e.md", "");
    tree.write("r/b.md", "B");
    let p = tree.profiles(&["r"], &[]);
    assert_eq!(p.resolve(&names(&["a", "e", "b"])).unwrap().text, "A\n\nB");
    assert_eq!(p.resolve(&names(&["e", "a"])).unwrap().text, "A");
    assert_eq!(p.resolve(&names(&["a", "e"])).unwrap().text, "A");
    assert_eq!(p.resolve(&names(&["e"])).unwrap().text, "");
    tree.write("r/c.md", "C");
    tree.write("r/c.local.md", "---\nskills: []\n---\n");
    assert_eq!(p.resolve(&names(&["c"])).unwrap().text, "C");
}

#[test]
fn inner_whitespace_is_preserved() {
    let tree = Tree::new();
    tree.write("r/a.md", "line1\n\n\n\n   indented\n\tTab\n");
    let r = tree.profiles(&["r"], &[]).resolve(&names(&["a"])).unwrap();
    assert_eq!(r.text, "line1\n\n\n\n   indented\n\tTab");
}

#[test]
fn errors_name_the_offending_file() {
    let tree = Tree::new();
    tree.write("r/a.md", "A");
    let local = tree.write("r/a.local.md", "---\nunclosed");
    let err = tree.profiles(&["r"], &[]).resolve(&names(&["a"])).unwrap_err();
    assert!(matches!(&err, RoleError::UnclosedFrontmatter(p) if *p == local), "{err:?}");
}

#[test]
fn invalid_utf8_role_file_is_a_read_error() {
    let tree = Tree::new();
    let path = tree.path("r/a.md");
    std::fs::create_dir_all(tree.path("r")).unwrap();
    std::fs::write(&path, [0xff, 0xfe, b'A']).unwrap();
    let err = tree.profiles(&["r"], &[]).resolve(&names(&["a"])).unwrap_err();
    assert!(matches!(&err, RoleError::Read { path: p, .. } if *p == path), "{err:?}");
}

#[test]
fn skill_lookup_is_lazy_only_after_all_roles_read() {
    // An unknown second role is reported even when the first role grants an unknown skill:
    // roles are resolved before skills.
    let tree = Tree::new();
    tree.write("r/a.md", "---\nskills: [ghost]\n---\nA");
    let err = tree.profiles(&["r"], &["s"]).resolve(&names(&["a", "nope"])).unwrap_err();
    assert!(matches!(err, RoleError::UnknownRole { .. }), "{err:?}");
}
