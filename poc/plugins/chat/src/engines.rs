//! Engine-specific layouts: where each engine keeps a chat's transcript, which sidecar files
//! travel with it, the Codex rollout reader, and the argv rewrites a fork needs. Launch argv
//! comes from the engine crates' pure `LlmEngine::launch`, used as library code.

use std::path::{Path, PathBuf};

use tx_core::{
    Launch, LaunchMode, LaunchPlan, LaunchRequest, Lifecycle, LlmEngine, Profile, Profiles,
    Session, SessionId, TxCore,
};
pub use tx_engine_claude::{munge, realpath};

use crate::ChatError;
use crate::ext::AgentLaunch;

pub const BUNDLE_TRANSCRIPT_NAME: &str = "transcript.jsonl";
const TRANSCRIPT_SUFFIX: &str = ".jsonl";
const ROLLOUT_PREFIX: &str = "rollout-";

/// Where the engines keep their chats. Built once from the environment by verbs; the library
/// never reads the environment itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locator {
    pub claude_home: PathBuf,
    pub codex_home: PathBuf,
}

impl Locator {
    /// `CLAUDE_CONFIG_DIR` | `$HOME/.claude`; `CODEX_HOME` | `$HOME/.codex`.
    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let pick = |var: &str, default: &str| match std::env::var_os(var) {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => home.join(default),
        };
        Self {
            claude_home: pick("CLAUDE_CONFIG_DIR", ".claude"),
            codex_home: pick("CODEX_HOME", ".codex"),
        }
    }

    fn claude_projects(&self) -> PathBuf {
        self.claude_home.join("projects")
    }

    fn claude_transcript(&self, chat_id: &str, cwd: &str) -> PathBuf {
        self.claude_projects()
            .join(munge(&realpath(Path::new(cwd)).to_string_lossy()))
            .join(format!("{chat_id}{TRANSCRIPT_SUFFIX}"))
    }

    /// A chat's source transcript, or `None` if it is not on disk yet. Claude: the project dir
    /// of `cwd_hint` when the file is there, else a glob of `projects/*/<chat>.jsonl` preferring
    /// the hint's dir. Codex: the newest `sessions/**/rollout-*-<chat>.jsonl`, never Claude's
    /// projects root. Any other engine resolves nothing.
    pub fn locate_transcript(
        &self,
        engine: &str,
        chat_id: &str,
        cwd_hint: Option<&str>,
    ) -> Option<PathBuf> {
        match engine {
            "claude" => self.locate_claude(chat_id, cwd_hint),
            "codex" => self.find_rollout(chat_id),
            _ => None,
        }
    }

    fn locate_claude(&self, chat_id: &str, cwd_hint: Option<&str>) -> Option<PathBuf> {
        let cwd_hint = cwd_hint.filter(|cwd| !cwd.is_empty());
        if let Some(cwd) = cwd_hint {
            let fast = self.claude_transcript(chat_id, cwd);
            if fast.exists() {
                return Some(fast);
            }
        }
        let file_name = format!("{chat_id}{TRANSCRIPT_SUFFIX}");
        let mut matches: Vec<PathBuf> = std::fs::read_dir(self.claude_projects())
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path().join(&file_name))
            .filter(|path| std::fs::symlink_metadata(path).is_ok())
            .collect();
        matches.sort();
        if matches.len() > 1
            && let Some(cwd) = cwd_hint
        {
            let preferred = self.claude_transcript(chat_id, cwd);
            if matches.contains(&preferred) {
                return Some(preferred);
            }
        }
        matches.into_iter().next()
    }

    fn find_rollout(&self, chat_id: &str) -> Option<PathBuf> {
        let suffix = format!("-{chat_id}{TRANSCRIPT_SUFFIX}");
        let mut matches = Vec::new();
        collect_rollouts(&self.codex_home.join("sessions"), &suffix, &mut matches);
        matches.sort();
        matches.pop()
    }
}

/// Hidden entries included, symlinked dirs not descended.
fn collect_rollouts(directory: &Path, suffix: &str, matches: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.len() >= ROLLOUT_PREFIX.len() + suffix.len()
            && name.starts_with(ROLLOUT_PREFIX)
            && name.ends_with(suffix)
        {
            matches.push(entry.path());
        }
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            collect_rollouts(&entry.path(), suffix, matches);
        }
    }
}

