//! tx-core plugin `assistant`: the `tx-assistant` agent session. `assistant warm` starts it
//! unless it is running and prints its name; `tx ui attach` calls it to land the operator in
//! the assistant. `assistant ask` delivers one line to it as the operator: prefix+/ (the
//! `keys/assistant.tmux` fragment) parks the typed line in a paste buffer and runs
//! `assistant ask --from-buffer --pane <firing pane>`; the line goes out as typed in that pane's
//! session, behind the focus envelope (`focus`). Ships the TX-ASSISTANT role as an `agents/`
//! resource.
//!
//! Every agent runs in a worktree of a git checkout, so the assistant has a small repository of
//! its own under the plugin's data directory (`home/`), made on first use. A stopped assistant
//! keeps its worktree and its name: `warm` finds the running assistant by its role, else
//! respawns the newest ended one in place, else creates it. Concurrent warms take turns on a lock
//! file, so they never make two.
//!
//! Settings (`plugins.assistant.settings` in `config.yaml`): `model` (default `opus`) and
//! `effort` (default `2`, medium; a number 1-5 or the engine's word), used when a new assistant
//! is created.

mod focus;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tx_client::tmux::Ui;
use tx_core::{
    Agent, AgentId, CoreError, CreateOptions, EngineKind, LaunchOptions, ListFilter,
    LlmEngineState, Plugin, PluginError, Registry, SendOptions, Sender, TxCore, Verb, VerbError,
    VerbIo,
};
use tx_engine_claude::claude_config_file_from_env;
use tx_engine_claude::trust::trust_project;

pub const PLUGIN_ID: &str = "assistant";
pub const SESSION_NAME: &str = "tx-assistant";
pub const ROLE: &str = "TX-ASSISTANT";
pub const COMMON_ROLE: &str = "COMMON";
pub const TAG: &str = "tx-system";
pub const ENGINE: EngineKind = EngineKind::Claude;
pub const DEFAULT_MODEL: &str = "opus";
pub const DEFAULT_EFFORT: &str = "2";
/// The first turn, typed by core at launch: an agent needs an initial prompt.
pub const GREETING: &str = "You are starting up. Reply with the single word READY.";
/// Under the plugin's data directory: the assistant's own repository, the checkout its
/// worktrees branch from.
pub const HOME_DIR: &str = "home";
/// Under the plugin's data directory: the reason the last `assistant warm` failed, removed when a
/// warm succeeds. The home header reads it.
pub const FAILURE_FILE: &str = "warm.failed";
/// Under the plugin's data directory: held for the length of a warm.
const WARM_LOCK_FILE: &str = "warm.lock";
/// The paste buffer prefix+/ fills on the core's server; `ask --from-buffer` reads and deletes it.
pub const INPUT_BUFFER: &str = "tx-assistant-input";
/// What the envelope names as where the operator typed, for a line with no firing pane to name
/// (`ask TEXT…`, or a pane gone before it was read).
pub const VIA: &str = "prefix+/";
/// How long `ask` waits for a freshly started assistant to leave `starting`, so the line is typed
/// into a running engine rather than a loading one.
pub const START_TIMEOUT: Duration = Duration::from_secs(20);
const START_POLL: Duration = Duration::from_millis(250);

/// `plugins.assistant.settings`: the model and effort the assistant is launched with.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub model: String,
    /// A number on the tx scale or an engine's word, as text; the engine checks it.
    #[serde(deserialize_with = "tx_core::deserialize_effort")]
    pub effort: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.to_owned(),
            effort: DEFAULT_EFFORT.to_owned(),
        }
    }
}

impl Settings {
    /// No settings (`null`) are the defaults; an unknown key or a wrong type is refused.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, PluginError> {
        if value.is_null() {
            return Ok(Self::default());
        }
        serde_json::from_value(value.clone()).map_err(|error| PluginError::Register {
            plugin: PLUGIN_ID.to_owned(),
            reason: format!("settings: {error}"),
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AssistantPlugin;

impl Plugin for AssistantPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        let settings = Settings::from_value(r.settings())?;
        r.verb(Arc::new(Assistant { settings }));
        r.resource("keys/assistant.tmux", include_bytes!("../tmux/keys.tmux"))
    }
}

/// `<data>/plugins/assistant/home`: the assistant's own repository.
pub fn home(core: &TxCore) -> PathBuf {
    core.config().plugin_data_dir(PLUGIN_ID).join(HOME_DIR)
}

/// The assistant's repository, made with one commit when it does not exist yet, and trusted in
/// Claude's config on every call (a changed `CLAUDE_CONFIG_DIR`, or a record written by an older
/// build, is covered too). Git's own identity settings are not needed: the commit names tx.
pub fn ensure_home(core: &TxCore) -> Result<PathBuf, CoreError> {
    let home = home(core);
    if !home.join(".git").is_dir() {
        create_home(&home)?;
    }
    if let Err(error) = trust_project(&claude_config_file_from_env(), &home) {
        eprintln!(
            "tx: assistant: claude trust for {}: {error}",
            home.display()
        );
    }
    Ok(home)
}

fn create_home(home: &Path) -> Result<(), CoreError> {
    std::fs::create_dir_all(home).map_err(|source| io_error(home, source))?;
    let readme = home.join("README.md");
    std::fs::write(
        &readme,
        "The tx-assistant's home repository. Each assistant runs in a worktree of it.\n",
    )
    .map_err(|source| io_error(&readme, source))?;
    git(home, &["init", "-q", "-b", "main"])?;
    git(home, &["add", "README.md"])?;
    git(home, &["commit", "-q", "-m", "tx-assistant home"])?;
    Ok(())
}

fn git(home: &Path, args: &[&str]) -> Result<(), CoreError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(home)
        .args(args)
        .env("GIT_AUTHOR_NAME", "tx")
        .env("GIT_AUTHOR_EMAIL", "tx@localhost")
        .env("GIT_COMMITTER_NAME", "tx")
        .env("GIT_COMMITTER_EMAIL", "tx@localhost")
        .output()
        .map_err(tx_core::WorktreeError::Spawn)?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(tx_core::WorktreeError::Git(stderr).into())
}

