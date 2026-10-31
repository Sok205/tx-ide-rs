//! Adversarial / property tests for `tx_core::engine`: `EngineRegistry`, `resolve_in`, and
//! `apply_workspace`. Tests marked `#[ignore = "BUG: …"]` document real failures; anything that
//! is merely surprising is asserted as observed and labelled "sharp edge".

use std::path::{Component, Path, PathBuf};

use proptest::prelude::*;
use tx_core::engine::{apply_workspace, resolve_in};
use tx_core::{
    CoreError, EngineError, EngineKind, EngineRegistry, Launch, LaunchPlan, LaunchRequest,
    Lifecycle, LlmEngine, LlmEngineState, Profile, ResolvedProfile, Session, SessionId,
    WorkspaceAction,
};

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

/// An engine of the kind, owning the ext key of the same name.
struct Stub(EngineKind);

impl LlmEngine for Stub {
    fn on_hook(
        &self,
        _: &tx_core::Session,
        _: &tx_core::LlmEngineHook,
    ) -> tx_core::LlmEngineChange {
        tx_core::LlmEngineChange::NONE
    }

    fn kind(&self) -> EngineKind {
        self.0
    }

    fn ext_key(&self) -> &'static str {
        self.0.as_str()
    }

    fn configure(
        &self,
        _: &tx_core::Session,
        options: &serde_json::Value,
    ) -> Result<serde_json::Value, EngineError> {
        Ok(options.clone())
    }

    fn start(&self, _: &LaunchRequest<'_>, _: &str) -> Result<LaunchPlan, EngineError> {
        Ok(LaunchPlan::default())
    }

    fn resume(&self, _: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        Ok(LaunchPlan::default())
    }

    fn fork(&self, _: &LaunchRequest<'_>, _: &Session, _: &str) -> Result<LaunchPlan, EngineError> {
        Ok(LaunchPlan::default())
    }
}

fn stub(kind: EngineKind) -> Box<dyn LlmEngine> {
    Box::new(Stub(kind))
}

fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new().prefix("txe").tempdir().unwrap()
}

fn write(path: &str, content: &str) -> WorkspaceAction {
    WorkspaceAction::Write {
        path: path.into(),
        content: content.into(),
    }
}

fn link(src: impl Into<PathBuf>, dst: &str) -> WorkspaceAction {
    WorkspaceAction::Symlink {
        src: src.into(),
        dst: dst.into(),
    }
}

fn has_dot_component(path: &Path) -> bool {
    path.components()
        .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
}

/// Lexical depth model for a relative path: `None` when a `..` pops below the start.
fn relative_depth(components: &[String]) -> Option<usize> {
    let mut depth = 0usize;
    for c in components {
        match c.as_str() {
            "" | "." => {}
            ".." => depth = depth.checked_sub(1)?,
            _ => depth += 1,
        }
    }
    Some(depth)
}

fn component() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("a".to_owned()),
        Just("b".to_owned()),
        Just("..".to_owned()),
        Just(".".to_owned()),
        Just(String::new()),
        Just("a b".to_owned()),
        Just("..a".to_owned()),
        Just("a..".to_owned()),
        Just("ünï cødé".to_owned()),
        Just("日本".to_owned()),
    ]
}

fn relative_path() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(component(), 0..=8)
}

/// Joins components without letting a leading empty component turn the path absolute.
fn join_relative(components: &[String]) -> PathBuf {
    let joined = components.join("/");
    if joined.starts_with('/') {
        PathBuf::from(format!(".{joined}"))
    } else {
        PathBuf::from(joined)
    }
}

