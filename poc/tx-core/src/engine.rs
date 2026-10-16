//! Engines as plugins: one `EngineAdapter` per coding-agent CLI, each a pure function from a
//! `LaunchRequest` to a `LaunchPlan`. Core applies the plan's workspace actions and starts the
//! process, so an adapter is unit-testable without a filesystem or tmux.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::error::CoreError;
use crate::profile::Profile;
use crate::session::{Effort, validate_name};

/// An adapter could not honor a request, or a workspace action could not be applied.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The adapter refuses the request (an unsupported option, say).
    #[error("{0}")]
    Refused(String),
    #[error("workspace action on {}: {reason}", path.display())]
    Workspace { path: PathBuf, reason: String },
}

/// What core asks an engine to turn into a command line. Borrowed: `launch` is pure.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct LaunchRequest<'a> {
    pub cwd: &'a Path,
    /// Combined role text, the file holding it, and the granted skill dirs.
    pub profile: &'a Profile,
    pub model: Option<&'a str>,
    pub effort: Option<Effort>,
    pub initial_prompt: Option<&'a str>,
    /// An engine-specific conversation id to continue instead of starting fresh. Core never
    /// interprets it.
    pub resume: Option<&'a str>,
}

impl<'a> LaunchRequest<'a> {
    pub fn new(cwd: &'a Path, profile: &'a Profile) -> Self {
        Self {
            cwd,
            profile,
            model: None,
            effort: None,
            initial_prompt: None,
            resume: None,
        }
    }

    pub fn model(mut self, model: &'a str) -> Self {
        self.model = Some(model);
        self
    }

    pub fn effort(mut self, effort: Effort) -> Self {
        self.effort = Some(effort);
        self
    }

    pub fn initial_prompt(mut self, prompt: &'a str) -> Self {
        self.initial_prompt = Some(prompt);
        self
    }

    pub fn resume(mut self, resume: &'a str) -> Self {
        self.resume = Some(resume);
        self
    }
}

/// Filesystem preparation core performs before starting the process. Relative paths resolve
/// against the session's `cwd`; a path that escapes `cwd` is refused.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkspaceAction {
    /// Writes `content` to `path`, creating parents, overwriting.
    Write { path: PathBuf, content: String },
    /// Links `dst` → `src` (`src` is the link target, used verbatim). Replaces an existing
    /// symlink; refuses an existing file or directory.
    Symlink { src: PathBuf, dst: PathBuf },
}

/// How to start the engine: `program args…`, extra environment, and workspace preparation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workspace: Vec<WorkspaceAction>,
}

impl LaunchPlan {
    pub fn new(program: impl Into<String>) -> Self {
        Self::default().program(program)
    }

    pub fn program(mut self, program: impl Into<String>) -> Self {
        self.program = program.into();
        self
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn workspace(mut self, action: WorkspaceAction) -> Self {
        self.workspace.push(action);
        self
    }
}

/// One coding-agent engine. Two required methods; no I/O in `launch`. `Send + Sync` so a core
/// can be shared across threads.
pub trait EngineAdapter: Send + Sync {
    /// A bare name (`[A-Za-z0-9_-]+`); stored in every record this engine launches.
    fn id(&self) -> &str;
    fn launch(&self, req: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError>;

    /// How a message is framed before it is typed into one of this engine's sessions.
    /// `sender` is the sending session's name, `None` for the user. Pure; the default is the
    /// `<from-user>` / `<from-agent session="…">` envelope.
    fn envelope(&self, sender: Option<&str>, text: &str) -> String {
        crate::messaging::build_envelope(sender, text)
    }
}

/// id → adapter. Ids are unique; registration order does not matter.
#[derive(Default)]
pub struct EngineRegistry {
    adapters: BTreeMap<String, Box<dyn EngineAdapter>>,
}

impl EngineRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// `EngineTaken` on a duplicate id; `InvalidName` when the id is not a bare name.
    pub fn register(&mut self, adapter: Box<dyn EngineAdapter>) -> Result<(), CoreError> {
        let id = adapter.id().to_owned();
        validate_name(&id)?;
        if self.adapters.contains_key(&id) {
            return Err(CoreError::EngineTaken(id));
        }
        self.adapters.insert(id, adapter);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&dyn EngineAdapter> {
        self.adapters.get(id).map(Box::as_ref)
    }

    /// Registered ids, sorted.
    pub fn ids(&self) -> Vec<&str> {
        self.adapters.keys().map(String::as_str).collect()
    }
}

fn workspace_error(path: &Path) -> impl FnOnce(io::Error) -> EngineError + '_ {
    move |error| EngineError::Workspace {
        path: path.to_owned(),
        reason: error.to_string(),
    }
}

/// `path` against `cwd`, both normalized lexically (`.` and `..` folded), refused if the result
/// leaves `cwd` or `cwd` is not absolute. Lexical on purpose: it never touches the filesystem,
/// so the check happens before any write. It cannot see symlinks; adapters are trusted code and
/// core canonicalizes `cwd` before launching, which is as far as a lexical check can go.
pub fn resolve_in(cwd: &Path, path: &Path) -> Result<PathBuf, EngineError> {
    let escapes = |path: &Path| EngineError::Workspace {
        path: path.to_owned(),
        reason: "escapes the working directory".into(),
    };
    if !cwd.is_absolute() {
        return Err(EngineError::Workspace {
            path: cwd.to_owned(),
            reason: "working directory is not absolute".into(),
        });
    }
    let cwd = normalize(cwd).ok_or_else(|| escapes(cwd))?;
    let normalized = normalize(&cwd.join(path)).ok_or_else(|| escapes(path))?;
    if !normalized.starts_with(&cwd) {
        return Err(escapes(path));
    }
    Ok(normalized)
}

/// Folds `.` and `..`; `None` when `..` would climb above the root.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    return None;
                }
                normalized.pop();
            }
            other => normalized.push(other),
        }
    }
    Some(normalized)
}

