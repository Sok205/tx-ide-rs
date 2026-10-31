//! Engines as plugins: one `LlmEngine` per coding-agent CLI, each a pure function from a
//! `LaunchRequest` to a `LaunchPlan`. Core applies the plan's workspace actions and starts the
//! process, so an adapter is unit-testable without a filesystem or tmux.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CoreError;
use crate::profile::Profile;
use crate::session::Session;
use crate::state::{LlmEngineChange, LlmEngineHook, LlmEngineState};

/// An adapter could not honor a request, or a workspace action could not be applied.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// The adapter refuses the request (an unsupported option, say).
    #[error("{0}")]
    Refused(String),
    #[error("workspace action on {}: {reason}", path.display())]
    Workspace { path: PathBuf, reason: String },
    /// A file operation in `archive` or `restore` failed; `path` names the file.
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// What core asks an engine to turn into a command line, the same for a start, a resume and
/// a fork; which one is the method called, and a prompt is its argument. Borrowed: the launch
/// methods are pure.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct LaunchRequest<'a> {
    /// The record being launched: the engine reads its id, `cwd` and its own `ext[ext_key]`.
    pub session: &'a Session,
    /// Combined role text, the file holding it, and the granted skill dirs.
    pub profile: &'a Profile,
    /// The argv prefix every hook the adapter wires runs, with the CLI's event name appended:
    /// `["/abs/txc", "chat", "hook"]`. Empty: wire no hooks.
    pub hook_command: &'a [String],
}

impl<'a> LaunchRequest<'a> {
    /// A launch of `session`. Options such as a model or an effort are not here: they are
    /// engine data, merged into `session.ext[ext_key]` by `LlmEngine::configure` beforehand.
    pub fn new(session: &'a Session, profile: &'a Profile) -> Self {
        Self {
            session,
            profile,
            hook_command: &[],
        }
    }

    pub fn hook_command(mut self, hook_command: &'a [String]) -> Self {
        self.hook_command = hook_command;
        self
    }
}

/// One thing core types into an engine's pane: a line of text, submitted with `Enter` when
/// `submit` says so, or a single key. What an engine answers for a message, a stop and an
/// interrupt; core only knows how to type it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Text { text: String, submit: bool },
    Key(Key),
}

impl Input {
    /// `text`, submitted.
    pub fn line(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            submit: true,
        }
    }
}

/// A key core can press in a pane, by tmux's name for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Escape,
    Enter,
    ControlC,
    ControlD,
}

impl Key {
    /// The name `tmux send-keys` takes.
    pub fn tmux_name(self) -> &'static str {
        match self {
            Key::Escape => "Escape",
            Key::Enter => "Enter",
            Key::ControlC => "C-c",
            Key::ControlD => "C-d",
        }
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

/// The conversation a record's engine data names: the engine's own id for it and where the CLI
/// keeps its live transcript. Readers (the timeline, `chat ls`) ask for it through
/// [`crate::TxCore::conversation`]; only the engine knows how its data spells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conversation {
    pub id: String,
    pub transcript_path: PathBuf,
}

/// The engines tx knows, a closed set: a record names its engine with one of these, and each
/// engine plugin declares the kind it provides. Serialized as its lowercase name (`"claude"`,
/// `"codex"`), which is also how `--engine` spells it. A new engine is a new variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineKind {
    Claude,
    Codex,
}

impl EngineKind {
    /// Every kind, in listing order.
    pub const ALL: [EngineKind; 2] = [EngineKind::Claude, EngineKind::Codex];

    /// The lowercase name: `"claude"`, `"codex"`.
    pub fn as_str(self) -> &'static str {
        match self {
            EngineKind::Claude => "claude",
            EngineKind::Codex => "codex",
        }
    }

    /// `"claude, codex"`: the names an error lists.
    pub fn names() -> String {
        Self::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl fmt::Display for EngineKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EngineKind {
    type Err = CoreError;

    /// `InvalidEngineKind` unless `text` is a kind's lowercase name.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.as_str() == text)
            .ok_or_else(|| CoreError::InvalidEngineKind(text.to_owned()))
    }
}

/// One coding-agent engine. Seven required methods; no I/O in `configure`, the three launch
/// methods, `on_hook` or the three input methods. `archive` and `restore` are the two methods
/// that touch the filesystem. `Send + Sync` so a core can be shared across threads.
pub trait LlmEngine: Send + Sync {
    /// The kind this engine provides; stored in every record it launches, and the registry's
    /// key.
    fn kind(&self) -> EngineKind;

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