// ---------------------------------------------------------------------------------------------
// 1. resolve_in
// ---------------------------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn resolve_in_relative_is_contained_or_refused(components in relative_path()) {
        let cwd = Path::new("/w/root");
        let path = join_relative(&components);
        let result = resolve_in(cwd, &path);
        let depth = relative_depth(&components);
        match result {
            Ok(resolved) => {
                prop_assert!(resolved.starts_with(cwd), "{resolved:?}");
                prop_assert!(!has_dot_component(&resolved), "{resolved:?}");
                prop_assert!(resolved.is_absolute());
                // Round trip: resolving the resolved path (absolute, inside) is a fixed point.
                prop_assert_eq!(resolve_in(cwd, &resolved).unwrap(), resolved.clone());
                // Depth never negative implies Ok; Ok implies the final depth is non-negative in
                // the sense that we are at or under cwd (dipping to `/w` and back is allowed).
                if let Some(d) = depth {
                    let actual = resolved.strip_prefix(cwd).unwrap().components().count();
                    prop_assert_eq!(actual, d);
                }
            }
            Err(EngineError::Workspace { path: reported, reason }) => {
                prop_assert!(depth.is_none(), "refused although depth stays >= 0: {components:?}");
                prop_assert_eq!(reported, path.clone());
                prop_assert!(reason.contains("escapes"));
            }
            Err(_) => prop_assert!(false, "unexpected error kind from resolve_in"),
        }
        // A negative depth is not always refused: a dip and return is Ok, see
        // `resolve_in_dip_below_cwd_and_return_is_accepted`; the containment check above covers it.
    }

    #[test]
    fn resolve_in_absolute_is_ok_iff_lexically_inside(components in relative_path(), inside in any::<bool>()) {
        let cwd = Path::new("/w/root");
        let base = if inside { PathBuf::from("/w/root") } else { PathBuf::from("/w/other") };
        let path = base.join(join_relative(&components));
        let result = resolve_in(cwd, &path);
        match result {
            Ok(resolved) => {
                prop_assert!(resolved.starts_with(cwd), "{resolved:?}");
                prop_assert!(!has_dot_component(&resolved));
            }
            Err(_) => {
                // Either it points outside or it climbed through cwd.
                let ok_lexically = inside && relative_depth(&components).is_some();
                prop_assert!(!ok_lexically, "refused inside path {path:?}");
            }
        }
    }

    #[test]
    fn resolve_in_never_panics_on_arbitrary_strings(s in "[^\0]{0,64}") {
        let _ = resolve_in(Path::new("/w/root"), Path::new(&s));
        let _ = resolve_in(Path::new("rel"), Path::new(&s));
        let _ = resolve_in(Path::new("/"), Path::new(&s));
        let _ = resolve_in(Path::new(""), Path::new(&s));
    }
}

#[test]
fn resolve_in_dip_below_cwd_and_return_is_accepted() {
    // Sharp edge: `../root/x` climbs out of cwd lexically then back in; accepted (fine, the
    // final location is inside).
    let cwd = Path::new("/w/root");
    assert_eq!(
        resolve_in(cwd, Path::new("../root/x")).unwrap(),
        PathBuf::from("/w/root/x")
    );
    assert!(resolve_in(cwd, Path::new("../other/x")).is_err());
    assert!(resolve_in(cwd, Path::new("../../w/root/x")).is_ok());
    assert!(resolve_in(cwd, Path::new("../../../w/root/x")).is_err());
}

#[test]
fn resolve_in_empty_and_dot_resolve_to_cwd_itself() {
    let cwd = Path::new("/w/root");
    assert_eq!(resolve_in(cwd, Path::new("")).unwrap(), cwd);
    assert_eq!(resolve_in(cwd, Path::new(".")).unwrap(), cwd);
    assert_eq!(resolve_in(cwd, Path::new("./")).unwrap(), cwd);
    assert_eq!(resolve_in(cwd, Path::new("a/..")).unwrap(), cwd);
    assert_eq!(resolve_in(cwd, cwd).unwrap(), cwd);
    assert_eq!(resolve_in(cwd, Path::new("/w/root/")).unwrap(), cwd);
}

#[test]
fn resolve_in_absolute_inside_cwd_is_ok_and_outside_is_err() {
    let cwd = Path::new("/w/root");
    assert_eq!(
        resolve_in(cwd, &cwd.join("x/y")).unwrap(),
        PathBuf::from("/w/root/x/y")
    );
    assert_eq!(
        resolve_in(cwd, Path::new("/w/root/a/../b")).unwrap(),
        PathBuf::from("/w/root/b")
    );
    for bad in [
        "/w/root2/x",
        "/w/roo",
        "/w",
        "/",
        "/w/root/../other",
        "/etc/passwd",
    ] {
        assert!(resolve_in(cwd, Path::new(bad)).is_err(), "{bad}");
    }
}

