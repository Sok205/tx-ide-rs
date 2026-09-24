//! The Google Antigravity CLI (`agy`) adapter (engines/antigravity.py): launch/op argv in Go
//! `flag` grammar, the per-worktree `.agents/` files + settings trust, the sqlite conversation-db
//! fork surgery, and brain-transcript reading.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use rusqlite::types::{Value as SqlValue, ValueRef};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::pyjson::dumps_pretty;
use crate::session::State;
use crate::shlex::shlex_join;
use crate::skills::{SKILLS_ENV, exclude_from_git, link_skills};
use crate::storage::{Home, expand_user};

use super::adapter::{
    CapturedChat, DEFAULT_EFFORT, Effort, EngineAdapter, EngineError, LaunchEnv, LaunchOptions,
    StateSource, Stripped, capture_keys, copy2, env_get, env_set, is_shell_control, realpath,
    split_command, takes_next_token,
};

pub const AGY_BIN: &str = "agy";
/// The agy version every recipe here (flag grammar, hooks.json, fork surgery, brain layout) was
/// verified against.
pub const AGY_VERIFIED_VERSION: &str = "1.1.13";
/// Antigravity's app-data dir: no env override exists, so the path is fixed.
pub const AGY_HOME: &str = "~/.gemini/antigravity-cli";

/// Effort is rendered into the model slug; `--effort` is never emitted.
pub const AGY_MODEL_FAMILY: &str = "gemini-3.7-flash";
pub const DISTILLER_MODEL: &str = "gemini-3.7-flash-high";

pub const SKIP_PERMISSIONS_FLAG: &str = "--dangerously-skip-permissions";
pub const ADD_DIR_FLAG: &str = "--add-dir";
pub const LOG_FILE_FLAG: &str = "--log-file";
pub const MODE_FLAG: &str = "--mode";
pub const PLAN_MODE: &str = "plan";
/// A bare positional prompt is silently dropped by agy: every seed rides `-i`.
pub const PROMPT_FLAG: &str = "-i";

/// Launch-env key pointing at the content-addressed role-priming rules file.
pub const RULES_FILE_ENV: &str = "TX_AGY_RULES_FILE";
pub const RULES_FILE_NAME: &str = "tx-role.md";
pub const HOOKS_FILE_NAME: &str = "hooks.json";
pub const HOOKS_TEMPLATE_NAME: &str = "hooks.json.template";
pub const AGENTS_DIR_NAME: &str = ".agents";
pub const TRANSCRIPT_SUFFIX: &str = ".jsonl";

// ----- chat-op command derivation (Go `flag` grammar) -----------------------------------------

/// Booleans: never consume a following token.
const BARE_NAMES: [&str; 9] = [
    "dangerously-skip-permissions",
    "disable-slash-commands",
    "new-project",
    "sandbox",
    "continue",
    "c",
    "help",
    "h",
    "version",
];
const IDENTITY_VALUE_NAMES: [&str; 1] = ["conversation"];
const IDENTITY_BARE_NAMES: [&str; 2] = ["continue", "c"];
/// The prompt riders; their value is always kept atomically.
const PROMPT_VALUE_NAMES: [&str; 5] = ["i", "prompt-interactive", "p", "print", "prompt"];
/// Workspace-bound, re-bound by `prepare_workspace`.
const CWD_BOUND_VALUE_NAMES: [&str; 2] = ["add-dir", "log-file"];
const ACCESS_BARE_NAMES: [&str; 2] = ["dangerously-skip-permissions", "sandbox"];
const ACCESS_VALUE_NAMES: [&str; 1] = ["mode"];

/// `(name, inline_value)` for a flag token (dashes normalised away, `--flag=value` split), or
/// `None` for a positional (including an all-dash token).
fn flag_parts(token: &str) -> Option<(&str, Option<&str>)> {
    if !token.starts_with('-') || token.trim_matches('-').is_empty() {
        return None;
    }
    let body = token.trim_start_matches('-');
    Some(match body.split_once('=') {
        Some((name, inline_value)) => (name, Some(inline_value)),
        None => (body, None),
    })
}

/// Split a source agy `cmd` into (binary, inherited-flags): identity, prompt rider, workspace
/// binding and positionals dropped; the persona (and any unknown flag WITH its value) kept.
fn strip_identity(source_cmd: &str) -> Result<Stripped, EngineError> {
    let tokens = split_command(source_cmd)?;
    let binary = tokens.first().map_or(AGY_BIN, String::as_str).to_owned();
    let mut inherited = Vec::new();
    let mut index = 1;
    while index < tokens.len() {
        let token = tokens[index].as_str();
        if is_shell_control(token) {
            break;
        }
        let Some((name, inline_value)) = flag_parts(token) else {
            index += 1;
            continue;
        };
        if IDENTITY_BARE_NAMES.contains(&name) {
            index += 1;
        } else if IDENTITY_VALUE_NAMES.contains(&name)
            || PROMPT_VALUE_NAMES.contains(&name)
            || CWD_BOUND_VALUE_NAMES.contains(&name)
        {
            index += if inline_value.is_some() { 1 } else { 2 };
        } else if BARE_NAMES.contains(&name) || inline_value.is_some() {
            inherited.push(token.to_owned());
            index += 1;
        } else if takes_next_token(&tokens, index) {
            inherited.extend_from_slice(&tokens[index..index + 2]);
            index += 2;
        } else {
            inherited.push(token.to_owned());
            index += 1;
        }
    }
    Ok((binary, inherited))
}

