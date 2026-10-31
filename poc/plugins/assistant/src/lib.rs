//! tx-core plugin `assistant`: the `tx-assistant` agent session. `assistant warm` starts it
//! unless it is running and prints its name; `txc ui attach` calls it to land the operator in
//! the assistant. Ships the TX-ASSISTANT role as an `agents/` resource.
//!
//! Every agent runs in a worktree of a git checkout, so the assistant has a small repository of
//! its own under the plugin's data directory (`home/`), made on first use. A stopped assistant
//! keeps its worktree, so the next one is `tx-assistant-2`: the running assistant is found by
//! its role, and `warm` prints the name it really has.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use tx_core::{
    CoreError, CreateOptions, Launch, LaunchOptions, Lifecycle, ListFilter, Plugin,
    PluginError, Registry, Session, SessionId, TxCore, Verb, VerbError, VerbIo,
};

pub const PLUGIN_ID: &str = "assistant";
pub const SESSION_NAME: &str = "tx-assistant";
pub const ROLE: &str = "TX-ASSISTANT";
pub const COMMON_ROLE: &str = "COMMON";
pub const TAG: &str = "tx-system";
pub const ENGINE: &str = "claude";
pub const MODEL: &str = "opus";
pub const EFFORT: &str = "medium";
/// The first turn, typed by core at launch: an agent needs an initial prompt.
pub const GREETING: &str = "You are starting up. Reply with the single word READY.";
/// Under the plugin's data directory: the assistant's own repository, the checkout its
/// worktrees branch from.
pub const HOME_DIR: &str = "home";

#[derive(Clone, Copy, Debug, Default)]
pub struct AssistantPlugin;

impl Plugin for AssistantPlugin {
    fn id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn register(&self, r: &mut Registry) -> Result<(), PluginError> {
        r.verb(Arc::new(Assistant));
        r.resource(
            "agents/TX-ASSISTANT.md",
            include_bytes!("../../../../agents/TX-ASSISTANT.md"),
        )
    }
}

/// `<data>/plugins/assistant/home`: the assistant's own repository.
pub fn home(core: &TxCore) -> PathBuf {
    core.config().plugin_data_dir(PLUGIN_ID).join(HOME_DIR)
}

/// The assistant's repository, made with one commit when it does not exist yet. Git's own
/// identity settings are not needed: the commit names tx.
pub fn ensure_home(core: &TxCore) -> Result<PathBuf, CoreError> {
    let home = home(core);
    if home.join(".git").is_dir() {
        return Ok(home);
    }
    std::fs::create_dir_all(&home).map_err(|source| io_error(&home, source))?;
    let readme = home.join("README.md");
    std::fs::write(
        &readme,
        "The tx-assistant's home repository. Each assistant runs in a worktree of it.\n",
    )
    .map_err(|source| io_error(&readme, source))?;
    git(&home, &["init", "-q", "-b", "main"])?;
    git(&home, &["add", "README.md"])?;
    git(&home, &["commit", "-q", "-m", "tx-assistant home"])?;
    Ok(home)
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
pub fn create_options(core: &TxCore) -> Result<CreateOptions, CoreError> {
    let launch = LaunchOptions::agent(ENGINE)
        .profile([COMMON_ROLE, ROLE])
        .option("model", MODEL)
        .option("effort", EFFORT)
        .initial_prompt(GREETING);
    Ok(CreateOptions::new(SESSION_NAME, ensure_home(core)?, launch).tags([TAG]))
}

/// An agent launched with the TX-ASSISTANT role, whatever its name ended up as.
fn is_assistant(session: &Session) -> bool {
    matches!(&session.launch, Launch::Agent { profile, .. } if profile.iter().any(|role| role == ROLE))
}

/// The running assistant's id, after a liveness check; `None` when there is none. Found by its
/// tag and role, not its name: a restart after a stop is `tx-assistant-2`, the stopped one
/// keeping its name and its worktree.
pub fn running(core: &TxCore) -> Result<Option<SessionId>, CoreError> {
    let filter = ListFilter::new().tag(TAG).lifecycle(Lifecycle::Running);
    for id in core.list_sessions(&filter)? {
        let session = core.refresh_session(&id)?.session().clone();
        if session.lifecycle == Lifecycle::Running && is_assistant(&session) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// The running assistant, started when there is none. Nothing is removed: a new assistant is a
/// new agent, with its own worktree (respawn in place will reuse the stopped one's instead).
pub fn warm(core: &TxCore) -> Result<SessionId, CoreError> {
    match running(core)? {
        Some(id) => Ok(id),
        None => core.create_session(create_options(core)?),
    }
}

pub struct Assistant;

impl Verb for Assistant {
    fn name(&self) -> &'static str {
        "assistant"
    }

    fn usage(&self) -> &'static str {
        "assistant warm   start the tx-assistant session unless it is running; prints its name \
         (tx-assistant, or tx-assistant-2 … after a stop)"
    }

    fn run(&self, core: &TxCore, io: &mut VerbIo<'_>, argv: &[String]) -> Result<i32, VerbError> {
        match argv {
            [sub] if sub == "warm" => {
                let id = warm(core)?;
                let name = core.get_session(&id)?.name;
                writeln!(io.stdout, "{name}")
                    .map_err(|error| VerbError::Failed(format!("write: {error}")))?;
                Ok(0)
            }
            _ => {
                let _ = writeln!(io.stderr, "usage: txc assistant warm");
                Ok(2)
            }
        }
    }
}