#[test]
fn resolve_in_trailing_slash_on_cwd_is_harmless() {
    let cwd = Path::new("/w/root/");
    let got = resolve_in(cwd, Path::new("a")).unwrap();
    assert_eq!(got, PathBuf::from("/w/root/a"));
    assert!(got.starts_with(cwd));
}

#[test]
fn resolve_in_cwd_with_inner_dot_is_harmless() {
    // `Path::components` already drops interior `.` so `starts_with` still matches.
    let cwd = Path::new("/w/./root");
    assert_eq!(
        resolve_in(cwd, Path::new("a")).unwrap(),
        PathBuf::from("/w/root/a")
    );
}

#[test]
fn resolve_in_cwd_containing_dotdot_accepts_relative_paths() {
    let cwd = Path::new("/w/x/../root");
    assert_eq!(
        resolve_in(cwd, Path::new("a")).unwrap(),
        PathBuf::from("/w/root/a")
    );
    assert_eq!(
        resolve_in(cwd, Path::new("")).unwrap(),
        PathBuf::from("/w/root")
    );
    assert_eq!(
        resolve_in(cwd, Path::new("/w/root/a")).unwrap(),
        PathBuf::from("/w/root/a")
    );
    assert!(resolve_in(cwd, Path::new("/w/x/a")).is_err());
}

#[test]
fn resolve_in_refuses_a_relative_cwd() {
    for cwd in ["rel/w", "", ".", "../x"] {
        assert!(
            resolve_in(Path::new(cwd), Path::new("x")).is_err(),
            "{cwd:?}"
        );
        assert!(
            resolve_in(Path::new(cwd), Path::new("/etc/passwd")).is_err(),
            "{cwd:?}"
        );
    }
}

#[test]
fn resolve_in_root_cwd() {
    let cwd = Path::new("/");
    assert_eq!(
        resolve_in(cwd, Path::new("x")).unwrap(),
        PathBuf::from("/x")
    );
    assert!(resolve_in(cwd, Path::new("..")).is_err());
    assert!(resolve_in(cwd, Path::new("../x")).is_err());
    assert_eq!(resolve_in(cwd, Path::new("")).unwrap(), PathBuf::from("/"));
}

#[test]
fn resolve_in_canonical_absolute_path_vs_symlinked_cwd() {
    // Sharp edge: on macOS tempdirs live under /var (-> /private/var). An adapter that hands
    // back a canonicalized absolute path under a non-canonical cwd is refused, because the
    // check is lexical only.
    let dir = tmp();
    let cwd = dir.path();
    let canonical = cwd.canonicalize().unwrap();
    let non_canonical_absolute = cwd.join("x");
    assert!(resolve_in(cwd, &non_canonical_absolute).is_ok());
    if canonical != cwd {
        assert!(
            resolve_in(cwd, &canonical.join("x")).is_err(),
            "lexical check accepted a canonical path for a symlinked cwd"
        );
        // And the other way round: canonical cwd refuses the symlinked spelling.
        assert!(resolve_in(&canonical, &cwd.join("x")).is_err());
    }
}

#[test]
fn resolve_in_symlink_cwd_still_contains_results() {
    let dir = tmp();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let linked = dir.path().join("linked");
    std::os::unix::fs::symlink(&real, &linked).unwrap();
    let got = resolve_in(&linked, Path::new("a/b")).unwrap();
    assert!(got.starts_with(&linked));
    assert_eq!(got, linked.join("a/b"));
    // Writing through a symlinked cwd lands in the real directory.
    apply_workspace(&linked, &[write("a/b", "x")]).unwrap();
    assert_eq!(std::fs::read_to_string(real.join("a/b")).unwrap(), "x");
}

#[test]
fn write_to_cwd_itself_fails_cleanly_and_names_cwd() {
    let dir = tmp();
    for p in ["", ".", "./", "a/.."] {
        let error = apply_workspace(dir.path(), &[write(p, "x")]).unwrap_err();
        match &error {
            EngineError::Workspace { path, .. } => assert_eq!(path, dir.path(), "{p:?}"),
            other => panic!("{p:?}: {other:?}"),
        }
    }
    assert!(dir.path().is_dir());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

// ---------------------------------------------------------------------------------------------
// 2. Symlink traversal: the lexical check cannot see filesystem symlinks
// ---------------------------------------------------------------------------------------------

#[test]
fn write_through_pre_existing_symlink_escapes_cwd() {
    // Sharp edge, documented: lexical containment is not physical containment.
    let dir = tmp();
    let cwd = dir.path().join("cwd");
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, cwd.join("link")).unwrap();
    apply_workspace(&cwd, &[write("link/x", "escaped")]).unwrap();
    assert_eq!(
        std::fs::read_to_string(elsewhere.join("x")).unwrap(),
        "escaped"
    );
    assert!(!cwd.join("x").exists());
}