/// Remove the named flags (with their values), keeping a prompt flag's value atomically.
fn drop_flags(tokens: &[String], bare_names: &[&str], value_names: &[&str]) -> Vec<String> {
    let mut kept = Vec::with_capacity(tokens.len());
    let mut index = 0;
    while index < tokens.len() {
        match flag_parts(&tokens[index]) {
            Some((name, None)) if PROMPT_VALUE_NAMES.contains(&name) => {
                let end = (index + 2).min(tokens.len());
                kept.extend_from_slice(&tokens[index..end]);
                index += 2;
            }
            Some((name, _)) if bare_names.contains(&name) => index += 1,
            Some((name, inline_value)) if value_names.contains(&name) => {
                index += if inline_value.is_some() { 1 } else { 2 };
            }
            _ => {
                kept.push(tokens[index].clone());
                index += 1;
            }
        }
    }
    kept
}

/// Strip tx's access controls, then apply the destination mode: writable = skip-permissions,
/// read-only = `--mode plan` (never agy's own `--sandbox`).
fn apply_access(command: &[String], read_only: bool) -> Vec<String> {
    let mut command = drop_flags(command, &ACCESS_BARE_NAMES, &ACCESS_VALUE_NAMES);
    if read_only {
        command.extend([MODE_FLAG.to_owned(), PLAN_MODE.to_owned()]);
    } else {
        command.push(SKIP_PERMISSIONS_FLAG.to_owned());
    }
    command
}

/// pathlib's `PurePosixPath(path).name` (`.` components and trailing slashes ignored).
fn path_name(path: &str) -> &str {
    path.split('/')
        .rfind(|part| !part.is_empty() && *part != ".")
        .unwrap_or("")
}

/// `bytes.replace(old, new)`.
fn replace_bytes(haystack: &[u8], old: &[u8], new: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    if old.is_empty() {
        for byte in haystack {
            out.extend_from_slice(new);
            out.push(*byte);
        }
        out.extend_from_slice(new);
        return out;
    }
    let mut rest = haystack;
    while let Some(position) = rest.windows(old.len()).position(|window| window == old) {
        out.extend_from_slice(&rest[..position]);
        out.extend_from_slice(new);
        rest = &rest[position + old.len()..];
    }
    out.extend_from_slice(rest);
    out
}

// ----- fork surgery (version-fragile, no CLI fork verb) ---------------------------------------

/// The fork surgery failed: sqlite errors become the version-fragility refusal (and the partial
/// copy is removed); filesystem errors propagate as-is, like the reference.
enum SurgeryError {
    Sqlite(rusqlite::Error),
    Engine(EngineError),
}

impl From<rusqlite::Error> for SurgeryError {
    fn from(error: rusqlite::Error) -> Self {
        SurgeryError::Sqlite(error)
    }
}

/// A column value as the reference's `sqlite3` sees it: TEXT that is not UTF-8 is an error.
fn owned_value(value: ValueRef<'_>) -> rusqlite::Result<SqlValue> {
    Ok(match value {
        ValueRef::Null => SqlValue::Null,
        ValueRef::Integer(number) => SqlValue::Integer(number),
        ValueRef::Real(number) => SqlValue::Real(number),
        ValueRef::Text(text) => SqlValue::Text(
            std::str::from_utf8(text)
                .map_err(rusqlite::Error::from)?
                .to_owned(),
        ),
        ValueRef::Blob(blob) => SqlValue::Blob(blob.to_vec()),
    })
}

