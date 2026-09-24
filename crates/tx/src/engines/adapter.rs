//! The engine adapter surface (engine_adapter.py): one [`EngineAdapter`] per coding-agent CLI; the
//! rest of the core stays engine-blind.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session::State;
use crate::shlex::ShlexError;
use crate::skills::SkillError;

use super::codex_rollout::RolloutError;

/// The launch environment of a session (`Session::spawn_env`), in record order.
pub type LaunchEnv = Vec<(String, String)>;

/// `env.get(key)` on a launch environment.
pub fn env_get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// `env[key] = value`: replaces in place, else appends (dict insertion order).
pub fn env_set(env: &mut LaunchEnv, key: &str, value: String) {
    match env.iter_mut().find(|(name, _)| name == key) {
        Some((_, existing)) => *existing = value,
        None => env.push((key.to_owned(), value)),
    }
}

/// A reasoning-effort tier (`EFFORT_LEVELS`), `1..=5`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effort {
    Low = 1,
    Medium = 2,
    High = 3,
    XHigh = 4,
    Max = 5,
}

/// `DEFAULT_EFFORT`.
pub const DEFAULT_EFFORT: Effort = Effort::High;

impl Effort {
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    pub fn from_level(level: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|effort| effort.level() == level)
    }

    pub fn level(self) -> u8 {
        self as u8
    }

    /// The `EFFORT_LEVELS` name.
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }
}

/// Where an engine's turn-done → WAITING transition comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StateSource {
    HookEvents,
    StatusPoll,
}

impl StateSource {
    pub fn as_str(self) -> &'static str {
        match self {
            StateSource::HookEvents => "hook_events",
            StateSource::StatusPoll => "status_poll",
        }
    }
}

/// An engine adapter could not honor a request. The CLI maps it to a `tx <verb>: <msg>` line +
/// exit 1 (the Python's `EngineError`), and the variants below cover what the reference let
/// escape as a traceback (Q9/Q26).
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// `EngineError(msg)` raised by an adapter (antigravity's effort / fork-surgery refusals).
    #[error("{0}")]
    Refused(String),
    #[error(transparent)]
    Skill(#[from] SkillError),
    #[error(transparent)]
    Rollout(#[from] RolloutError),
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
    #[error("cannot parse command {command:?}: {source}")]
    Shlex {
        command: String,
        #[source]
        source: ShlexError,
    },
}

impl EngineError {
    pub(crate) fn io(path: &Path) -> impl FnOnce(io::Error) -> EngineError + '_ {
        move |source| EngineError::Io {
            path: path.to_owned(),
            source,
        }
    }
}

/// `shlex.split(command)` with the error carrying the command.
pub(crate) fn split_command(command: &str) -> Result<Vec<String>, EngineError> {
    crate::shlex::shlex_split(command).map_err(|source| EngineError::Shlex {
        command: command.to_owned(),
        source,
    })
}

/// The chat identity a hook payload carries — captured, never minted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedChat {
    pub session_id: String,
    pub transcript_path: String,
}

/// The keyword arguments of `build_launch_command` (all default off / unset).
#[derive(Clone, Copy, Debug, Default)]
pub struct LaunchOptions<'a> {
    pub model: Option<&'a str>,
    pub effort: Option<Effort>,
    pub initial_prompt: Option<&'a str>,
    pub read_only: bool,
    /// Grants the engine's browser tooling; an engine without one ignores it.
    pub browser: bool,
    /// Injected additively (never replacing the base prompt) so chat ops inherit it.
    pub role_priming: Option<&'a str>,
}

/// The adapter surface for one coding-agent CLI. Argv builders return the argv (binary first);
/// callers `shlex_join` it. Everything env-derived (engine homes, `$PATH`, `$TX_IDE_HOME`, the
/// `tx` executable) is bound into the adapter at construction, never read here.
pub trait EngineAdapter {
    // ----- identity -----

    fn binary(&self) -> &str;

    /// Whether `command` invokes this engine's binary (first-token basename).
    fn matches_binary(&self, command: &str) -> bool {
        command
            .split_whitespace()
            .next()
            .is_some_and(|first| basename(first) == self.binary())
    }

    /// `(session_id, transcript_path)` off a hook payload; `None` when the payload lacks them
    /// (the reference's swallowed `KeyError` — the next capture event retries).
    fn capture_session_id(&self, hook_payload: &Value) -> Option<CapturedChat>;

    // ----- launch / ops -----

    /// Argv for a fresh session. `env` is the session's launch environment, mutable so an engine
    /// without a persona flag can add a pointer to it (antigravity's rules file).
    fn build_launch_command(
        &self,
        options: &LaunchOptions<'_>,
        env: &mut LaunchEnv,
    ) -> Result<Vec<String>, EngineError>;

    /// Argv to resume `chat_id` in place, carrying the persona of `source_cmd` when given.
    fn resume_command(
        &self,
        chat_id: &str,
        source_cmd: Option<&str>,
        read_only: bool,
    ) -> Result<Vec<String>, EngineError>;