    /// A new conversation for `request.session`, opened with `prompt` (never empty: core
    /// requires one). Pure.
    fn start(&self, request: &LaunchRequest<'_>, prompt: &str) -> Result<LaunchPlan, EngineError>;

    /// Continue the conversation this engine recorded in `request.session.ext[ext_key]`. Pure.
    fn resume(&self, request: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError>;

    /// Branch the conversation of `source` (the engine reads `source.ext[ext_key]`) into
    /// `request.session`, whose data starts as a copy of the source's, and open it with
    /// `prompt`. Pure.
    fn fork(
        &self,
        request: &LaunchRequest<'_>,
        source: &Session,
        prompt: &str,
    ) -> Result<LaunchPlan, EngineError>;

    /// What to type to deliver a message to one of this engine's sessions: the framed text
    /// and whether to submit it. `sender` is the operator (with where they typed, if known) or
    /// the sending agent's name. Pure; the default is the `<from-user>` /
    /// `<from-user session="…">` / `<from-agent session="…">` envelope, submitted.
    fn receive_message(&self, sender: &crate::messaging::EnvelopeSender<'_>, text: &str) -> Input {
        Input::line(crate::messaging::build_envelope(sender, text))
    }

    /// What to type so the CLI exits cleanly, closing its transcript; `None`: it has no such
    /// input, core kills the process. Core waits for the exit either way and kills what is
    /// still there after its grace period. Pure. The default knows none.
    fn stop(&self, session: &Session) -> Option<Input> {
        let _ = session;
        None
    }

    /// What to type to abort the running turn, and the state the agent is in once it landed
    /// (`Some(Idle)`: at its prompt; `None`: say nothing, a hook will), since an aborted turn
    /// need not fire the CLI's end-of-turn hook. Core applies the state as it does a hook's.
    /// `None` as a whole: not supported. Pure. The default knows none.
    fn interrupt(&self, session: &Session) -> Option<(Input, Option<LlmEngineState>)> {
        let _ = session;
        None
    }

    /// A hook this adapter wired fired inside the session. Given the record (its current state is
    /// `session.state`, this engine's data is `session.ext[self.ext_key()]`, absent at first),
    /// say which state the agent moves to and what its engine data becomes; an empty change
    /// when the hook changes nothing. Pure: core applies the result under the session's lock.
    /// Required, because an engine that wires no hooks never leaves `Starting`; returning
    /// `LlmEngineChange::NONE` says so.
    fn on_hook(&self, session: &Session, hook: &LlmEngineHook) -> LlmEngineChange;

    /// Bring `archive_directory` up to date with this session's conversation: the transcript
    /// (append-by-offset, [`crate::mirror::mirror_file`]) and its sidecars (copy-if-absent,
    /// [`crate::mirror::mirror_tree`]), keyed by conversation so every past conversation of the
    /// session is kept. Core calls it after every hook, outside the session lock, under a
    /// non-blocking per-session lock of its own, so it must be cheap when nothing changed; and
    /// once more after a stop or an exit, when `session.lifecycle` is terminal: the process is
    /// gone, the transcript final, and the copy must be exact, so the engine compares it whole
    /// ([`crate::mirror::mirror_file_exact`]) rather than by its trailing window. Idempotent; a
    /// missing live transcript archives nothing. The default archives nothing.
    fn archive(&self, session: &Session, archive_directory: &Path) -> Result<(), EngineError> {
        let _ = (session, archive_directory);
        Ok(())
    }

    /// Before a resume: if the CLI's own copy of the conversation is missing, put the archived
    /// one back where the CLI expects it. Never overwrites a live transcript. Returns whether
    /// anything was restored. The default restores nothing.
    fn restore(&self, session: &Session, archive_directory: &Path) -> Result<bool, EngineError> {
        let _ = (session, archive_directory);
        Ok(false)
    }

    /// The conversation this session's engine data names, for readers (the timeline, `chat
    /// ls`). `None` while it is not known yet (a Codex agent before its first `SessionStart`).
    /// Pure. The default knows none.
    fn conversation(&self, session: &Session) -> Option<Conversation> {
        let _ = session;
        None
    }
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

/// kind → adapter. One adapter per kind, ext keys unique; registration order does not matter.
/// A kind nobody registered (its plugin disabled) is simply absent.
#[derive(Default)]
pub struct EngineRegistry {
    adapters: BTreeMap<EngineKind, Box<dyn LlmEngine>>,
}

impl EngineRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// `EngineTaken` when the kind is registered already; `ExtKeyTaken` when another engine
    /// owns the same `ext` key; `InvalidExtKey` when the key is not `[a-z0-9-]+`.
    pub fn register(&mut self, adapter: Box<dyn LlmEngine>) -> Result<(), CoreError> {
        let kind = adapter.kind();
        validate_ext_key(adapter.ext_key())?;
        if self.adapters.contains_key(&kind) {
            return Err(CoreError::EngineTaken(kind));
        }
        if self
            .adapters
            .values()
            .any(|registered| registered.ext_key() == adapter.ext_key())
        {
            return Err(CoreError::ExtKeyTaken(adapter.ext_key().to_owned()));
        }
        self.adapters.insert(kind, adapter);
        Ok(())
    }

    pub fn get(&self, kind: EngineKind) -> Option<&dyn LlmEngine> {
        self.adapters.get(&kind).map(Box::as_ref)
    }

    /// Registered kinds, sorted.
    pub fn kinds(&self) -> Vec<EngineKind> {
        self.adapters.keys().copied().collect()
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

    /// An engine of the kind, owning the ext key of the same name.
    struct Stub(EngineKind);

    impl LlmEngine for Stub {
        fn on_hook(&self, _: &Session, _: &crate::LlmEngineHook) -> LlmEngineChange {
            LlmEngineChange::NONE
        }

        fn kind(&self) -> EngineKind {
            self.0
        }

        fn ext_key(&self) -> &'static str {
            self.0.as_str()
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

        fn start(
            &self,
            request: &LaunchRequest<'_>,
            prompt: &str,
        ) -> Result<LaunchPlan, EngineError> {
            Ok(LaunchPlan {
                program: self.0.to_string(),
                args: vec![
                    request.profile.file().display().to_string(),
                    prompt.to_owned(),
                ],
                ..Default::default()
            })
        }

        fn resume(&self, request: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
            self.start(request, "")
        }

        fn fork(
            &self,
            request: &LaunchRequest<'_>,
            source: &Session,
            prompt: &str,
        ) -> Result<LaunchPlan, EngineError> {
            self.start(request, prompt)
                .map(|plan| plan.arg(source.id.as_str()))
        }
    }

    #[test]
    fn registry_refuses_a_duplicate_kind_and_lists_kinds_sorted() {
        let mut registry = EngineRegistry::new();
        registry
            .register(Box::new(Stub(EngineKind::Codex)))
            .unwrap();
        assert!(registry.get(EngineKind::Claude).is_none());
        registry
            .register(Box::new(Stub(EngineKind::Claude)))
            .unwrap();
        let error = registry
            .register(Box::new(Keyed(EngineKind::Codex, "other")))
            .unwrap_err();
        assert!(
            matches!(&error, CoreError::EngineTaken(EngineKind::Codex)),
            "{error:?}"
        );
        assert_eq!(registry.kinds(), EngineKind::ALL);
        assert_eq!(
            registry.get(EngineKind::Claude).unwrap().kind(),
            EngineKind::Claude
        );
        assert!(EngineRegistry::default().kinds().is_empty());
    }

    #[test]
    fn engine_kind_names_round_trip_and_an_unknown_one_is_refused() {
        for kind in EngineKind::ALL {
            assert_eq!(kind.as_str().parse::<EngineKind>().unwrap(), kind);
            assert_eq!(kind.to_string(), kind.as_str());
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("{:?}", kind.as_str()));
            assert_eq!(serde_json::from_str::<EngineKind>(&json).unwrap(), kind);
        }
        assert_eq!(EngineKind::names(), "claude, codex");
        for bad in ["", "Claude", "kodex", " codex"] {
            let error = bad.parse::<EngineKind>().unwrap_err();
            assert!(
                matches!(&error, CoreError::InvalidEngineKind(text) if text == bad),
                "{bad:?}: {error:?}"
            );
            assert!(error.to_string().contains("claude, codex"), "{error}");
            assert!(serde_json::from_str::<EngineKind>(&format!("{bad:?}")).is_err());
        }
    }

    /// An engine of the kind owning an arbitrary ext key.
    struct Keyed(EngineKind, &'static str);

    impl LlmEngine for Keyed {
        fn on_hook(&self, _: &Session, _: &crate::LlmEngineHook) -> LlmEngineChange {
            LlmEngineChange::NONE
        }

        fn kind(&self) -> EngineKind {
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

        fn start(&self, _: &LaunchRequest<'_>, _: &str) -> Result<LaunchPlan, EngineError> {
            Ok(LaunchPlan::default())
        }

        fn resume(&self, _: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
            Ok(LaunchPlan::default())
        }

        fn fork(
            &self,
            _: &LaunchRequest<'_>,
            _: &Session,
            _: &str,
        ) -> Result<LaunchPlan, EngineError> {
            Ok(LaunchPlan::default())
        }
    }

    #[test]
    fn registry_refuses_a_taken_or_invalid_ext_key() {
        let mut registry = EngineRegistry::new();
        registry
            .register(Box::new(Keyed(EngineKind::Claude, "one-1")))
            .unwrap();
        let error = registry
            .register(Box::new(Keyed(EngineKind::Codex, "one-1")))
            .unwrap_err();
        assert!(
            matches!(&error, CoreError::ExtKeyTaken(key) if key == "one-1"),
            "{error:?}"
        );
        for bad in ["", "Claude", "a_b", "a.b", "a/b", "ä"] {
            let error = registry
                .register(Box::new(Keyed(EngineKind::Codex, bad)))
                .unwrap_err();
            assert!(
                matches!(&error, CoreError::InvalidExtKey(key) if key == bad),
                "{bad:?}: {error:?}"
            );
        }
        assert_eq!(registry.kinds(), [EngineKind::Claude]);
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
            state: crate::state::LlmEngineState::Starting,
            ext: BTreeMap::new(),
        }
    }

    #[test]
    fn configure_sees_the_current_data_and_returns_the_merged_object() {
        let engine = Stub(EngineKind::Claude);
        let mut session = record(Path::new("/tmp"));
        session
            .ext
            .insert("claude".into(), serde_json::json!({"conversation_id": "c"}));
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
        let engine = Keyed(EngineKind::Claude, "e");
        let mut session = record(Path::new("/tmp"));
        assert_eq!(engine_data(&engine, &session), &serde_json::Value::Null);
        session.ext.insert("e".into(), serde_json::json!("text"));
        assert_eq!(engine_data(&engine, &session), &serde_json::Value::Null);
        session.ext.insert("e".into(), serde_json::json!({"k": 1}));
        assert_eq!(engine_data(&engine, &session), &serde_json::json!({"k": 1}));
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
        let hooks = ["txi".to_owned(), "hook".to_owned()];
        let request = LaunchRequest::new(&session, &profile).hook_command(&hooks);
        assert_eq!(request.session.id, session.id);
        assert_eq!(request.hook_command, &hooks);
        let profile_file = dir.path().join("p.md").display().to_string();
        let plan = Stub(EngineKind::Claude).start(&request, "go").unwrap();
        assert_eq!(plan.program, "claude");
        assert_eq!(plan.args, [profile_file.clone(), "go".to_owned()]);
        assert!(plan.env.is_empty() && plan.workspace.is_empty() && plan.engine_data.is_none());
        let plan = Stub(EngineKind::Claude).resume(&request).unwrap();
        assert_eq!(plan.args, [profile_file.clone(), String::new()]);
        let plan = Stub(EngineKind::Claude)
            .fork(&request, &source, "branch")
            .unwrap();
        assert_eq!(
            plan.args,
            [profile_file, "branch".to_owned(), source.id.to_string()]
        );
        let plan = LaunchPlan::new("y").engine_data(serde_json::json!({"k": 1}));
        assert_eq!(plan.engine_data, Some(serde_json::json!({"k": 1})));
    }

    #[test]
    fn input_defaults_are_the_envelope_submitted_and_no_stop_or_interrupt() {
        use crate::messaging::EnvelopeSender;
        let engine = Stub(EngineKind::Claude);
        let session = record(Path::new("/tmp"));
        assert_eq!(
            engine.receive_message(&EnvelopeSender::User { via: None }, "hi"),
            Input::Text {
                text: "<from-user>hi</from-user>".into(),
                submit: true
            }
        );
        assert_eq!(
            engine.receive_message(&EnvelopeSender::Agent { name: "a" }, "hi"),
            Input::line("<from-agent session=\"a\">hi</from-agent>")
        );
        assert_eq!(engine.stop(&session), None);
        assert_eq!(engine.interrupt(&session), None);
        assert_eq!(Key::Escape.tmux_name(), "Escape");
        assert_eq!(Key::Enter.tmux_name(), "Enter");
        assert_eq!(Key::ControlC.tmux_name(), "C-c");
        assert_eq!(Key::ControlD.tmux_name(), "C-d");
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