/// Files that travel with a transcript into a bundle: Claude's sibling `<chat-id>/` dir
/// (subagents, tool results), relative to the resolved transcript. Codex inlines everything.
pub fn bundle_sidecars(engine: &str, src_transcript: &Path, chat_id: &str) -> Vec<PathBuf> {
    match engine {
        "claude" => {
            let parent = src_transcript.parent().unwrap_or(Path::new(""));
            vec![parent.join(chat_id)]
        }
        _ => Vec::new(),
    }
}

/// Copies Claude's cwd-keyed chat files so a resume or fork can start in another directory.
/// Other engines key nothing on the cwd. Only missing files are copied: an existing
/// destination may be a newer live chat and is never overwritten.
pub fn prepare_chat_for_cwd(
    loc: &Locator,
    engine: &str,
    chat_id: &str,
    src_cwd: &Path,
    dst_cwd: &Path,
) -> Result<(), ChatError> {
    if engine != "claude" || realpath(src_cwd) == realpath(dst_cwd) {
        return Ok(());
    }
    let source = loc.claude_transcript(chat_id, &src_cwd.to_string_lossy());
    if !source.is_file() {
        return Ok(());
    }
    let target = loc.claude_transcript(chat_id, &dst_cwd.to_string_lossy());
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(io(parent))?;
    }
    copy2_if_absent(&source, &target)?;
    let sidecars = source.with_extension("");
    if sidecars.is_dir() {
        copytree(&sidecars, &target.with_extension(""))?;
    }
    Ok(())
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> ChatError + '_ {
    move |source| ChatError::Io {
        path: path.to_owned(),
        source,
    }
}

/// Content, permission bits, and access / modification times; an existing target is kept.
fn copy2_if_absent(source: &Path, target: &Path) -> Result<(), ChatError> {
    if target.symlink_metadata().is_ok() {
        return Ok(());
    }
    std::fs::copy(source, target).map_err(io(target))?;
    copy_times(source, target)
}

fn copy_times(source: &Path, target: &Path) -> Result<(), ChatError> {
    let metadata = std::fs::metadata(source).map_err(io(source))?;
    let times = std::fs::FileTimes::new()
        .set_accessed(metadata.accessed().map_err(io(source))?)
        .set_modified(metadata.modified().map_err(io(source))?);
    let file = std::fs::File::options()
        .write(!metadata.is_dir())
        .read(metadata.is_dir())
        .open(target)
        .map_err(io(target))?;
    file.set_times(times).map_err(io(target))
}

/// Recursive merge into an existing tree, symlinks followed; only missing entries are copied.
fn copytree(source: &Path, target: &Path) -> Result<(), ChatError> {
    let created = target.symlink_metadata().is_err();
    std::fs::create_dir_all(target).map_err(io(target))?;
    for entry in std::fs::read_dir(source).map_err(io(source))? {
        let entry = entry.map_err(io(source))?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if from.is_dir() {
            copytree(&from, &to)?;
        } else {
            copy2_if_absent(&from, &to)?;
        }
    }
    if !created {
        return Ok(());
    }
    let permissions = std::fs::metadata(source).map_err(io(source))?.permissions();
    std::fs::set_permissions(target, permissions).map_err(io(target))?;
    copy_times(source, target)
}

/// The engine crates' adapters, used as library code. Their homes are left empty: chat only
/// launches through them to resume a chat id it already has, which reads no home.
pub fn adapter(engine: &str) -> Option<Box<dyn LlmEngine>> {
    match engine {
        "claude" => Some(Box::new(tx_engine_claude::ClaudeAdapter::new(
            PathBuf::new(),
        ))),
        "codex" => Some(Box::new(tx_engine_codex::CodexAdapter::new(PathBuf::new()))),
        _ => None,
    }
}

