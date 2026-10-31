//! Engines as plugins: one `LlmEngine` per coding-agent CLI, each a pure function from a
//! `LaunchRequest` to a `LaunchPlan`. Core applies the plan's workspace actions and starts the
//! process, so an adapter is unit-testable without a filesystem or tmux.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::error::CoreError;
use crate::profile::Profile;
use crate::session::{Session, validate_name};
use crate::state::{LlmEngineChange, LlmEngineHook};

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

/// Which conversation a launch opens.
#[derive(Clone, Copy, Debug)]
pub enum LaunchMode<'a> {
    /// A new conversation, beginning with the request's `initial_prompt`.
    Start,
    /// Continue the conversation the engine recorded in `session.ext[ext_key]`.
    Resume,
    /// Branch the conversation of `source` (the engine reads `source.ext[ext_key]`) into the
    /// launched session.
    Fork { source: &'a Session },
}

/// What core asks an engine to turn into a command line. Borrowed: `launch` is pure.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct LaunchRequest<'a> {
    /// The record being launched: the engine reads its id, `cwd` and its own `ext[ext_key]`.
    pub session: &'a Session,
    pub mode: LaunchMode<'a>,
    /// Combined role text, the file holding it, and the granted skill dirs.
    pub profile: &'a Profile,
    pub initial_prompt: Option<&'a str>,
    /// The argv prefix every hook the adapter wires runs, with the CLI's event name appended:
    /// `["/abs/txc", "chat", "hook"]`. Empty: wire no hooks.
    pub hook_command: &'a [String],
}

impl<'a> LaunchRequest<'a> {
    /// A `Start` of `session`. Options such as a model or an effort are not here: they are
    /// engine data, merged into `session.ext[ext_key]` by `LlmEngine::configure` beforehand.
    pub fn new(session: &'a Session, profile: &'a Profile) -> Self {
        Self {
            session,
            mode: LaunchMode::Start,
            profile,
            initial_prompt: None,
            hook_command: &[],
        }
    }

    pub fn mode(mut self, mode: LaunchMode<'a>) -> Self {
        self.mode = mode;
        self
    }

    pub fn initial_prompt(mut self, prompt: &'a str) -> Self {
        self.initial_prompt = Some(prompt);
        self
    }

    pub fn hook_command(mut self, hook_command: &'a [String]) -> Self {
        self.hook_command = hook_command;
        self
    }
}

/// The shell command a wired hook runs: every word of `hook_command` single-quoted, then
/// `event`, then `|| exit 1`. Claude reads exit 2 as "block the agent", so a `txc` that fails
/// to even start must never block a prompt or a stop.
pub fn hook_shell_command(hook_command: &[String], event: &str) -> String {
    let words: Vec<String> = hook_command
        .iter()
        .map(|word| shell_quote(word))
        .chain(std::iter::once(shell_quote(event)))
        .collect();
    format!("{} || exit 1", words.join(" "))
}

/// POSIX single-quoting: safe for any word (spaces, `$`, embedded `'`).
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
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

/// How to start the engine: `program args…`, extra environment, workspace preparation, and the
/// engine data known before the process starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LaunchPlan {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workspace: Vec<WorkspaceAction>,
    /// The complete value core stores under `ext[ext_key]` before the process starts (Claude:
    /// its id and transcript path are known up front). `None`: the record's data stays as it
    /// is, and anything new arrives by hook (Codex).
    pub engine_data: Option<serde_json::Value>,
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

    pub fn engine_data(mut self, engine_data: serde_json::Value) -> Self {
        self.engine_data = Some(engine_data);
        self
    }
}

/// One coding-agent engine. Five required methods; no I/O in `configure`, `launch` or `on_hook`.
/// `Send + Sync` so a core can be shared across threads.
pub trait LlmEngine: Send + Sync {
    /// A bare name (`[A-Za-z0-9_-]+`); stored in every record this engine launches.
    fn id(&self) -> &str;

    /// The `ext` key this engine owns. Core stores engine data under it and never reads it.
    /// `[a-z0-9-]+`, unique among engines and plugin ids.
    fn ext_key(&self) -> &'static str;