/// Replace every occurrence of `old_id` with `new_id` across all TEXT and BLOB values of every
/// table via row-level UPDATEs (indexes stay consistent), in one transaction.
fn rewrite_embedded_id(db_path: &Path, old_id: &str, new_id: &str) -> rusqlite::Result<()> {
    let mut connection = Connection::open(db_path)?;
    let transaction = connection.transaction()?;
    let tables: Vec<String> = transaction
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for table in &tables {
        let columns: Vec<String> = transaction
            .prepare(&format!("PRAGMA table_info(\"{table}\")"))?
            .query_map([], |row| row.get(1))?
            .collect::<rusqlite::Result<_>>()?;
        let rows: Vec<Vec<SqlValue>> = {
            let mut statement =
                transaction.prepare(&format!("SELECT rowid, * FROM \"{table}\""))?;
            let width = statement.column_count();
            statement
                .query_map([], |row| {
                    (0..width)
                        .map(|index| owned_value(row.get_ref(index)?))
                        .collect()
                })?
                .collect::<rusqlite::Result<_>>()?
        };
        for row in rows {
            let Some((rowid, values)) = row.split_first() else {
                continue;
            };
            let mut set_columns = Vec::new();
            let mut parameters = Vec::new();
            for (column, value) in columns.iter().zip(values) {
                let replaced = match value {
                    SqlValue::Text(text) if text.contains(old_id) => {
                        SqlValue::Text(text.replace(old_id, new_id))
                    }
                    SqlValue::Blob(blob) if contains_bytes(blob, old_id.as_bytes()) => {
                        SqlValue::Blob(replace_bytes(blob, old_id.as_bytes(), new_id.as_bytes()))
                    }
                    _ => continue,
                };
                set_columns.push(format!("\"{column}\" = ?"));
                parameters.push(replaced);
            }
            if set_columns.is_empty() {
                continue;
            }
            parameters.push(rowid.clone());
            transaction.execute(
                &format!(
                    "UPDATE \"{table}\" SET {} WHERE rowid = ?",
                    set_columns.join(", ")
                ),
                rusqlite::params_from_iter(parameters),
            )?;
        }
    }
    transaction.commit()
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

// ----- the adapter --------------------------------------------------------------------------

/// `SessionStart` is capture-only; no permission or session-end event.
const EVENT_TO_STATE: &[(&str, State)] = &[
    ("PreInvocation", State::Working),
    ("PreToolUse", State::Working),
    ("PostToolUse", State::Working),
    ("PostInvocation", State::Working),
    ("Stop", State::Waiting),
];

/// The Google Antigravity CLI (`agy`) adapter — NOT gemini-cli.
#[derive(Clone, Debug)]
pub struct AntigravityEngine {
    home: Home,
    agy_home: PathBuf,
}

impl AntigravityEngine {
    /// `home_dir` is `$HOME`, against which the fixed agy app-data dir is expanded.
    pub fn new(home: Home, home_dir: Option<&Path>) -> Self {
        Self {
            home,
            agy_home: expand_user(AGY_HOME, home_dir),
        }
    }

    pub fn agy_home(&self) -> &Path {
        &self.agy_home
    }

    pub fn conversation_db(&self, chat_id: &str) -> PathBuf {
        self.agy_home
            .join("conversations")
            .join(format!("{chat_id}.db"))
    }

    /// The clean per-step JSONL agy writes per conversation — cwd-independent.
    pub fn brain_transcript(&self, chat_id: &str) -> PathBuf {
        self.agy_home
            .join("brain")
            .join(chat_id)
            .join(".system_generated/logs/transcript.jsonl")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.agy_home.join("settings.json")
    }

    /// The content-addressed priming file backing [`RULES_FILE_ENV`].
    pub fn priming_file(&self, role_priming: &str) -> PathBuf {
        let digest = Sha256::digest(role_priming.as_bytes());
        let hex: String = digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.home
            .root()
            .join("priming")
            .join(format!("agy-rules-{hex}.md"))
    }

    pub fn hooks_template_path(&self) -> PathBuf {
        self.home
            .hooks_dir()
            .join("antigravity")
            .join(HOOKS_TEMPLATE_NAME)
    }

    /// The per-worktree `--log-file` target, outside the repository.
    pub fn run_log_path(&self, cwd: &str) -> PathBuf {
        self.home
            .root()
            .join("agy-logs")
            .join(format!("{}.log", path_name(cwd)))
    }

    /// Copy the source conversation db under a fresh id and rewrite every embedded occurrence of
    /// the source uuid (same length, so protobuf length prefixes never shift). Returns the new id.
    fn fork_conversation_db(&self, source_id: &str) -> Result<String, EngineError> {
        let source_db = self.conversation_db(source_id);
        if !source_db.is_file() {
            return Err(EngineError::Refused(format!(
                "antigravity fork: conversation db not found: {}",
                source_db.display()
            )));
        }
        let new_id = uuid::Uuid::new_v4().to_string();
        let new_db = self.conversation_db(&new_id);
        let surgery = || -> Result<(), SurgeryError> {
            // Fold the WAL into the main db file so the copy sees every committed page.
            Connection::open(&source_db)?
                .query_row("PRAGMA wal_checkpoint(FULL)", [], |_| Ok(()))?;
            copy2(&source_db, &new_db).map_err(SurgeryError::Engine)?;
            rewrite_embedded_id(&new_db, source_id, &new_id)?;
            Ok(())
        };
        match surgery() {
            Ok(()) => Ok(new_id),
            Err(SurgeryError::Engine(error)) => Err(error),
            Err(SurgeryError::Sqlite(error)) => {
                // Best-effort cleanup of the partial copy (`unlink(missing_ok=True)`).
                let _ = std::fs::remove_file(&new_db);
                Err(EngineError::Refused(format!(
                    "antigravity fork surgery failed (version-fragile — verified against agy \
                     {AGY_VERIFIED_VERSION}): {error}"
                )))
            }
        }
    }

    /// The per-worktree `.agents/` files: the tx hooks.json and, when the session carries a
    /// priming pointer, the role rules file.
    fn write_workspace_customizations(
        &self,
        cwd: &str,
        env: &[(String, String)],
    ) -> Result<(), EngineError> {
        let agents_directory = Path::new(cwd).join(AGENTS_DIR_NAME);
        create_dir_exist_ok(&agents_directory)?;
        let template = self.hooks_template_path();
        if !template.is_file() {
            return Err(EngineError::Refused(format!(
                "antigravity hooks template missing at {} — run \
                 `setup/engines/antigravity.sh install` (tx state tracking needs per-worktree hooks)",
                template.display()
            )));
        }
        copy2(&template, &agents_directory.join(HOOKS_FILE_NAME))?;
        let Some(rules_source) = env_get(env, RULES_FILE_ENV).filter(|value| !value.is_empty())
        else {
            return Ok(());
        };
        if !Path::new(rules_source).is_file() {
            return Err(EngineError::Refused(format!(
                "antigravity role priming file missing at {rules_source} (recorded in \
                 ${RULES_FILE_ENV}) — it is content-addressed under $TX_IDE_HOME/priming/ and \
                 should never be removed"
            )));
        }
        let rules_directory = agents_directory.join("rules");
        create_dir_exist_ok(&rules_directory)?;
        copy2(
            Path::new(rules_source),
            &rules_directory.join(RULES_FILE_NAME),
        )
    }

    /// Pre-trust the worktree in agy's settings.json (`trustedWorkspaces`). An unparseable file
    /// (or one whose shape is not an object with a list) is left alone; the write is an atomic
    /// replace.
    fn seed_workspace_trust(&self, cwd: &str) -> Result<(), EngineError> {
        let settings = self.settings_path();
        let mut data = if settings.is_file() {
            let bytes = std::fs::read(&settings).map_err(EngineError::io(&settings))?;
            let Ok(text) = String::from_utf8(bytes) else {
                return Ok(());
            };
            match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(map)) => map,
                _ => return Ok(()),
            }
        } else {
            Map::new()
        };
        let trusted = data
            .entry("trustedWorkspaces")
            .or_insert_with(|| Value::Array(Vec::new()));
        let Value::Array(trusted) = trusted else {
            return Ok(());
        };
        let resolved = realpath(Path::new(cwd)).to_string_lossy().into_owned();
        let mut additions: Vec<String> = [cwd.to_owned(), resolved]
            .into_iter()
            .filter(|path| !trusted.iter().any(|entry| entry.as_str() == Some(path)))
            .collect();
        additions.sort();
        additions.dedup();
        if additions.is_empty() {
            return Ok(());
        }
        trusted.extend(additions.into_iter().map(Value::String));
        let parent = settings.parent().unwrap_or(Path::new("/"));
        std::fs::create_dir_all(parent).map_err(EngineError::io(parent))?;
        let temporary = parent.join(format!(".tx-trust-{}.tmp", std::process::id()));
        let text = dumps_pretty(&Value::Object(data)) + "\n";
        std::fs::write(&temporary, text).map_err(EngineError::io(&temporary))?;
        std::fs::rename(&temporary, &settings).map_err(EngineError::io(&settings))
    }
}