#[test]
fn symlink_action_then_write_escapes_cwd_within_one_plan() {
    // Sharp edge: the plan itself can create the escape hatch and then use it. All targets
    // pass the lexical check up front, so "checked before anything is written" does not
    // prevent this.
    let dir = tmp();
    let cwd = dir.path().join("cwd");
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    apply_workspace(
        &cwd,
        &[
            link(elsewhere.clone(), "skills/one"),
            write("skills/one/SKILL.md", "pwned"),
        ],
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(elsewhere.join("SKILL.md")).unwrap(),
        "pwned"
    );
}

#[test]
fn symlink_dst_through_symlinked_parent_lands_outside() {
    let dir = tmp();
    let cwd = dir.path().join("cwd");
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, cwd.join("link")).unwrap();
    apply_workspace(&cwd, &[link("/nonexistent", "link/inner")]).unwrap();
    assert!(elsewhere.join("inner").is_symlink());
}

// ---------------------------------------------------------------------------------------------
// 3. Atomicity claim
// ---------------------------------------------------------------------------------------------

#[test]
fn late_escaping_write_leaves_nothing_written() {
    let dir = tmp();
    let error = apply_workspace(
        dir.path(),
        &[
            write("a.txt", "1"),
            write("b/c.txt", "2"),
            write("../escape", "3"),
        ],
    )
    .unwrap_err();
    let is_workspace = matches!(error, EngineError::Workspace { .. });
    assert!(is_workspace, "{error:?}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert!(!dir.path().parent().unwrap().join("escape").exists());
}

#[test]
fn apply_time_failure_is_not_rolled_back() {
    // Sharp edge: the pre-check is lexical only. A Symlink onto an existing file fails during
    // application, and the preceding Write stays.
    let dir = tmp();
    std::fs::write(dir.path().join("existing"), "keep").unwrap();
    let error = apply_workspace(
        dir.path(),
        &[
            write("first.txt", "written"),
            link("/tmp", "existing"),
            write("third.txt", "never"),
        ],
    )
    .unwrap_err();
    match &error {
        EngineError::Workspace { path, reason } => {
            assert_eq!(path, &dir.path().join("existing"));
            assert!(reason.contains("not a symlink"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("first.txt")).unwrap(),
        "written"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("existing")).unwrap(),
        "keep"
    );
    assert!(!dir.path().join("third.txt").exists());
}

#[test]
fn parents_are_created_for_later_actions_before_an_earlier_apply_failure_is_noticed() {
    // Parent creation happens per action, in order, so an early failure leaves nothing from
    // later actions. (Pinned so a reordering is noticed.)
    let dir = tmp();
    std::fs::create_dir(dir.path().join("isdir")).unwrap();
    let _ =
        apply_workspace(dir.path(), &[write("isdir", "x"), write("later/leaf", "y")]).unwrap_err();
    assert!(!dir.path().join("later").exists());
}

// ---------------------------------------------------------------------------------------------
// 4. Symlink details
// ---------------------------------------------------------------------------------------------

#[test]
fn relative_src_is_stored_verbatim() {
    let dir = tmp();
    apply_workspace(dir.path(), &[link("../skills/one", ".claude/skills/one")]).unwrap();
    let dst = dir.path().join(".claude/skills/one");
    assert_eq!(
        std::fs::read_link(&dst).unwrap(),
        PathBuf::from("../skills/one")
    );
    // It resolves relative to the link's parent (.claude/skills), i.e. .claude/skills/../skills/one
    // = .claude/skills/one — the link points at itself. Dangling, allowed, no error.
    assert!(dst.is_symlink());
    assert!(!dst.exists());
}

#[test]
fn dangling_src_is_allowed_and_src_is_not_resolved_against_cwd() {
    let dir = tmp();
    apply_workspace(dir.path(), &[link("/definitely/not/here", "l")]).unwrap();
    assert_eq!(
        std::fs::read_link(dir.path().join("l")).unwrap(),
        PathBuf::from("/definitely/not/here")
    );
    // src may even escape cwd: it is a target, not a workspace path.
    apply_workspace(dir.path(), &[link("../../../../etc", "etc")]).unwrap();
    assert_eq!(
        std::fs::read_link(dir.path().join("etc")).unwrap(),
        PathBuf::from("../../../../etc")
    );
}

#[test]
fn src_equal_to_dst_creates_a_self_loop_link() {
    let dir = tmp();
    apply_workspace(dir.path(), &[link("me", "me")]).unwrap();
    let me = dir.path().join("me");
    assert!(me.is_symlink());
    assert_eq!(std::fs::read_link(&me).unwrap(), PathBuf::from("me"));
    assert!(me.metadata().is_err(), "a self loop should not stat");
    // Re-applying replaces it (is_symlink is true even for a loop).
    apply_workspace(dir.path(), &[link("me", "me")]).unwrap();
    assert!(me.is_symlink());
}

#[test]
fn existing_symlink_to_directory_is_replaced_and_target_dir_survives() {
    let dir = tmp();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("keep"), "k").unwrap();
    let l = dir.path().join("l");
    std::os::unix::fs::symlink(&real, &l).unwrap();
    assert!(l.is_dir(), "precondition: symlink-to-dir");
    apply_workspace(dir.path(), &[link("/other", "l")]).unwrap();
    assert_eq!(std::fs::read_link(&l).unwrap(), PathBuf::from("/other"));
    assert!(
        real.join("keep").exists(),
        "remove_file must unlink, not recurse"
    );
}

#[test]
fn symlink_dst_with_trailing_slash_onto_existing_symlink_to_dir() {
    // Trailing slash is dropped lexically, so `l/` behaves like `l`.
    let dir = tmp();
    let real = dir.path().join("real");
    std::fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join("l")).unwrap();
    apply_workspace(dir.path(), &[link("/x", "l/")]).unwrap();
    assert_eq!(
        std::fs::read_link(dir.path().join("l")).unwrap(),
        PathBuf::from("/x")
    );
}