/// The argv `launch` would run for a new, resumed or primed chat: the profile is resolved and
/// written to `profile_file`, then the adapter turns the request into a plan. No state hooks
/// are wired: the plan runs as a process session, which has no engine to read them.
pub fn agent_plan(
    core: &TxCore,
    launch: &AgentLaunch,
    cwd: &Path,
    profile_file: &Path,
    resume: Option<&str>,
    prompt: Option<&str>,
) -> Result<LaunchPlan, ChatError> {
    let adapter =
        adapter(&launch.engine).ok_or_else(|| ChatError::EngineUnknown(launch.engine.clone()))?;
    let config = core.config();
    let resolved = Profiles::new(config.profile_dirs.clone(), config.skill_dirs.clone())
        .resolve(&launch.profile)
        .map_err(tx_core::CoreError::from)?;
    let profile = Profile::write(resolved, profile_file).map_err(io(profile_file))?;
    let record = scratch_record(launch, cwd, adapter.ext_key(), resume);
    let mut request = LaunchRequest::new(&record, &profile);
    if let Some(model) = launch.model.as_deref() {
        request = request.model(model);
    }
    if let Some(effort) = launch.effort {
        request = request.effort(effort);
    }
    if resume.is_some() {
        request = request.mode(LaunchMode::Resume);
    }
    if let Some(prompt) = prompt {
        request = request.initial_prompt(prompt);
    }
    Ok(adapter.launch(&request)?)
}

/// The record a chat launch hands the engine: the plan runs as a process session, created only
/// afterwards, so the engine gets a fresh id, `cwd`, and `resume` as its conversation.
fn scratch_record(
    launch: &AgentLaunch,
    cwd: &Path,
    ext_key: &str,
    resume: Option<&str>,
) -> Session {
    let mut ext = std::collections::BTreeMap::new();
    if let Some(resume) = resume {
        ext.insert(
            ext_key.to_owned(),
            serde_json::json!({ "conversation_id": resume }),
        );
    }
    Session {
        id: SessionId::mint(),
        name: String::new(),
        parent: None,
        tags: Vec::new(),
        cwd: cwd.to_owned(),
        created_at: 0.0,
        ended_at: None,
        last_modified_at: 0.0,
        lifecycle: Lifecycle::Running,
        launch: Launch::Agent {
            engine: launch.engine.clone(),
            profile: launch.profile.clone(),
            model: launch.model.clone(),
            effort: launch.effort,
            resume: resume.map(str::to_owned),
        },
        state: None,
        ext,
    }
}

/// Turns a resume argv into a fork argv. Claude: `--fork-session` right after `--resume <id>`.
/// Codex: the leading `resume` subcommand becomes `fork`.
pub fn fork_args(engine: &str, args: &mut Vec<String>) {
    match engine {
        "claude" => {
            if let Some(at) = args.iter().position(|arg| arg == "--resume") {
                let end = (at + 2).min(args.len());
                args.insert(end, "--fork-session".to_owned());
            }
        }
        "codex" => {
            if let Some(first) = args.first_mut().filter(|first| *first == "resume") {
                *first = "fork".to_owned();
            }
        }
        _ => {}
    }
}

/// Codex rollout reader (codex_rollout.py). Records are `{type, timestamp, payload}`; the
/// first line is always the `session_meta`.
pub mod rollout {
    use std::io;
    use std::path::{Path, PathBuf};

    use serde_json::{Map, Value};

    pub const SESSION_META_TYPE: &str = "session_meta";
    /// A conversation turn (an OpenAI Responses item); other payload types are skipped.
    pub const MESSAGE_TYPE: &str = "message";
    pub const MESSAGE_ROLES: [&str; 3] = ["developer", "user", "assistant"];
    /// user / developer turns hold text in input_text, assistant turns in output_text.
    pub const TEXT_BLOCK_TYPES: [&str; 2] = ["input_text", "output_text"];
    /// Codex injects the first user turn as an `<environment_context>` block.
    pub const ENVIRONMENT_CONTEXT_TAG: &str = "<environment_context>";