/// `Path.mkdir(exist_ok=True)`.
fn create_dir_exist_ok(path: &Path) -> Result<(), EngineError> {
    match std::fs::create_dir(path) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
            Err(EngineError::io(path)(error))
        }
        _ => Ok(()),
    }
}

/// Python's `str.strip()` whitespace set.
fn is_py_whitespace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

impl EngineAdapter for AntigravityEngine {
    fn binary(&self) -> &str {
        AGY_BIN
    }

    /// camelCase (protojson) keys; `transcriptPath` points at agy's `transcript_full.jsonl`.
    fn capture_session_id(&self, hook_payload: &Value) -> Option<CapturedChat> {
        capture_keys(hook_payload, "conversationId", "transcriptPath")
    }

    /// Effort → slug suffix on the pinned flash family (1-3 only); an explicit model wins
    /// verbatim. Role priming is written content-addressed and pointed at from `env`.
    fn build_launch_command(
        &self,
        options: &LaunchOptions<'_>,
        env: &mut LaunchEnv,
    ) -> Result<Vec<String>, EngineError> {
        let model = match options.model {
            Some(model) => model.to_owned(),
            None => {
                let suffix = match options.effort.unwrap_or(DEFAULT_EFFORT) {
                    Effort::Low => "low",
                    Effort::Medium => "medium",
                    Effort::High => "high",
                    other => {
                        return Err(EngineError::Refused(format!(
                            "antigravity supports efforts 1-3 (rendered into the \
                             {AGY_MODEL_FAMILY}-low/medium/high slugs); effort {} has no slug — \
                             pass --model for a pro/opus tier instead",
                            other.level()
                        )));
                    }
                };
                format!("{AGY_MODEL_FAMILY}-{suffix}")
            }
        };
        let command = [AGY_BIN.to_owned(), "--model".to_owned(), model];
        if let Some(priming) = options.role_priming.filter(|p| !p.is_empty()) {
            let rules_file = self.priming_file(priming);
            if !rules_file.is_file() {
                if let Some(parent) = rules_file.parent() {
                    std::fs::create_dir_all(parent).map_err(EngineError::io(parent))?;
                }
                std::fs::write(&rules_file, priming).map_err(EngineError::io(&rules_file))?;
            }
            env_set(
                env,
                RULES_FILE_ENV,
                rules_file.to_string_lossy().into_owned(),
            );
        }
        let mut command = apply_access(&command, options.read_only);
        if let Some(prompt) = options.initial_prompt.filter(|p| !p.is_empty()) {
            command.extend([PROMPT_FLAG.to_owned(), prompt.to_owned()]);
        }
        Ok(command)
    }

    /// `--conversation <id>`, bare (the resumed TUI waits for input).
    fn resume_command(
        &self,
        chat_id: &str,
        source_cmd: Option<&str>,
        read_only: bool,
    ) -> Result<Vec<String>, EngineError> {
        let (binary, inherited) = match source_cmd {
            Some(source_cmd) => strip_identity(source_cmd)?,
            None => (AGY_BIN.to_owned(), Vec::new()),
        };
        let mut command = vec![binary, "--conversation".to_owned(), chat_id.to_owned()];
        command.extend(inherited);
        Ok(apply_access(&command, read_only))
    }

    /// Mints the fork on disk (db surgery) and resumes it.
    fn fork_command(
        &self,
        source_cmd: &str,
        chat_id: &str,
        read_only: bool,
    ) -> Result<Vec<String>, EngineError> {
        let (binary, inherited) = strip_identity(source_cmd)?;
        let new_id = self.fork_conversation_db(chat_id)?;
        let mut command = vec![binary, "--conversation".to_owned(), new_id];
        command.extend(inherited);
        Ok(apply_access(&command, read_only))
    }