    /// Argv to fork a chat onto its full history, carrying the source persona. May do I/O
    /// (antigravity mints the fork on disk).
    fn fork_command(
        &self,
        source_cmd: &str,
        chat_id: &str,
        read_only: bool,
    ) -> Result<Vec<String>, EngineError>;

    /// Argv for a fresh session carrying the source persona + `seed` as its initial prompt.
    fn seed_command(
        &self,
        source_cmd: &str,
        seed: &str,
        read_only: bool,
    ) -> Result<Vec<String>, EngineError>;

    /// Argv for the fixed distiller pass (no source persona).
    fn distiller_command(&self, seed: &str) -> Vec<String>;

    /// Make an existing chat discoverable when a continuation moves to another cwd.
    fn prepare_chat_for_cwd(
        &self,
        chat_id: &str,
        source_cwd: &str,
        target_cwd: &str,
    ) -> Result<(), EngineError>;

    /// Bind an engine command to its FINAL working directory (skill grant, per-worktree files,
    /// workspace argv) and return the bound command. Idempotent.
    fn prepare_workspace(
        &self,
        command: &str,
        cwd: &str,
        env: &[(String, String)],
    ) -> Result<String, EngineError>;

    /// Whether `command` carries this engine's inner read-only controls.
    fn is_read_only_command(&self, command: &str) -> bool;

    // ----- transcript -----

    /// Where a chat's transcript lives. May not exist — existence is the caller's check.
    fn resolve_transcript(&self, chat_id: &str, cwd: &str) -> PathBuf;

    /// The per-engine moved-cwd fallback when the fast path misses. Default: none (Q7 FIX — the
    /// reference globbed Claude's projects root for every engine).
    fn relocate_transcript(&self, _chat_id: &str, _cwd_hint: Option<&str>) -> Option<PathBuf> {
        None
    }

    /// history.py's `resolve_transcript`: the fast path from a non-empty `cwd_hint` when it
    /// exists, else [`Self::relocate_transcript`].
    fn locate_transcript(&self, chat_id: &str, cwd_hint: Option<&str>) -> Option<PathBuf> {
        if let Some(cwd) = cwd_hint.filter(|cwd| !cwd.is_empty()) {
            let fast = self.resolve_transcript(chat_id, cwd);
            if fast.exists() {
                return Some(fast);
            }
        }
        self.relocate_transcript(chat_id, cwd_hint)
    }

    /// Each turn as an engine-neutral message object.
    fn iter_messages(&self, transcript: &Path) -> Result<Vec<Value>, EngineError>;

    /// Pure (no I/O): sidecar dirs to copy into the history bundle beside the transcript.
    fn bundle_sidecars(&self, src_transcript: &Path, chat_id: &str) -> Vec<PathBuf>;

    // ----- hooks / state -----

    /// Hook-event-name → State table.
    fn event_to_state(&self) -> &'static [(&'static str, State)];

    fn state_for_event(&self, event: &str) -> Option<State> {
        self.event_to_state()
            .iter()
            .find(|(name, _)| *name == event)
            .map(|(_, state)| *state)
    }

    fn state_source(&self) -> StateSource;
}

/// `os.path.basename`.
pub(crate) fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Shell-control tokens: once shlex surfaces one, the rest of a compound command is shell
/// wrapping, not engine argv.
const SHELL_CONTROL_TOKENS: [&str; 12] = [
    ";", "&", "&&", "||", "|", "|&", "&>", "&>>", "(", ")", "{", "}",
];

/// An exact control token, or a redirection (starts with `<`/`>`).
pub(crate) fn is_shell_control(token: &str) -> bool {
    SHELL_CONTROL_TOKENS.contains(&token) || token.starts_with(['<', '>'])
}

/// Whether `tokens[index + 1]` exists and would be consumed as a flag value.
pub(crate) fn takes_next_token(tokens: &[String], index: usize) -> bool {
    tokens
        .get(index + 1)
        .is_some_and(|next| !next.starts_with('-') && !is_shell_control(next))
}

/// `(binary, inherited-flags)` of a source command after an identity strip.
pub(crate) type Stripped = (String, Vec<String>);

/// Hook payload lookup: both keys must be strings.
pub(crate) fn capture_keys(payload: &Value, id_key: &str, path_key: &str) -> Option<CapturedChat> {
    Some(CapturedChat {
        session_id: payload.get(id_key)?.as_str()?.to_owned(),
        transcript_path: payload.get(path_key)?.as_str()?.to_owned(),
    })
}