    #[derive(Debug, thiserror::Error)]
    pub enum RolloutError {
        #[error("{}: {source}", path.display())]
        Io {
            path: PathBuf,
            #[source]
            source: io::Error,
        },
        #[error("{}: invalid JSON: {source}", path.display())]
        Json {
            path: PathBuf,
            #[source]
            source: serde_json::Error,
        },
        #[error("{}: first record is {found}, expected {}", path.display(), py_repr_str(SESSION_META_TYPE))]
        NotSessionMeta { path: PathBuf, found: String },
        #[error("{}: empty rollout — no {SESSION_META_TYPE} record", path.display())]
        Empty { path: PathBuf },
        /// A record lacks a key (or is not an object where one is indexed).
        #[error("{}: malformed record: missing {}", path.display(), py_repr_str(.key))]
        Malformed { path: PathBuf, key: &'static str },
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct CodexMessage {
        pub role: String,
        pub text: String,
        pub is_environment_context: bool,
    }

    /// The first-line `session_meta` payload (session id, cwd, and on a fork the
    /// forked_from_id).
    pub fn read_session_meta(path: &Path) -> Result<Value, RolloutError> {
        let Some(record) = records(path)?.into_iter().next() else {
            return Err(RolloutError::Empty {
                path: path.to_owned(),
            });
        };
        let kind = field(path, &record, "type")?;
        if kind.as_str() != Some(SESSION_META_TYPE) {
            return Err(RolloutError::NotSessionMeta {
                path: path.to_owned(),
                found: py_repr(kind),
            });
        }
        Ok(field(path, &record, "payload")?.clone())
    }

    /// The conversation turns in order; the injected first-user `<environment_context>` is
    /// flagged, not dropped.
    pub fn iter_messages(path: &Path) -> Result<Vec<CodexMessage>, RolloutError> {
        let mut messages = Vec::new();
        for record in records(path)? {
            let payload = field(path, &record, "payload")?;
            let Some(payload) = payload.as_object() else {
                return Err(malformed(path, "type"));
            };
            if payload.get("type").and_then(Value::as_str) != Some(MESSAGE_TYPE) {
                continue;
            }
            let role = payload.get("role").ok_or_else(|| malformed(path, "role"))?;
            let Some(role) = role.as_str().filter(|role| MESSAGE_ROLES.contains(role)) else {
                continue;
            };
            let content = payload
                .get("content")
                .ok_or_else(|| malformed(path, "content"))?;
            let text = message_text(path, content)?;
            let is_environment_context =
                role == "user" && py_lstrip(&text).starts_with(ENVIRONMENT_CONTEXT_TAG);
            messages.push(CodexMessage {
                role: role.to_owned(),
                text,
                is_environment_context,
            });
        }
        Ok(messages)
    }

    fn records(path: &Path) -> Result<Vec<Value>, RolloutError> {
        let text = std::fs::read_to_string(path).map_err(|source| RolloutError::Io {
            path: path.to_owned(),
            source,
        })?;
        text.split_inclusive('\n')
            .map(py_strip)
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_str(line).map_err(|source| RolloutError::Json {
                    path: path.to_owned(),
                    source,
                })
            })
            .collect()
    }