#[test]
fn symlink_dst_equal_to_cwd_is_refused_as_existing_directory() {
    let dir = tmp();
    let error = apply_workspace(dir.path(), &[link("/x", "")]).unwrap_err();
    match &error {
        EngineError::Workspace { path, reason } => {
            assert_eq!(path, dir.path());
            assert!(reason.contains("not a symlink"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert!(dir.path().is_dir());
}

#[test]
fn write_through_dangling_symlink_parent_fails_with_file_exists() {
    // Sharp edge (fine, even protective): `create_dir_all` on a dangling symlink sees a
    // non-directory that already exists and errors with EEXIST; the target dir is NOT created
    // behind the link. The error names the write target.
    let dir = tmp();
    let target = dir.path().join("target-dir");
    let error = apply_workspace(
        dir.path(),
        &[link(target.clone(), "l"), write("l/file", "via link")],
    )
    .unwrap_err();
    match &error {
        EngineError::Workspace { path, reason } => {
            assert_eq!(path, &dir.path().join("l/file"));
            assert!(reason.contains("exists"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    assert!(!target.exists());
    // Non-atomic: the link from the first action remains.
    assert!(dir.path().join("l").is_symlink());
}

#[test]
fn symlink_with_empty_src_is_accepted_on_macos() {
    // Sharp edge: macOS `symlink("")` succeeds and creates a link with an empty target (Linux
    // returns ENOENT). Core passes `src` through verbatim, so the plan "succeeds" with a link
    // that can never resolve. Either outcome is tolerated here; the error, if any, names dst.
    let dir = tmp();
    let l = dir.path().join("l");
    match apply_workspace(dir.path(), &[link("", "l")]) {
        Ok(()) => {
            assert!(l.is_symlink());
            assert_eq!(std::fs::read_link(&l).unwrap(), PathBuf::from(""));
            assert!(!l.exists());
        }
        Err(EngineError::Workspace { path, .. }) => {
            assert_eq!(path, l);
            assert!(!l.is_symlink());
        }
        Err(other) => panic!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------
// 5. Write details
// ---------------------------------------------------------------------------------------------

#[test]
fn write_content_edge_cases() {
    let dir = tmp();
    let nul = "a\0b\0\0c";
    let big = "x".repeat(10 * 1024 * 1024);
    apply_workspace(
        dir.path(),
        &[write("nul", nul), write("big", &big), write("empty", "")],
    )
    .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("nul")).unwrap(),
        nul.as_bytes()
    );
    assert_eq!(
        std::fs::metadata(dir.path().join("big")).unwrap().len(),
        10 * 1024 * 1024
    );
    assert_eq!(
        std::fs::metadata(dir.path().join("empty")).unwrap().len(),
        0
    );
}

#[test]
fn write_with_trailing_slash_writes_a_file_named_without_it() {
    // Sharp edge: `a/` looks like a directory but the slash is dropped lexically.
    let dir = tmp();
    apply_workspace(dir.path(), &[write("a/", "x")]).unwrap();
    assert!(dir.path().join("a").is_file());
}

#[test]
fn write_onto_existing_directory_errs_naming_it() {
    let dir = tmp();
    std::fs::create_dir(dir.path().join("d")).unwrap();
    let error = apply_workspace(dir.path(), &[write("d", "x")]).unwrap_err();
    match &error {
        EngineError::Workspace { path, .. } => assert_eq!(path, &dir.path().join("d")),
        other => panic!("{other:?}"),
    }
    assert!(error.to_string().contains("/d:"), "{error}");
    assert!(dir.path().join("d").is_dir());
}

#[test]
fn write_under_parent_that_is_symlink_to_file_errs_naming_target() {
    let dir = tmp();
    std::fs::write(dir.path().join("file"), "").unwrap();
    std::os::unix::fs::symlink(dir.path().join("file"), dir.path().join("lf")).unwrap();
    let error = apply_workspace(dir.path(), &[write("lf/x", "x")]).unwrap_err();
    match &error {
        EngineError::Workspace { path, .. } => assert_eq!(path, &dir.path().join("lf/x")),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("file")).unwrap(),
        ""
    );
}

#[test]
fn write_onto_symlink_follows_it() {
    // Sharp edge: Write does not refuse an existing symlink; it writes through it.
    let dir = tmp();
    let real = dir.path().join("real");
    std::fs::write(&real, "old").unwrap();
    std::os::unix::fs::symlink(&real, dir.path().join("l")).unwrap();
    apply_workspace(dir.path(), &[write("l", "new")]).unwrap();
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
    assert!(dir.path().join("l").is_symlink());
}

#[test]
fn write_creates_missing_cwd() {
    // cwd need not exist: create_dir_all on the parent brings it into being.
    let dir = tmp();
    let cwd = dir.path().join("not/yet");
    apply_workspace(&cwd, &[write("f", "x")]).unwrap();
    assert_eq!(std::fs::read_to_string(cwd.join("f")).unwrap(), "x");
}

#[test]
fn write_unicode_and_odd_names() {
    let dir = tmp();
    apply_workspace(
        dir.path(),
        &[
            write("日本/ünï cødé/..a/a..", "u"),
            write("a b/ c /-", "s"),
            write("--/-rf", "d"),
        ],
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("日本/ünï cødé/..a/a..")).unwrap(),
        "u"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("a b/ c /-")).unwrap(),
        "s"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("--/-rf")).unwrap(),
        "d"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn random_relative_write_plans_never_leave_cwd(
        plans in prop::collection::vec((relative_path(), "[a-z]{0,8}"), 0..6)
    ) {
        let dir = tmp();
        let cwd = dir.path().join("cwd");
        std::fs::create_dir(&cwd).unwrap();
        let actions: Vec<WorkspaceAction> = plans
            .iter()
            .map(|(c, content)| write(&join_relative(c).to_string_lossy(), content))
            .collect();
        let result = apply_workspace(&cwd, &actions);
        // Whatever happened, the sibling dir has nothing new and the parent gained nothing
        // but `cwd` itself.
        let siblings: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        prop_assert_eq!(siblings, vec![std::ffi::OsString::from("cwd")]);
        if result.is_ok() {
            for (c, _) in &plans {
                let resolved = resolve_in(&cwd, &join_relative(c)).unwrap();
                prop_assert!(resolved.is_file(), "{:?}", resolved);
                prop_assert!(resolved.starts_with(&cwd));
            }
        } else {
            // Any Err must be a Workspace error naming a path.
            let named = matches!(result, Err(EngineError::Workspace { .. }));
            prop_assert!(named);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 6. Registry
// ---------------------------------------------------------------------------------------------

/// Registration order does not matter: `kinds()` is sorted; a kind registered twice is
/// `EngineTaken`; a kind nobody registered is absent.
#[test]
fn registry_is_keyed_by_kind_in_any_order() {
    for order in [
        vec![EngineKind::Claude, EngineKind::Codex],
        vec![EngineKind::Codex, EngineKind::Claude],
    ] {
        let mut registry = EngineRegistry::new();
        for kind in &order {
            registry.register(stub(*kind)).unwrap();
        }
        assert_eq!(registry.kinds(), EngineKind::ALL);
        for kind in EngineKind::ALL {
            assert_eq!(registry.get(kind).unwrap().kind(), kind);
            let error = registry.register(stub(kind)).unwrap_err();
            assert!(
                matches!(error, CoreError::EngineTaken(taken) if taken == kind),
                "{error:?}"
            );
        }
        assert_eq!(registry.kinds(), EngineKind::ALL);
    }
    let mut registry = EngineRegistry::new();
    assert!(registry.kinds().is_empty());
    registry.register(stub(EngineKind::Codex)).unwrap();
    assert_eq!(registry.kinds(), [EngineKind::Codex]);
    assert!(registry.get(EngineKind::Claude).is_none());
}

/// The kind's name is its only spelling: `EngineKind::from_str` and serde agree, and anything
/// else, including case variants and padding, is refused without a panic.
proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn engine_kind_parses_exactly_its_lowercase_names(text in "[A-Za-z _\\-]{0,8}|claude|codex") {
        let known = EngineKind::ALL.into_iter().find(|kind| kind.as_str() == text);
        let parsed = text.parse::<EngineKind>();
        match known {
            Some(kind) => {
                prop_assert_eq!(parsed.unwrap(), kind);
                prop_assert_eq!(serde_json::from_str::<EngineKind>(&format!("{:?}", text)).unwrap(), kind);
                prop_assert_eq!(serde_json::to_string(&kind).unwrap(), format!("{:?}", text));
            }
            None => {
                let refused = matches!(parsed, Err(CoreError::InvalidEngineKind(ref t)) if *t == text);
                prop_assert!(refused, "{:?}", text);
                prop_assert!(serde_json::from_str::<EngineKind>(&format!("{:?}", text)).is_err());
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 7. Core does not validate the LaunchPlan
// ---------------------------------------------------------------------------------------------

#[test]
fn core_accepts_a_plan_with_empty_program() {
    // Documented, not a bug here: validating `program` is the launcher's job (Task 10).
    let dir = tmp();
    let profile = Profile::write(
        ResolvedProfile {
            text: "t".into(),
            skill_dirs: vec![],
        },
        &dir.path().join("p.md"),
    )
    .unwrap();
    let session = Session {
        id: SessionId::mint(),
        name: "s".into(),
        parent: None,
        tags: Vec::new(),
        cwd: dir.path().to_owned(),
        created_at: 0.0,
        ended_at: None,
        archived_at: None,
        last_modified_at: 0.0,
        lifecycle: Lifecycle::Running,
        launch: Launch::Agent {
            engine: EngineKind::Claude,
            profile: Vec::new(),
            model: None,
            effort: None,
            resume: None,
        },
        state: LlmEngineState::Starting,
        ext: Default::default(),
    };
    let request = LaunchRequest::new(&session, &profile);
    let mut registry = EngineRegistry::new();
    registry.register(stub(EngineKind::Claude)).unwrap();
    let plan = registry
        .get(EngineKind::Claude)
        .unwrap()
        .start(&request, "")
        .unwrap();
    assert_eq!(plan.program, "");
    assert_eq!(plan, LaunchPlan::default());
    apply_workspace(dir.path(), &plan.workspace).unwrap();
}

#[test]
fn error_display_names_the_offending_path() {
    let error = resolve_in(Path::new("/w"), Path::new("../x")).unwrap_err();
    assert_eq!(
        error.to_string(),
        "workspace action on ../x: escapes the working directory"
    );
}