    fn seed_command(
        &self,
        source_cmd: &str,
        seed: &str,
        read_only: bool,
    ) -> Result<Vec<String>, EngineError> {
        let (binary, mut command) = strip_identity(source_cmd)?;
        command.insert(0, binary);
        let mut command = apply_access(&command, read_only);
        command.extend([PROMPT_FLAG.to_owned(), seed.to_owned()]);
        Ok(command)
    }

    fn distiller_command(&self, seed: &str) -> Vec<String> {
        [
            AGY_BIN,
            "--model",
            DISTILLER_MODEL,
            SKIP_PERMISSIONS_FLAG,
            PROMPT_FLAG,
            seed,
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// Conversations and brain transcripts are global by id: nothing to move.
    fn prepare_chat_for_cwd(&self, _: &str, _: &str, _: &str) -> Result<(), EngineError> {
        Ok(())
    }

    /// Re-point `--add-dir` + `--log-file` at `cwd`, write the `.agents/` files, grant skills,
    /// hide `.agents/` from git and pre-trust the directory. Idempotent.
    fn prepare_workspace(
        &self,
        command: &str,
        cwd: &str,
        env: &[(String, String)],
    ) -> Result<String, EngineError> {
        let tokens = split_command(command)?;
        if tokens.is_empty() {
            return Ok(command.to_owned());
        }
        let mut tokens = drop_flags(&tokens, &[], &CWD_BOUND_VALUE_NAMES);
        let log_file = self.run_log_path(cwd);
        if let Some(parent) = log_file.parent() {
            std::fs::create_dir_all(parent).map_err(EngineError::io(parent))?;
        }
        let binding = [
            ADD_DIR_FLAG.to_owned(),
            cwd.to_owned(),
            LOG_FILE_FLAG.to_owned(),
            log_file.to_string_lossy().into_owned(),
        ];
        let at = tokens.len().min(1);
        tokens.splice(at..at, binding);
        self.write_workspace_customizations(cwd, env)?;
        let workspace = Path::new(cwd);
        link_skills(
            &self.home,
            workspace,
            env_get(env, SKILLS_ENV),
            &format!("{AGENTS_DIR_NAME}/skills"),
        )?;
        exclude_from_git(&self.home, workspace, &format!("{AGENTS_DIR_NAME}/"))?;
        self.seed_workspace_trust(cwd)?;
        Ok(shlex_join(&tokens))
    }

    /// No skip-permissions, no `--sandbox`, and the last `--mode` is `plan`.
    fn is_read_only_command(&self, command: &str) -> bool {
        let Ok(tokens) = split_command(command) else {
            return false;
        };
        let mut mode = None;
        for (index, token) in tokens.iter().enumerate() {
            let Some((name, inline_value)) = flag_parts(token) else {
                continue;
            };
            if ACCESS_BARE_NAMES.contains(&name) {
                return false;
            }
            if name == "mode" {
                mode = inline_value.or_else(|| tokens.get(index + 1).map(String::as_str));
            }
        }
        mode == Some(PLAN_MODE)
    }

    /// The brain-dir JSONL; `cwd` is unused.
    fn resolve_transcript(&self, chat_id: &str, _cwd: &str) -> PathBuf {
        self.brain_transcript(chat_id)
    }

    /// USER_EXPLICIT → user, MODEL → assistant; SYSTEM bookkeeping skipped.
    fn iter_messages(&self, transcript: &Path) -> Result<Vec<Value>, EngineError> {
        let text = std::fs::read_to_string(transcript).map_err(EngineError::io(transcript))?;
        let mut messages = Vec::new();
        // Text-mode universal newlines: `\r`, `\n` and `\r\n` all end a line.
        for line in text.split(['\n', '\r']) {
            let line = line.trim_matches(is_py_whitespace);
            if line.is_empty() {
                continue;
            }
            let step: Value = serde_json::from_str(line).map_err(|source| EngineError::Json {
                path: transcript.to_owned(),
                source,
            })?;
            let role = match step.get("source").and_then(Value::as_str) {
                Some("USER_EXPLICIT") => "user",
                Some("MODEL") => "assistant",
                _ => continue,
            };
            let text = step
                .get("content")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new()));
            let mut message = Map::new();
            message.insert("role".to_owned(), Value::String(role.to_owned()));
            message.insert("text".to_owned(), text);
            messages.push(Value::Object(message));
        }
        Ok(messages)
    }

    /// The brain transcript inlines step content; the `.db` is internal state.
    fn bundle_sidecars(&self, _: &Path, _: &str) -> Vec<PathBuf> {
        Vec::new()
    }