    fn field<'a>(
        path: &Path,
        record: &'a Value,
        key: &'static str,
    ) -> Result<&'a Value, RolloutError> {
        record.get(key).ok_or_else(|| malformed(path, key))
    }

    fn malformed(path: &Path, key: &'static str) -> RolloutError {
        RolloutError::Malformed {
            path: path.to_owned(),
            key,
        }
    }

    fn message_text(path: &Path, content: &Value) -> Result<String, RolloutError> {
        let blocks = content
            .as_array()
            .ok_or_else(|| malformed(path, "content"))?;
        let mut text = String::new();
        for block in blocks {
            let block: &Map<String, Value> =
                block.as_object().ok_or_else(|| malformed(path, "type"))?;
            let kind = block.get("type").ok_or_else(|| malformed(path, "type"))?;
            if kind
                .as_str()
                .is_some_and(|kind| TEXT_BLOCK_TYPES.contains(&kind))
            {
                let part = block.get("text").ok_or_else(|| malformed(path, "text"))?;
                text.push_str(part.as_str().ok_or_else(|| malformed(path, "text"))?);
            }
        }
        Ok(text)
    }

    /// `str.strip()` (Python whitespace).
    fn py_strip(line: &str) -> &str {
        line.trim_matches(is_py_space)
    }

    fn py_lstrip(text: &str) -> &str {
        text.trim_start_matches(is_py_space)
    }

    fn is_py_space(c: char) -> bool {
        c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
    }

    /// Python `repr()` of a JSON-decoded scalar; containers fall back to compact JSON.
    fn py_repr(value: &Value) -> String {
        match value {
            Value::Null => "None".into(),
            Value::Bool(true) => "True".into(),
            Value::Bool(false) => "False".into(),
            Value::String(text) => py_repr_str(text),
            other => other.to_string(),
        }
    }

    /// Python `repr(str)`: single quotes unless the text has `'` and no `"`; non-printable
    /// characters escaped, printable non-ASCII kept.
    fn py_repr_str(s: &str) -> String {
        let quote = if s.contains('\'') && !s.contains('"') {
            '"'
        } else {
            '\''
        };
        let mut out = String::with_capacity(s.len() + 2);
        out.push(quote);
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c == quote => {
                    out.push('\\');
                    out.push(c);
                }
                c if is_printable(c) => out.push(c),
                c if (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
                c if (c as u32) < 0x10000 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push_str(&format!("\\U{:08x}", c as u32)),
            }
        }
        out.push(quote);
        out
    }

    fn is_printable(c: char) -> bool {
        if c == ' ' {
            return true;
        }
        if c.is_control() || c.is_whitespace() {
            return false;
        }
        !matches!(
            c,
            '\u{ad}'
                | '\u{600}'..='\u{605}'
                | '\u{61c}'
                | '\u{6dd}'
                | '\u{70f}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn rollout(text: &str) -> (tempfile::TempDir, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("rollout.jsonl");
            std::fs::write(&path, text).unwrap();
            (dir, path)
        }

        const ROLLOUT: &str = concat!(
            r#"{"type":"session_meta","timestamp":"t","payload":{"id":"r1","cwd":"/w"}}"#,
            "\n\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"  <environment_context>cwd</environment_context>"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"system","content":[]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hi "},{"type":"input_image","url":"x"},{"type":"input_text","text":"there"}]}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"<environment_context>"}]}}"#,
            "\n",
        );

        #[test]
        fn session_meta_is_the_first_record_payload() {
            let (_dir, path) = rollout(ROLLOUT);
            assert_eq!(
                read_session_meta(&path).unwrap(),
                serde_json::json!({"id": "r1", "cwd": "/w"})
            );
        }

        #[test]
        fn session_meta_errors_match_the_reference_text() {
            let (_dir, path) = rollout("{\"type\":\"x\",\"payload\":{}}\n");
            assert_eq!(
                read_session_meta(&path).unwrap_err().to_string(),
                format!(
                    "{}: first record is 'x', expected 'session_meta'",
                    path.display()
                )
            );
            let (_dir, path) = rollout("\n  \n");
            assert_eq!(
                read_session_meta(&path).unwrap_err().to_string(),
                format!("{}: empty rollout — no session_meta record", path.display())
            );
        }

        #[test]
        fn messages_keep_order_flag_environment_context_and_skip_non_turns() {
            let (_dir, path) = rollout(ROLLOUT);
            let messages = iter_messages(&path).unwrap();
            let summary: Vec<_> = messages
                .iter()
                .map(|m| (m.role.as_str(), m.text.as_str(), m.is_environment_context))
                .collect();
            assert_eq!(
                summary,
                [
                    (
                        "user",
                        "  <environment_context>cwd</environment_context>",
                        true
                    ),
                    ("user", "hi there", false),
                    ("assistant", "<environment_context>", false),
                ]
            );
        }

        #[test]
        fn a_message_without_role_is_malformed() {
            let (_dir, path) = rollout("{\"payload\":{\"type\":\"message\"}}\n");
            assert!(matches!(
                iter_messages(&path),
                Err(RolloutError::Malformed { key: "role", .. })
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tx_core::Effort;

    fn write(path: &Path, data: &str) -> PathBuf {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, data).unwrap();
        path.to_owned()
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn locator(base: &Path) -> Locator {
        Locator {
            claude_home: base.join("claude"),
            codex_home: base.join("codex"),
        }
    }

    #[test]
    fn claude_locate_fast_path_then_projects_glob() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let loc = locator(&base);
        let work = base.join("w");
        std::fs::create_dir(&work).unwrap();
        let work = work.to_str().unwrap();
        assert_eq!(loc.locate_transcript("claude", "c1", Some(work)), None);

        let put = |name: &str| {
            write(
                &loc.claude_home.join("projects").join(name).join("c1.jsonl"),
                "{}\n",
            )
        };
        let other = put("-zz");
        assert_eq!(
            loc.locate_transcript("claude", "c1", Some("/gone")),
            Some(other.clone())
        );
        assert_eq!(loc.locate_transcript("claude", "c1", None), Some(other));
        let first = put("-aa");
        assert_eq!(
            loc.locate_transcript("claude", "c1", Some("/gone")),
            Some(first)
        );
        let fast = put(&munge(work));
        assert_eq!(
            loc.locate_transcript("claude", "c1", Some(work)),
            Some(fast.clone())
        );
        // A dangling link misses the fast path but is still a glob hit; the cwd munge is
        // preferred over the first sorted match.
        std::fs::remove_file(&fast).unwrap();
        std::os::unix::fs::symlink(base.join("missing"), &fast).unwrap();
        assert_eq!(
            loc.locate_transcript("claude", "c1", Some(work)),
            Some(fast)
        );
        assert_eq!(loc.locate_transcript("other", "c1", Some(work)), None);
    }

    /// History's `glob_fallback_prefers_cwd_munge_then_sorted_first`.
    #[test]
    fn claude_glob_fallback_prefers_cwd_munge_then_sorted_first() {
        let dir = tempfile::tempdir().unwrap();
        let loc = locator(&std::fs::canonicalize(dir.path()).unwrap());
        let projects = loc.claude_home.join("projects");
        write(&projects.join("-a/c1.jsonl"), "A");
        write(&projects.join("-b/c1.jsonl"), "B");
        let resolve = |cwd| loc.locate_transcript("claude", "c1", cwd);
        assert_eq!(resolve(Some("/b")), Some(projects.join("-b/c1.jsonl")));
        assert_eq!(resolve(Some("/zzz")), Some(projects.join("-a/c1.jsonl")));
        assert_eq!(resolve(Some("")), Some(projects.join("-a/c1.jsonl")));
        assert_eq!(resolve(None), Some(projects.join("-a/c1.jsonl")));
    }

    #[test]
    fn codex_locate_never_globs_claude_projects() {
        let dir = tempfile::tempdir().unwrap();
        let loc = locator(dir.path());
        write(&loc.claude_home.join("projects/-w/r1.jsonl"), "stray");
        assert_eq!(loc.locate_transcript("codex", "r1", Some("/w")), None);
        assert_eq!(loc.locate_transcript("codex", "r1", None), None);
    }

    #[test]
    fn codex_rollout_newest_match() {
        let dir = tempfile::tempdir().unwrap();
        let loc = locator(dir.path());
        let put = |date: &str, name: &str| {
            write(
                &loc.codex_home.join("sessions").join(date).join(name),
                "{}\n",
            )
        };
        put("2026/09/16", "rollout-2026-09-16T10-00-00-r1x.jsonl");
        put("2026/09/16", "rollout-r1.jsonl");
        assert_eq!(loc.locate_transcript("codex", "r1", Some("/w")), None);
        let early = put("2026/09/16", "rollout-2026-09-16T10-00-00-r1.jsonl");
        assert_eq!(
            loc.locate_transcript("codex", "r1", Some("/any")),
            Some(early)
        );
        let later = put("2026/09/17", "rollout-2026-09-17T10-00-00-r1.jsonl");
        assert_eq!(loc.locate_transcript("codex", "r1", None), Some(later));
        assert!(bundle_sidecars("codex", Path::new("/s/rollout-x-r1.jsonl"), "r1").is_empty());
    }

    #[test]
    fn claude_sidecar_is_the_chat_dir_beside_the_resolved_transcript() {
        assert_eq!(
            bundle_sidecars("claude", Path::new("/p/-x/c1.jsonl"), "c1"),
            [PathBuf::from("/p/-x/c1")]
        );
    }

    #[test]
    fn prepare_chat_for_cwd_copies_transcript_and_merges_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let loc = locator(&base);
        let (src, dst) = (base.join("src"), base.join("dst"));
        std::fs::create_dir(&src).unwrap();
        let path = |cwd: &Path| loc.claude_transcript("c1", cwd.to_str().unwrap());

        prepare_chat_for_cwd(&loc, "claude", "c1", &src, &dst).unwrap();
        assert!(!path(&dst).exists(), "no source transcript, nothing copied");

        write(&path(&src), "{\"l\":1}\n");
        prepare_chat_for_cwd(&loc, "claude", "c1", &src, &src).unwrap();
        prepare_chat_for_cwd(&loc, "codex", "c1", &src, &dst).unwrap();
        assert!(!path(&dst).exists());
        prepare_chat_for_cwd(&loc, "claude", "c1", &src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(path(&dst)).unwrap(), "{\"l\":1}\n");

        write(
            &path(&src).with_extension("").join("tool-results/a.txt"),
            "A",
        );
        write(
            &path(&dst).with_extension("").join("tool-results/z.txt"),
            "Z",
        );
        prepare_chat_for_cwd(&loc, "claude", "c1", &src, &dst).unwrap();
        let mut names: Vec<_> =
            std::fs::read_dir(path(&dst).with_extension("").join("tool-results"))
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
        names.sort();
        assert_eq!(names, ["a.txt", "z.txt"]);
    }

    #[test]
    fn prepare_chat_for_cwd_never_overwrites_a_newer_destination() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let loc = locator(&base);
        let (src, dst) = (base.join("src"), base.join("dst"));
        let path = |cwd: &Path| loc.claude_transcript("c1", cwd.to_str().unwrap());
        write(&path(&src), "old\n");
        write(
            &path(&src).with_extension("").join("tool-results/a.txt"),
            "old A",
        );
        write(
            &path(&src).with_extension("").join("tool-results/b.txt"),
            "B",
        );
        write(&path(&dst), "old\nnewer live turn\n");
        write(
            &path(&dst).with_extension("").join("tool-results/a.txt"),
            "new A",
        );

        prepare_chat_for_cwd(&loc, "claude", "c1", &src, &dst).unwrap();
        assert_eq!(
            std::fs::read_to_string(path(&dst)).unwrap(),
            "old\nnewer live turn\n"
        );
        let results = path(&dst).with_extension("").join("tool-results");
        assert_eq!(
            std::fs::read_to_string(results.join("a.txt")).unwrap(),
            "new A"
        );
        assert_eq!(std::fs::read_to_string(results.join("b.txt")).unwrap(), "B");
    }

    #[test]
    fn fork_args_claude() {
        let mut args = argv(&["--resume", "X", "--model", "m", "--effort", "high"]);
        fork_args("claude", &mut args);
        assert_eq!(
            args,
            argv(&[
                "--resume",
                "X",
                "--fork-session",
                "--model",
                "m",
                "--effort",
                "high"
            ])
        );
        let mut no_resume = argv(&["--model", "m"]);
        fork_args("claude", &mut no_resume);
        assert_eq!(no_resume, argv(&["--model", "m"]));
    }

    #[test]
    fn fork_args_codex() {
        let mut args = argv(&["resume", "X", "-m", "gpt"]);
        fork_args("codex", &mut args);
        assert_eq!(args, argv(&["fork", "X", "-m", "gpt"]));
        let mut fresh = argv(&["-m", "gpt"]);
        fork_args("codex", &mut fresh);
        assert_eq!(fresh, argv(&["-m", "gpt"]));
    }

    #[test]
    fn adapters_are_the_engine_crates() {
        assert_eq!(adapter("claude").unwrap().id(), "claude");
        assert_eq!(adapter("codex").unwrap().id(), "codex");
        assert!(adapter("zzz").is_none());
    }

    #[test]
    fn agent_plan_resumes_through_the_adapter_then_forks() {
        let dir = tempfile::tempdir().unwrap();
        let core = TxCore::open(tx_core::CoreConfig::new(tx_core::Directories::under(
            dir.path(),
        )))
        .unwrap();
        let launch = AgentLaunch {
            engine: "claude".into(),
            profile: vec![],
            model: Some("opus".into()),
            effort: Some(Effort::Low),
            rest: serde_json::Map::new(),
        };
        let plan = agent_plan(
            &core,
            &launch,
            dir.path(),
            &dir.path().join("p.md"),
            Some("X"),
            Some("go"),
        )
        .unwrap();
        assert_eq!(plan.program, "claude");
        assert_eq!(plan.args[..4], argv(&["--resume", "X", "--model", "opus"]));
        assert_eq!(plan.args.last().map(String::as_str), Some("go"));
        let mut args = plan.args;
        fork_args("claude", &mut args);
        assert_eq!(args[..3], argv(&["--resume", "X", "--fork-session"]));
        let unknown = AgentLaunch {
            engine: "zzz".into(),
            ..launch
        };
        let error = agent_plan(
            &core,
            &unknown,
            dir.path(),
            &dir.path().join("p.md"),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(error, ChatError::EngineUnknown(_)));
    }
}