fn io_error(path: &Path, source: std::io::Error) -> CoreError {
    tx_core::WorktreeError::Io {
        path: path.to_owned(),
        source,
    }
    .into()
}

/// The assistant session: COMMON + TX-ASSISTANT on claude, greeted with `GREETING`, tagged
/// `tx-system`, in a worktree of its own repository (so the engine's skill links land there, not
/// in `$HOME`).
pub fn create_options(core: &TxCore, settings: &Settings) -> Result<CreateOptions, CoreError> {
    let launch = LaunchOptions::agent(ENGINE)
        .profile([COMMON_ROLE, ROLE])
        .option("model", settings.model.as_str())
        .option("effort", settings.effort.as_str())
        .initial_prompt(GREETING);
    Ok(CreateOptions::new(SESSION_NAME, ensure_home(core)?, launch).tags([TAG]))
}

/// An agent launched with the TX-ASSISTANT role, whatever its name ended up as.
fn is_assistant(session: &Agent) -> bool {
    session.roles.iter().any(|role| role == ROLE)
}

/// The running assistant's id, after a liveness check; `None` when there is none. Found by its
/// tag and role, not its name.
pub fn running(core: &TxCore) -> Result<Option<AgentId>, CoreError> {
    let filter = ListFilter::new().tag(TAG).running();
    for id in core.list_agents(&filter)? {
        let session = core.refresh_agent(&id)?.agent().clone();
        if session.state != LlmEngineState::Ended && is_assistant(&session) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// The newest ended assistant, by tag and role; `None` when there is none.
pub fn latest_ended(core: &TxCore) -> Result<Option<AgentId>, CoreError> {
    let filter = ListFilter::new().tag(TAG).ended();
    let mut newest: Option<Agent> = None;
    for id in core.list_agents(&filter)? {
        let session = core.get_agent(&id)?;
        if is_assistant(&session)
            && newest
                .as_ref()
                .is_none_or(|best| session.last_modified_at > best.last_modified_at)
        {
            newest = Some(session);
        }
    }
    Ok(newest.map(|session| session.id))
}

/// The lock file `warm` holds, taken (waiting for another warm to finish) and released when the
/// returned file is dropped.
fn warm_lock(core: &TxCore) -> Result<File, CoreError> {
    let directory = core.config().plugin_data_dir(PLUGIN_ID);
    std::fs::create_dir_all(&directory).map_err(|source| io_error(&directory, source))?;
    let path = directory.join(WARM_LOCK_FILE);
    let lock = File::create(&path).map_err(|source| io_error(&path, source))?;
    lock.lock().map_err(|source| io_error(&path, source))?;
    Ok(lock)
}

/// The running assistant; else the newest ended one, respawned in place (same name, same
/// worktree); else a new one. One warm at a time. The outcome is left in `FAILURE_FILE` for
/// every caller: the reason on failure, no file after a success.
pub fn warm(core: &TxCore, settings: &Settings) -> Result<AgentId, CoreError> {
    let warmed = start(core, settings);
    record_outcome(core, &warmed);
    warmed
}

fn start(core: &TxCore, settings: &Settings) -> Result<AgentId, CoreError> {
    let _lock = warm_lock(core)?;
    ensure_home(core)?;
    if let Some(id) = running(core)? {
        return Ok(id);
    }
    match latest_ended(core)? {
        Some(id) => Ok(core.respawn_agent(&id)?.id),
        None => core.create_agent(create_options(core, settings)?),
    }
}

/// Delivers `text` to the running assistant (started when there is none) as the operator. Typed
/// in `pane`: via that pane's session, behind its focus envelope; else via `VIA`. A new
/// assistant is given `START_TIMEOUT` to report a state past `starting`; the line goes out either
/// way, since the engine queues input typed while it works.
pub fn ask(
    core: &TxCore,
    settings: &Settings,
    text: &str,
    pane: Option<&str>,
) -> Result<AgentId, CoreError> {
    let ui = Ui::from_core(core);
    let (via, text) = match pane.and_then(|pane| focus::focus(&ui, pane)) {
        Some(focus) => (focus.session_name, format!("{} {text}", focus.envelope)),
        None => (VIA.to_owned(), text.to_owned()),
    };
    let id = warm(core, settings)?;
    let deadline = Instant::now() + START_TIMEOUT;
    while core.get_agent(&id)?.state == LlmEngineState::Starting && Instant::now() < deadline {
        std::thread::sleep(START_POLL);
    }
    let options = SendOptions::from(Sender::User { via: Some(via) });
    core.send_message(&id, &text, options)?;
    Ok(id)
}

/// The line prefix+/ parked in `INPUT_BUFFER` on the core's server, deleted once read; empty when
/// there is no buffer.
fn take_input_buffer(core: &TxCore) -> String {
    let ui = Ui::from_core(core);
    let (found, text) = ui.quiet(&["show-buffer", "-b", INPUT_BUFFER]);
    if found {
        ui.quiet(&["delete-buffer", "-b", INPUT_BUFFER]);
    }
    text
}

/// `ask` with the line prefix+/ parked; nothing when the line is blank.
fn ask_from_buffer(
    core: &TxCore,
    settings: &Settings,
    pane: Option<&str>,
) -> Result<(), CoreError> {
    let text = take_input_buffer(core);
    if text.trim().is_empty() {
        return Ok(());
    }
    ask(core, settings, text.trim_end_matches('\n'), pane)?;
    Ok(())
}

/// Leaves the reason a warm failed in `FAILURE_FILE`, or removes the file after a warm that
/// worked. Best effort: the file is a hint for the home header.
fn record_outcome(core: &TxCore, warmed: &Result<AgentId, CoreError>) {
    let directory = core.config().plugin_data_dir(PLUGIN_ID);
    let file = directory.join(FAILURE_FILE);
    match warmed {
        Ok(_) => {
            let _ = std::fs::remove_file(&file);
        }
        Err(error) => {
            let _ = std::fs::create_dir_all(&directory);
            let _ = std::fs::write(&file, error.to_string());
        }
    }
}

pub struct Assistant {
    settings: Settings,
}

impl Verb for Assistant {
    fn name(&self) -> &'static str {
        "assistant"
    }

    fn usage(&self) -> &'static str {
        "assistant warm | ask --from-buffer [--pane PANE] | ask TEXT…   start the tx-assistant \
         session unless it is running (a stopped one is respawned in place) and print its name; \
         deliver a line to it as the operator (from the tx-assistant-input paste buffer, \
         or the words given): typed in PANE's session behind its focus envelope, else via prefix+/"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv {
            [sub] if sub == "warm" => {
                let id = warm(core, &self.settings)?;
                let name = core.get_agent(&id)?.name;
                writeln!(io.stdout, "{name}")
                    .map_err(|error| VerbError::Failed(format!("write: {error}")))?;
                Ok(0)
            }
            [sub, flag] if sub == "ask" && flag == "--from-buffer" => {
                ask_from_buffer(core, &self.settings, None)?;
                Ok(0)
            }
            [sub, flag, pane_flag, pane]
                if sub == "ask" && flag == "--from-buffer" && pane_flag == "--pane" =>
            {
                ask_from_buffer(core, &self.settings, Some(pane))?;
                Ok(0)
            }
            [sub, words @ ..] if sub == "ask" && !words.is_empty() => {
                ask(core, &self.settings, &words.join(" "), None)?;
                Ok(0)
            }
            _ => {
                let _ = writeln!(
                    io.stderr,
                    "usage: tx assistant warm | ask --from-buffer [--pane PANE] | ask TEXT…"
                );
                Ok(2)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn no_settings_are_the_defaults() {
        assert_eq!(
            Settings::from_value(&serde_json::Value::Null).unwrap(),
            Settings {
                model: "opus".to_owned(),
                effort: "2".to_owned(),
            }
        );
    }

    #[test]
    fn a_key_left_out_keeps_its_default() {
        let settings = Settings::from_value(&json!({"model": "sonnet"})).unwrap();
        assert_eq!(settings.model, "sonnet");
        assert_eq!(settings.effort, DEFAULT_EFFORT);
    }

    #[test]
    fn effort_is_a_number_or_a_word() {
        assert_eq!(
            Settings::from_value(&json!({"effort": 4})).unwrap().effort,
            "4"
        );
        assert_eq!(
            Settings::from_value(&json!({"effort": "max"}))
                .unwrap()
                .effort,
            "max"
        );
    }

    #[test]
    fn an_unknown_key_or_a_wrong_type_is_refused() {
        for value in [json!({"modle": "x"}), json!({"effort": true})] {
            let error = Settings::from_value(&value).unwrap_err();
            assert!(
                matches!(&error, PluginError::Register { plugin, .. } if plugin == "assistant"),
                "{error:?}"
            );
        }
    }
}