/// Applies `actions` in order against `cwd`. Every target path is checked before anything is
/// written, so a bad action late in the list leaves the workspace untouched.
pub fn apply_workspace(cwd: &Path, actions: &[WorkspaceAction]) -> Result<(), EngineError> {
    let resolved = actions
        .iter()
        .map(|action| match action {
            WorkspaceAction::Write { path, .. } | WorkspaceAction::Symlink { dst: path, .. } => {
                resolve_in(cwd, path)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (action, target) in actions.iter().zip(resolved) {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(workspace_error(&target))?;
        }
        match action {
            WorkspaceAction::Write { content, .. } => {
                std::fs::write(&target, content).map_err(workspace_error(&target))?;
            }
            WorkspaceAction::Symlink { src, .. } => {
                if target.is_symlink() {
                    std::fs::remove_file(&target).map_err(workspace_error(&target))?;
                } else if target.exists() {
                    return Err(EngineError::Workspace {
                        path: target,
                        reason: "exists and is not a symlink".into(),
                    });
                }
                std::os::unix::fs::symlink(src, &target).map_err(workspace_error(&target))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::{Profile, ResolvedProfile};

    struct Stub(&'static str);

    impl EngineAdapter for Stub {
        fn id(&self) -> &str {
            self.0
        }

        fn launch(&self, req: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
            Ok(LaunchPlan {
                program: self.0.to_owned(),
                args: vec![req.profile.file().display().to_string()],
                ..Default::default()
            })
        }
    }

    #[test]
    fn registry_refuses_duplicates_and_lists_sorted_ids() {
        let mut registry = EngineRegistry::new();
        registry.register(Box::new(Stub("zeta"))).unwrap();
        registry.register(Box::new(Stub("alpha"))).unwrap();
        let error = registry.register(Box::new(Stub("zeta"))).unwrap_err();
        assert!(
            matches!(&error, CoreError::EngineTaken(id) if id == "zeta"),
            "{error:?}"
        );
        assert_eq!(registry.ids(), ["alpha", "zeta"]);
        assert_eq!(registry.get("alpha").unwrap().id(), "alpha");
        assert!(registry.get("omega").is_none());
        assert!(EngineRegistry::default().ids().is_empty());
    }

    #[test]
    fn registry_refuses_an_adapter_whose_id_is_not_a_bare_name() {
        let mut registry = EngineRegistry::new();
        for bad in ["", "a/b", "a b", "ä"] {
            let error = registry.register(Box::new(Stub(bad))).unwrap_err();
            assert!(
                matches!(&error, CoreError::InvalidName(id) if id == bad),
                "{bad:?}: {error:?}"
            );
        }
    }

    #[test]
    fn adapter_sees_the_request() {
        let dir = tempfile::tempdir().unwrap();
        let profile = Profile::write(
            ResolvedProfile {
                text: "t".into(),
                skill_dirs: vec![],
            },
            &dir.path().join("p.md"),
        )
        .unwrap();
        let request = LaunchRequest::new(dir.path(), &profile)
            .model("m")
            .effort(Effort::Low);
        let plan = Stub("x").launch(&request).unwrap();
        assert_eq!(plan.program, "x");
        assert_eq!(plan.args, [dir.path().join("p.md").display().to_string()]);
        assert!(plan.env.is_empty() && plan.workspace.is_empty());
    }

    #[test]
    fn write_action_resolves_against_cwd_and_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        apply_workspace(
            dir.path(),
            &[
                WorkspaceAction::Write {
                    path: "a/b/c.txt".into(),
                    content: "one".into(),
                },
                WorkspaceAction::Write {
                    path: dir.path().join("abs.txt"),
                    content: "two".into(),
                },
                WorkspaceAction::Write {
                    path: "a/b/c.txt".into(),
                    content: "three".into(),
                },
            ],
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(),
            "three"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("abs.txt")).unwrap(),
            "two"
        );
    }

    #[test]
    fn symlink_action_creates_and_replaces_links_but_not_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("skills/one");
        std::fs::create_dir_all(&target).unwrap();
        let link = WorkspaceAction::Symlink {
            src: target.clone(),
            dst: ".claude/skills/one".into(),
        };
        apply_workspace(dir.path(), std::slice::from_ref(&link)).unwrap();
        assert_eq!(
            std::fs::read_link(dir.path().join(".claude/skills/one")).unwrap(),
            target
        );
        // Replaces a stale link.
        std::fs::remove_file(dir.path().join(".claude/skills/one")).unwrap();
        std::os::unix::fs::symlink("/nonexistent", dir.path().join(".claude/skills/one")).unwrap();
        apply_workspace(dir.path(), std::slice::from_ref(&link)).unwrap();
        assert_eq!(
            std::fs::read_link(dir.path().join(".claude/skills/one")).unwrap(),
            target
        );
        // Refuses to clobber a real file.
        std::fs::remove_file(dir.path().join(".claude/skills/one")).unwrap();
        std::fs::write(dir.path().join(".claude/skills/one"), "data").unwrap();
        let error = apply_workspace(dir.path(), std::slice::from_ref(&link)).unwrap_err();
        assert!(
            matches!(&error, EngineError::Workspace { path, .. } if *path == dir.path().join(".claude/skills/one")),
            "{error:?}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".claude/skills/one")).unwrap(),
            "data"
        );
        // Refuses to clobber a directory.
        std::fs::remove_file(dir.path().join(".claude/skills/one")).unwrap();
        std::fs::create_dir(dir.path().join(".claude/skills/one")).unwrap();
        assert!(apply_workspace(dir.path(), std::slice::from_ref(&link)).is_err());
    }

    #[test]
    fn paths_escaping_cwd_are_refused_before_any_write() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("w");
        std::fs::create_dir(&cwd).unwrap();
        for bad in ["../x", "a/../../x", "/etc/passwd", "a/./../../x"] {
            let error = apply_workspace(
                &cwd,
                &[WorkspaceAction::Write {
                    path: bad.into(),
                    content: "x".into(),
                }],
            )
            .unwrap_err();
            assert!(
                matches!(error, EngineError::Workspace { .. }),
                "{bad}: {error:?}"
            );
            let error = apply_workspace(
                &cwd,
                &[WorkspaceAction::Symlink {
                    src: "/tmp".into(),
                    dst: bad.into(),
                }],
            )
            .unwrap_err();
            assert!(
                matches!(error, EngineError::Workspace { .. }),
                "{bad}: {error:?}"
            );
        }
        assert!(!dir.path().join("x").exists());
        // `a/../b` stays inside and is fine.
        apply_workspace(
            &cwd,
            &[WorkspaceAction::Write {
                path: "a/../b.txt".into(),
                content: "ok".into(),
            }],
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(cwd.join("b.txt")).unwrap(), "ok");
    }

    #[test]
    fn resolve_in_normalizes_cwd_and_refuses_relative_cwd() {
        let cwd = Path::new("/w/x/../root/");
        assert_eq!(
            resolve_in(cwd, Path::new("a")).unwrap(),
            Path::new("/w/root/a")
        );
        assert_eq!(
            resolve_in(cwd, Path::new("")).unwrap(),
            Path::new("/w/root")
        );
        assert_eq!(
            resolve_in(cwd, Path::new("/w/root/a")).unwrap(),
            Path::new("/w/root/a")
        );
        assert!(resolve_in(cwd, Path::new("/w/x/a")).is_err());
        for bad in ["", ".", "rel", "../x"] {
            assert!(
                resolve_in(Path::new(bad), Path::new("a")).is_err(),
                "{bad:?}"
            );
            assert!(
                resolve_in(Path::new(bad), Path::new("/etc/passwd")).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn write_under_a_file_parent_names_the_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "").unwrap();
        let error = apply_workspace(
            dir.path(),
            &[WorkspaceAction::Write {
                path: "f/inner.txt".into(),
                content: "x".into(),
            }],
        )
        .unwrap_err();
        assert!(
            matches!(&error, EngineError::Workspace { path, .. } if *path == dir.path().join("f/inner.txt")),
            "{error:?}"
        );
        assert!(error.to_string().contains("inner.txt"), "{error}");
    }

    #[test]
    fn empty_plan_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        apply_workspace(dir.path(), &[]).unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn refused_error_text() {
        let error = EngineError::Refused("no effort support".into());
        assert_eq!(error.to_string(), "no effort support");
    }
}