/// Python's `os.path.realpath` (non-strict): symlinks resolved along the existing prefix, the
/// missing tail kept, `.`/`..` normalised. Relative paths are taken against the process cwd.
pub fn realpath(path: &Path) -> PathBuf {
    const MAX_LINKS: usize = 40;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut pending: Vec<std::ffi::OsString> = components_of(&absolute);
    pending.reverse();
    let mut resolved = PathBuf::from("/");
    let mut links = 0;
    while let Some(name) = pending.pop() {
        if name == "." || name.is_empty() {
            continue;
        }
        if name == ".." {
            resolved.pop();
            continue;
        }
        let candidate = resolved.join(&name);
        let is_link = std::fs::symlink_metadata(&candidate).is_ok_and(|m| m.is_symlink());
        if !is_link || links >= MAX_LINKS {
            resolved = candidate;
            continue;
        }
        let Ok(target) = std::fs::read_link(&candidate) else {
            resolved = candidate;
            continue;
        };
        links += 1;
        if target.is_absolute() {
            resolved = PathBuf::from("/");
        }
        let mut parts = components_of(&target);
        parts.reverse();
        pending.extend(parts);
    }
    resolved
}

fn components_of(path: &Path) -> Vec<std::ffi::OsString> {
    use std::path::Component;
    path.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_owned()),
            Component::ParentDir => Some("..".into()),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect()
}

/// `shutil.copy2`: content + permission bits + access/modification times.
pub(crate) fn copy2(source: &Path, target: &Path) -> Result<(), EngineError> {
    std::fs::copy(source, target).map_err(EngineError::io(target))?;
    copy_times(source, target)
}

fn copy_times(source: &Path, target: &Path) -> Result<(), EngineError> {
    let metadata = std::fs::metadata(source).map_err(EngineError::io(source))?;
    let times = std::fs::FileTimes::new()
        .set_accessed(metadata.accessed().map_err(EngineError::io(source))?)
        .set_modified(metadata.modified().map_err(EngineError::io(source))?);
    let file = std::fs::File::options()
        .write(!metadata.is_dir())
        .read(metadata.is_dir())
        .open(target)
        .map_err(EngineError::io(target))?;
    file.set_times(times).map_err(EngineError::io(target))
}

/// `shutil.copytree(source, target, dirs_exist_ok=True)` (symlinks followed, copy2 per file).
pub(crate) fn copytree(source: &Path, target: &Path) -> Result<(), EngineError> {
    std::fs::create_dir_all(target).map_err(EngineError::io(target))?;
    let entries = std::fs::read_dir(source).map_err(EngineError::io(source))?;
    for entry in entries {
        let entry = entry.map_err(EngineError::io(source))?;
        let from = entry.path();
        let to = target.join(entry.file_name());
        if from.is_dir() {
            copytree(&from, &to)?;
        } else {
            copy2(&from, &to)?;
        }
    }
    let permissions = std::fs::metadata(source)
        .map_err(EngineError::io(source))?
        .permissions();
    std::fs::set_permissions(target, permissions).map_err(EngineError::io(target))?;
    copy_times(source, target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effort_levels_match_the_reference_table() {
        let names: Vec<_> = (0..=6)
            .map(|level| Effort::from_level(level).map(Effort::as_str))
            .collect();
        assert_eq!(
            names,
            [
                None,
                Some("low"),
                Some("medium"),
                Some("high"),
                Some("xhigh"),
                Some("max"),
                None
            ]
        );
        assert_eq!(DEFAULT_EFFORT.as_str(), "high");
        assert_eq!(StateSource::HookEvents.as_str(), "hook_events");
        assert_eq!(StateSource::StatusPoll.as_str(), "status_poll");
    }

    #[test]
    fn env_set_replaces_in_place_or_appends() {
        let mut env: LaunchEnv = vec![("A".into(), "1".into()), ("B".into(), "2".into())];
        env_set(&mut env, "A", "3".into());
        env_set(&mut env, "C", "4".into());
        assert_eq!(env_get(&env, "A"), Some("3"));
        assert_eq!(
            env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
    }

    #[test]
    fn shell_control_and_value_lookahead() {
        for token in [";", "&&", "|", "(", "}", ">x", "<in", "2>"] {
            assert_eq!(is_shell_control(token), token != "2>", "{token}");
        }
        let tokens: Vec<String> = ["--a", "v", "--b", "--c", "|"].map(String::from).to_vec();
        assert!(takes_next_token(&tokens, 0));
        assert!(!takes_next_token(&tokens, 2));
        assert!(!takes_next_token(&tokens, 3));
        assert!(!takes_next_token(&tokens, 4));
    }

    #[test]
    fn realpath_resolves_links_and_keeps_a_missing_tail() {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(base.join("real")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        std::os::unix::fs::symlink("real", base.join("rel")).unwrap();
        assert_eq!(realpath(&base.join("link")), base.join("real"));
        assert_eq!(realpath(&base.join("rel/x/../y")), base.join("real/y"));
        assert_eq!(
            realpath(Path::new("/nope/gone/./a")),
            Path::new("/nope/gone/a")
        );
        assert_eq!(realpath(Path::new("/a/../..")), Path::new("/"));
    }
}