    /// Validates the caller's launch options and merges them into this session's engine data.
    /// `options` is a JSON object (`{"model": "fable", "effort": "high"}`); each engine owns the
    /// vocabulary, so an unknown key or a bad value is `EngineError::Refused`. Returns the
    /// complete new value of `session.ext[self.ext_key()]`, which core stores before `launch`
    /// reads it. Pure.
    fn configure(
        &self,
        session: &Session,
        options: &serde_json::Value,
    ) -> Result<serde_json::Value, EngineError>;

    fn launch(&self, req: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError>;

    /// How a message is framed before it is typed into one of this engine's sessions.
    /// `sender` is the operator (with where they typed, if known) or the sending agent's name.
    /// Pure; the default is the `<from-user>` / `<from-user session="…">` /
    /// `<from-agent session="…">` envelope.
    fn envelope(&self, sender: &crate::messaging::EnvelopeSender<'_>, text: &str) -> String {
        crate::messaging::build_envelope(sender, text)
    }

    /// A hook this adapter wired fired inside the session. Given the record (its current state is
    /// `session.state`, this engine's data is `session.ext[self.ext_key()]`, absent at first),
    /// say which state the agent moves to and what its engine data becomes; an empty change
    /// when the hook changes nothing. Pure: core applies the result under the session's lock.
    /// Required, because an engine that wires no hooks never leaves `Starting`; returning
    /// `LlmEngineChange::NONE` says so.
    fn on_hook(&self, session: &Session, hook: &LlmEngineHook) -> LlmEngineChange;
}

/// This engine's data in a record: `ext[ext_key]`, `Null` when absent. A non-object value (a
/// hand-edited record) counts as absent, so adapters can rely on `.get(..)` seeing an object.
pub fn engine_data<'a>(engine: &dyn LlmEngine, session: &'a Session) -> &'a serde_json::Value {
    const NULL: serde_json::Value = serde_json::Value::Null;
    match session.ext.get(engine.ext_key()) {
        Some(value @ serde_json::Value::Object(_)) => value,
        _ => &NULL,
    }
}

/// `InvalidExtKey` unless `key` is `[a-z0-9-]+`.
pub fn validate_ext_key(key: &str) -> Result<(), CoreError> {
    if key.is_empty()
        || !key
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(CoreError::InvalidExtKey(key.to_owned()));
    }
    Ok(())
}

/// id → adapter. Ids and ext keys are unique; registration order does not matter.
#[derive(Default)]
pub struct EngineRegistry {
    adapters: BTreeMap<String, Box<dyn LlmEngine>>,
}