    fn event_to_state(&self) -> &'static [(&'static str, State)] {
        EVENT_TO_STATE
    }

    /// agy fires `Stop` at turn end, so turn-done arrives as a hook event.
    fn state_source(&self) -> StateSource {
        StateSource::HookEvents
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::adapter::Effort;
    use serde_json::json;

    // Expected values below were produced by the Python reference (engines/antigravity.py).

    struct Fixture {
        _dir: tempfile::TempDir,
        base: PathBuf,
        engine: AntigravityEngine,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let engine = AntigravityEngine::new(Home::new(base.join("tx")), Some(&base.join("h")));
        Fixture {
            _dir: dir,
            base,
            engine,
        }
    }

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).to_owned()).collect()
    }

    fn env(pairs: &[(&str, &str)]) -> LaunchEnv {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    /// `kept` is the inherited argv after the access strip; the destination mode is appended.
    #[test]
    fn strip_identity_grammar_via_resume() {
        let e = fixture().engine;
        let cases: &[(&str, &[&str])] = &[
            (
                "agy --model m --dangerously-skip-permissions -i 'seed'",
                &["agy", "--model", "m"],
            ),
            (
                "agy --conversation abc --model=m -c --continue --add-dir /w --log-file=/l x positional --foo bar --sandbox --mode plan -p hello",
                &["agy", "--model=m", "--foo", "bar"],
            ),
            (
                "agy -model m --unknown --new-project",
                &["agy", "-model", "m", "--unknown", "--new-project"],
            ),
            ("agy --model m | tee x", &["agy", "--model", "m"]),
            ("", &["agy"]),
            (
                "/opt/agy --model m -- --x",
                &["/opt/agy", "--model", "m", "--x"],
            ),
            (
                "agy --foo=bar --dangling",
                &["agy", "--foo=bar", "--dangling"],
            ),
            ("agy -c=1 --conversation=z --prompt=q --i", &["agy"]),
        ];
        for (source, kept) in cases {
            let mut writable = vec![kept[0], "--conversation", "R"];
            writable.extend(&kept[1..]);
            let mut read_only = writable.clone();
            writable.push("--dangerously-skip-permissions");
            read_only.extend(["--mode", "plan"]);
            assert_eq!(
                e.resume_command("R", Some(source), false).unwrap(),
                argv(&writable),
                "{source}"
            );
            assert_eq!(
                e.resume_command("R", Some(source), true).unwrap(),
                argv(&read_only),
                "{source}"
            );
        }
        assert!(matches!(
            e.resume_command("R", Some("agy --model m 'x"), false),
            Err(EngineError::Shlex { .. })
        ));
        assert_eq!(
            e.resume_command("R", None, false).unwrap(),
            argv(&[
                "agy",
                "--conversation",
                "R",
                "--dangerously-skip-permissions"
            ])
        );
    }

    #[test]
    fn seed_and_distiller_keep_the_prompt_value_atomic() {
        let e = fixture().engine;
        assert_eq!(
            e.seed_command("agy --model m --mode=plan", "--mode", true)
                .unwrap(),
            argv(&["agy", "--model", "m", "--mode", "plan", "-i", "--mode"])
        );
        assert_eq!(
            e.seed_command("agy --model m --mode ask -i old", "-p x", false)
                .unwrap(),
            argv(&[
                "agy",
                "--model",
                "m",
                "--dangerously-skip-permissions",
                "-i",
                "-p x"
            ])
        );
        assert_eq!(
            e.distiller_command(""),
            argv(&[
                "agy",
                "--model",
                "gemini-3.7-flash-high",
                "--dangerously-skip-permissions",
                "-i",
                ""
            ])
        );
    }

    #[test]
    fn launch_renders_effort_into_the_slug() {
        let f = fixture();
        let launch = |options: LaunchOptions<'_>| {
            f.engine
                .build_launch_command(&options, &mut Vec::new())
                .map_err(|error| error.to_string())
        };
        let writable = |model: &str| {
            Ok(argv(&[
                "agy",
                "--model",
                model,
                "--dangerously-skip-permissions",
            ]))
        };
        assert_eq!(
            launch(LaunchOptions::default()),
            writable("gemini-3.7-flash-high")
        );
        assert_eq!(
            launch(LaunchOptions {
                effort: Some(Effort::Low),
                ..LaunchOptions::default()
            }),
            writable("gemini-3.7-flash-low")
        );
        assert_eq!(
            launch(LaunchOptions {
                effort: Some(Effort::Medium),
                initial_prompt: Some(""),
                browser: true,
                ..LaunchOptions::default()
            }),
            writable("gemini-3.7-flash-medium")
        );
        assert_eq!(
            launch(LaunchOptions {
                effort: Some(Effort::XHigh),
                ..LaunchOptions::default()
            }),
            Err("antigravity supports efforts 1-3 (rendered into the gemini-3.7-flash-low/medium/high slugs); effort 4 has no slug — pass --model for a pro/opus tier instead".to_owned())
        );
        assert_eq!(
            launch(LaunchOptions {
                effort: Some(Effort::Max),
                model: Some("pro"),
                ..LaunchOptions::default()
            }),
            writable("pro")
        );
        assert_eq!(
            launch(LaunchOptions {
                model: Some(""),
                ..LaunchOptions::default()
            }),
            writable("")
        );
        assert_eq!(
            launch(LaunchOptions {
                read_only: true,
                initial_prompt: Some("go"),
                ..LaunchOptions::default()
            }),
            Ok(argv(&[
                "agy",
                "--model",
                "gemini-3.7-flash-high",
                "--mode",
                "plan",
                "-i",
                "go"
            ]))
        );

        let mut launch_env = env(&[("X", "1")]);
        let command = f
            .engine
            .build_launch_command(
                &LaunchOptions {
                    role_priming: Some("Be terse.\n"),
                    ..LaunchOptions::default()
                },
                &mut launch_env,
            )
            .unwrap();
        assert_eq!(Ok(command), writable("gemini-3.7-flash-high"));
        let rules = f.base.join("tx/priming/agy-rules-26e226231c094155.md");
        assert_eq!(
            launch_env,
            env(&[("X", "1"), (RULES_FILE_ENV, rules.to_str().unwrap())])
        );
        assert_eq!(std::fs::read_to_string(rules).unwrap(), "Be terse.\n");
    }

    #[test]
    fn read_only_command_detection() {
        let e = fixture().engine;
        let cases = [
            ("agy --mode plan", true),
            ("agy --mode=plan -i x", true),
            ("agy --mode plan --sandbox", false),
            ("agy --mode plan --dangerously-skip-permissions", false),
            ("agy --mode ask", false),
            ("agy --mode", false),
            ("agy -mode plan --mode ask", false),
            ("agy", false),
            ("agy -i --mode plan", true),
            ("agy --mode plan 'x", false),
        ];
        for (command, expected) in cases {
            assert_eq!(e.is_read_only_command(command), expected, "{command}");
        }
    }

    #[test]
    fn prepare_workspace_binds_the_worktree() {
        let f = fixture();
        let (e, base) = (&f.engine, &f.base);
        let work = base.join("w");
        std::fs::create_dir(&work).unwrap();
        let w = work.to_str().unwrap();
        let no_skills = env(&[("TX_SKILLS", "")]);
        let error = e
            .prepare_workspace("agy --model m", w, &no_skills)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "antigravity hooks template missing at {}/tx/hooks/antigravity/hooks.json.template — run `setup/engines/antigravity.sh install` (tx state tracking needs per-worktree hooks)",
                base.display()
            )
        );
        let template = base.join("tx/hooks/antigravity");
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(template.join("hooks.json.template"), "{\"hooks\":1}\n").unwrap();
        let missing = base.join("nope.md");
        let error = e
            .prepare_workspace(
                "agy --model m",
                w,
                &env(&[
                    ("TX_SKILLS", ""),
                    (RULES_FILE_ENV, missing.to_str().unwrap()),
                ]),
            )
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "antigravity role priming file missing at {} (recorded in $TX_AGY_RULES_FILE) — it is content-addressed under $TX_IDE_HOME/priming/ and should never be removed",
                missing.display()
            )
        );

        let rules = base.join("r.md");
        std::fs::write(&rules, "RULES").unwrap();
        let bound = e
            .prepare_workspace(
                "agy --add-dir /old --model m --log-file=/o -i '--add-dir z'",
                w,
                &env(&[("TX_SKILLS", ""), (RULES_FILE_ENV, rules.to_str().unwrap())]),
            )
            .unwrap();
        let b = base.display();
        assert_eq!(
            bound,
            format!(
                "agy --add-dir {b}/w --log-file {b}/tx/agy-logs/w.log --model m -i '--add-dir z'"
            )
        );
        let settings = base.join("h/.gemini/antigravity-cli/settings.json");
        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            format!("{{\n  \"trustedWorkspaces\": [\n    \"{b}/w\"\n  ]\n}}\n")
        );
        assert_eq!(
            std::fs::read_to_string(work.join(".agents/rules/tx-role.md")).unwrap(),
            "RULES"
        );
        assert_eq!(
            std::fs::read_to_string(work.join(".agents/hooks.json")).unwrap(),
            "{\"hooks\":1}\n"
        );
        assert!(base.join("tx/agy-logs").is_dir());
        assert_eq!(e.prepare_workspace("", w, &[]).unwrap(), "");

        std::fs::write(
            &settings,
            "{\"a\": 1.0, \"trustedWorkspaces\": [\"/z\"], \"u\": \"\u{e9}\"}",
        )
        .unwrap();
        let link = base.join("lnk");
        std::os::unix::fs::symlink(&work, &link).unwrap();
        assert_eq!(
            e.prepare_workspace("agy", link.to_str().unwrap(), &no_skills)
                .unwrap(),
            format!("agy --add-dir {b}/lnk --log-file {b}/tx/agy-logs/lnk.log")
        );
        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            format!(
                "{{\n  \"a\": 1.0,\n  \"trustedWorkspaces\": [\n    \"/z\",\n    \"{b}/lnk\",\n    \"{b}/w\"\n  ],\n  \"u\": \"\\u00e9\"\n}}\n"
            )
        );

        std::fs::write(&settings, "not json").unwrap();
        assert_eq!(
            e.prepare_workspace("agy", &format!("{w}/"), &no_skills)
                .unwrap(),
            format!("agy --add-dir {b}/w/ --log-file {b}/tx/agy-logs/w.log")
        );
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), "not json");
        let leftovers: Vec<_> = std::fs::read_dir(settings.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, ["settings.json"]);
    }

    #[test]
    fn transcript_messages_capture_and_events() {
        let f = fixture();
        let e = &f.engine;
        let transcript = f.base.join("t.jsonl");
        std::fs::write(
            &transcript,
            "{\"source\":\"USER_EXPLICIT\",\"content\":\"hi\"}\n\n  {\"source\":\"SYSTEM\",\"content\":\"x\"}\r{\"source\":\"MODEL\"}\n{\"source\":\"MODEL\",\"content\":null}\n{\"content\":\"n\"}\n",
        )
        .unwrap();
        assert_eq!(
            e.iter_messages(&transcript).unwrap(),
            [
                json!({"role": "user", "text": "hi"}),
                json!({"role": "assistant", "text": ""}),
                json!({"role": "assistant", "text": null}),
            ]
        );
        std::fs::write(
            &transcript,
            "{\"source\":\"MODEL\",\"content\":\"a\"}\nnot json\n",
        )
        .unwrap();
        assert!(matches!(
            e.iter_messages(&transcript),
            Err(EngineError::Json { .. })
        ));
        assert_eq!(
            e.resolve_transcript("cid", "/x"),
            f.base.join(
                "h/.gemini/antigravity-cli/brain/cid/.system_generated/logs/transcript.jsonl"
            )
        );
        assert_eq!(e.locate_transcript("cid", None), None);
        assert_eq!(
            e.capture_session_id(&json!({"conversationId": "c", "transcriptPath": "/p"})),
            Some(CapturedChat {
                session_id: "c".into(),
                transcript_path: "/p".into()
            })
        );
        assert_eq!(e.capture_session_id(&json!({"conversationId": "c"})), None);
        assert_eq!(e.state_for_event("PreInvocation"), Some(State::Working));
        assert_eq!(e.state_for_event("Stop"), Some(State::Waiting));
        assert_eq!(e.state_for_event("SessionStart"), None);
        assert_eq!(e.state_source(), StateSource::HookEvents);
        assert!(e.matches_binary("/opt/agy --model m"));
        assert!(!e.matches_binary("agyx"));
        assert!(e.bundle_sidecars(&transcript, "cid").is_empty());
        e.prepare_chat_for_cwd("cid", "/a", "/b").unwrap();
    }

    #[test]
    fn fork_copies_the_db_and_rewrites_the_embedded_id() {
        let f = fixture();
        let e = &f.engine;
        let conversations = f.base.join("h/.gemini/antigravity-cli/conversations");
        let source = "11111111-2222-3333-4444-555555555555";
        let source_db = conversations.join(format!("{source}.db"));
        assert_eq!(
            e.fork_command("agy --model m", source, false)
                .unwrap_err()
                .to_string(),
            format!(
                "antigravity fork: conversation db not found: {}",
                source_db.display()
            )
        );
        std::fs::create_dir_all(&conversations).unwrap();
        let writer = rusqlite::Connection::open(&source_db).unwrap();
        writer
            .query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
            .unwrap();
        writer
            .execute_batch(
                "CREATE TABLE steps(id INTEGER PRIMARY KEY, cascade TEXT, blob BLOB, n INTEGER);
                 CREATE TABLE \"meta\"(k TEXT UNIQUE, v BLOB);",
            )
            .unwrap();
        let mut blob = b"\x00$".to_vec();
        blob.extend(source.as_bytes());
        blob.push(0xff);
        writer
            .execute(
                "INSERT INTO steps(cascade, blob, n) VALUES (?1, ?2, 7)",
                rusqlite::params![format!("id={source};again={source}"), blob],
            )
            .unwrap();
        writer
            .execute(
                "INSERT INTO steps(cascade, blob, n) VALUES ('other', x'01', NULL)",
                [],
            )
            .unwrap();
        writer
            .execute(
                "INSERT INTO meta VALUES (?1, ?2)",
                rusqlite::params![source, source.as_bytes()],
            )
            .unwrap();

        let command = e
            .fork_command("agy --conversation old --model m -i s", source, true)
            .unwrap();
        let new_id = command[2].clone();
        assert_eq!(new_id.len(), 36);
        assert_eq!(new_id, new_id.to_lowercase());
        assert_eq!(new_id.as_bytes()[14], b'4');
        assert_ne!(new_id, source);
        assert_eq!(
            command,
            argv(&[
                "agy",
                "--conversation",
                &new_id,
                "--model",
                "m",
                "--mode",
                "plan"
            ])
        );
        let fork = rusqlite::Connection::open(conversations.join(format!("{new_id}.db"))).unwrap();
        let steps: Vec<(i64, String, Vec<u8>, Option<i64>)> = fork
            .prepare("SELECT * FROM steps")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let mut expected_blob = b"\x00$".to_vec();
        expected_blob.extend(new_id.as_bytes());
        expected_blob.push(0xff);
        assert_eq!(
            steps,
            [
                (
                    1,
                    format!("id={new_id};again={new_id}"),
                    expected_blob,
                    Some(7)
                ),
                (2, "other".to_owned(), vec![1], None),
            ]
        );
        let meta: (String, Vec<u8>) = fork
            .query_row("SELECT * FROM meta", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(meta, (new_id.clone(), new_id.clone().into_bytes()));
        let untouched: String = writer
            .query_row("SELECT k FROM meta", [], |row| row.get(0))
            .unwrap();
        assert_eq!(untouched, source);

        std::fs::write(conversations.join("bad.db"), "garbage ".repeat(20)).unwrap();
        assert_eq!(
            e.fork_command("agy", "bad", false).unwrap_err().to_string(),
            "antigravity fork surgery failed (version-fragile — verified against agy 1.1.13): file is not a database"
        );
        let mut databases: Vec<_> = std::fs::read_dir(&conversations)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.ends_with(".db"))
            .collect();
        databases.sort();
        let mut expected = vec![
            format!("{source}.db"),
            "bad.db".to_owned(),
            format!("{new_id}.db"),
        ];
        expected.sort();
        assert_eq!(databases, expected);
    }

    #[test]
    fn flag_parts_and_log_names() {
        assert_eq!(flag_parts("--a=b=c"), Some(("a", Some("b=c"))));
        assert_eq!(flag_parts("-x"), Some(("x", None)));
        assert_eq!(flag_parts("--"), None);
        assert_eq!(flag_parts("pos"), None);
        assert_eq!(path_name("/a/b/"), "b");
        assert_eq!(path_name("/a/b/."), "b");
        assert_eq!(path_name("/"), "");
        assert_eq!(replace_bytes(b"xaxa", b"xa", b"yb"), b"ybyb");
    }
}