impl EngineRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// `EngineTaken` on a duplicate id; `InvalidName` when the id is not a bare name;
    /// `ExtKeyTaken` when another engine owns the same `ext` key; `InvalidExtKey` when the key
    /// is not `[a-z0-9-]+`.
    pub fn register(&mut self, adapter: Box<dyn LlmEngine>) -> Result<(), CoreError> {
        let id = adapter.id().to_owned();
        validate_name(&id)?;
        validate_ext_key(adapter.ext_key())?;
        if self.adapters.contains_key(&id) {
            return Err(CoreError::EngineTaken(id));
        }
        if self
            .adapters
            .values()
            .any(|registered| registered.ext_key() == adapter.ext_key())
        {
            return Err(CoreError::ExtKeyTaken(adapter.ext_key().to_owned()));
        }
        self.adapters.insert(id, adapter);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&dyn LlmEngine> {
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

    impl LlmEngine for Stub {
        fn on_hook(&self, _: &Session, _: &crate::LlmEngineHook) -> LlmEngineChange {
            LlmEngineChange::NONE
        }

        fn id(&self) -> &str {
            self.0
        }

        fn ext_key(&self) -> &'static str {
            self.0
        }

        fn configure(
            &self,
            session: &Session,
            options: &serde_json::Value,
        ) -> Result<serde_json::Value, EngineError> {
            let mut data = engine_data(self, session)
                .as_object()
                .cloned()
                .unwrap_or_default();
            data.extend(options.as_object().cloned().unwrap_or_default());
            Ok(serde_json::Value::Object(data))
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

    struct Keyed(&'static str, &'static str);

    impl LlmEngine for Keyed {
        fn on_hook(&self, _: &Session, _: &crate::LlmEngineHook) -> LlmEngineChange {
            LlmEngineChange::NONE
        }

        fn id(&self) -> &str {
            self.0
        }

        fn ext_key(&self) -> &'static str {
            self.1
        }

        fn configure(
            &self,
            _: &Session,
            options: &serde_json::Value,
        ) -> Result<serde_json::Value, EngineError> {
            Ok(options.clone())
        }

        fn launch(&self, _: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
            Ok(LaunchPlan::default())
        }
    }

    #[test]
    fn registry_refuses_a_taken_or_invalid_ext_key() {
        let mut registry = EngineRegistry::new();
        registry.register(Box::new(Keyed("one", "one-1"))).unwrap();
        let error = registry
            .register(Box::new(Keyed("two", "one-1")))
            .unwrap_err();
        assert!(
            matches!(&error, CoreError::ExtKeyTaken(key) if key == "one-1"),
            "{error:?}"
        );
        for bad in ["", "Claude", "a_b", "a.b", "a/b", "ä"] {
            let error = registry
                .register(Box::new(Keyed("three", bad)))
                .unwrap_err();
            assert!(
                matches!(&error, CoreError::InvalidExtKey(key) if key == bad),
                "{bad:?}: {error:?}"
            );
        }
        assert_eq!(registry.ids(), ["one"]);
    }

    fn record(cwd: &Path) -> Session {
        use crate::session::{Launch, Lifecycle, SessionId};
        Session {
            id: SessionId::mint(),
            name: "s".into(),
            parent: None,
            tags: Vec::new(),
            cwd: cwd.to_owned(),
            created_at: 0.0,
            ended_at: None,
            last_modified_at: 0.0,
            lifecycle: Lifecycle::Running,
            launch: Launch::Process {
                program: "x".into(),
                args: Vec::new(),
            },
            state: None,
            ext: BTreeMap::new(),
        }
    }

    #[test]
    fn configure_sees_the_current_data_and_returns_the_merged_object() {
        let engine = Stub("e");
        let mut session = record(Path::new("/tmp"));
        session
            .ext
            .insert("e".into(), serde_json::json!({"conversation_id": "c"}));
        let merged = engine
            .configure(&session, &serde_json::json!({"model": "m"}))
            .unwrap();
        assert_eq!(
            merged,
            serde_json::json!({"conversation_id": "c", "model": "m"})
        );
    }

    #[test]
    fn engine_data_is_the_object_under_the_key_or_null() {
        let engine = Keyed("e", "e");
        let mut session = record(Path::new("/tmp"));
        assert_eq!(engine_data(&engine, &session), &serde_json::Value::Null);
        session.ext.insert("e".into(), serde_json::json!("text"));
        assert_eq!(engine_data(&engine, &session), &serde_json::Value::Null);
        session.ext.insert("e".into(), serde_json::json!({"k": 1}));
        assert_eq!(engine_data(&engine, &session), &serde_json::json!({"k": 1}));
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
        let session = record(dir.path());
        let source = record(dir.path());
        let request = LaunchRequest::new(&session, &profile).initial_prompt("go");
        assert!(matches!(request.mode, LaunchMode::Start));
        assert_eq!(request.initial_prompt, Some("go"));
        let request = request.mode(LaunchMode::Fork { source: &source });
        assert!(
            matches!(request.mode, LaunchMode::Fork { source: forked } if forked.id == source.id)
        );
        assert_eq!(request.session.id, session.id);
        let plan = Stub("x").launch(&request).unwrap();
        assert_eq!(plan.program, "x");
        assert_eq!(plan.args, [dir.path().join("p.md").display().to_string()]);
        assert!(plan.env.is_empty() && plan.workspace.is_empty() && plan.engine_data.is_none());
        let plan = LaunchPlan::new("y").engine_data(serde_json::json!({"k": 1}));
        assert_eq!(plan.engine_data, Some(serde_json::json!({"k": 1})));
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
    fn hook_shell_command_quotes_every_word_and_never_exits_2() {
        let prefix = [
            "/a b/it's/txc".to_owned(),
            "chat".to_owned(),
            "hook".to_owned(),
        ];
        assert_eq!(
            hook_shell_command(&prefix, "Stop"),
            "'/a b/it'\\''s/txc' 'chat' 'hook' 'Stop' || exit 1"
        );
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
