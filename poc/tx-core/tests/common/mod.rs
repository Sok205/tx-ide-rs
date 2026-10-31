//! Shared harness for the integration suites: temp directories under /tmp (short socket path),
//! a shell-stub engine, and a kill-server on drop so no private server outlives its test.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tx_core::{
    CoreConfig, CreateOptions, Directories, EngineError, EngineKind, Input, Key, LaunchOptions,
    LaunchPlan, LaunchRequest, LlmEngine, SessionId, TxCore, WorkspaceAction,
};

/// `None` when tmux is not installed; callers skip.
pub fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}

#[macro_export]
macro_rules! require_tmux {
    () => {
        if !$crate::common::tmux_available() {
            eprintln!("tmux not available; skipping");
            return;
        }
    };
}

/// The five directories under one root in /tmp; the tmux server behind them is killed on drop.
pub struct Harness {
    pub dir: tempfile::TempDir,
}

impl Harness {
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("txc")
            .tempdir_in("/tmp")
            .unwrap();
        Self { dir }
    }

    /// The root the five directories live under (`Directories::under`).
    pub fn home(&self) -> PathBuf {
        self.dir.path().join("home")
    }

    pub fn directories(&self) -> Directories {
        Directories::under(self.home())
    }

    pub fn data_dir(&self) -> PathBuf {
        self.directories().data
    }

    pub fn state_dir(&self) -> PathBuf {
        self.directories().state
    }

    pub fn socket(&self) -> PathBuf {
        self.directories().run.join("tmux.sock")
    }

    pub fn config(&self) -> CoreConfig {
        CoreConfig::new(self.directories())
    }

    pub fn open(&self) -> TxCore {
        TxCore::open(self.config()).unwrap()
    }

    /// A core with the shell-stub engine registered.
    pub fn open_with_engine(&self) -> TxCore {
        let mut core = self.open();
        core.register_engine(Box::new(ShellStub)).unwrap();
        core
    }

    /// A fresh working directory for one session.
    pub fn workdir(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join("work").join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    /// A git repository with one commit under `work/<name>`: an agent's starting checkout,
    /// isolated from the host's git config.
    pub fn repository(&self, name: &str) -> PathBuf {
        let repository = self.workdir(name);
        std::fs::write(repository.join("README.md"), "hello\n").unwrap();
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["add", "README.md"],
            &["commit", "-q", "-m", "init"],
        ] {
            let output = git(&repository, args);
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        repository
    }

    /// `<data>/worktrees`: where every agent's worktree lands.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.config().worktrees_dir()
    }

    /// How many worktrees are on disk under `<data>/worktrees/<repository-key>/`.
    pub fn worktree_count(&self) -> usize {
        let Ok(keys) = self.worktrees_dir().read_dir() else {
            return 0;
        };
        keys.flatten()
            .map(|key| key.path().read_dir().unwrap().count())
            .sum()
    }

    /// Raw tmux against the private server.
    pub fn tmux(&self, args: &[&str]) -> std::process::Output {
        Command::new("tmux")
            .arg("-S")
            .arg(self.socket())
            .args(args)
            .output()
            .unwrap()
    }

    pub fn tmux_ok(&self, args: &[&str]) -> String {
        let output = self.tmux(args);
        assert!(
            output.status.success(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Session names on the private server.
    pub fn live_names(&self) -> Vec<String> {
        let output = self.tmux(&["list-sessions", "-F", "#{session_name}"]);
        if !output.status.success() {
            return Vec::new();
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Visible pane contents (whole history) of a session.
    pub fn pane_text(&self, session_target: &str) -> String {
        self.tmux_ok(&["capture-pane", "-p", "-J", "-S", "-", "-t", session_target])
    }

    pub fn log_lines(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.data_dir().join("log.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn record_bytes(&self, id: &SessionId) -> Vec<u8> {
        std::fs::read(self.data_dir().join(format!("agents/{id}/agent.json"))).unwrap()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}

/// `git -C <directory> <args>`, with the host's git config out of the way.
pub fn git(directory: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
}

/// Trimmed stdout of a git command that must succeed.
pub fn git_ok(directory: &Path, args: &[&str]) -> String {
    let output = git(directory, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Every checkout git lists for the repository `directory` belongs to, canonical, main first.
pub fn git_worktrees(directory: &Path) -> Vec<PathBuf> {
    git_ok(directory, &["worktree", "list", "--porcelain"])
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|path| PathBuf::from(path).canonicalize().unwrap())
        .collect()
}

/// Where a session runs: its record's `cwd`, the agent's worktree.
pub fn cwd_of(core: &TxCore, id: &SessionId) -> PathBuf {
    core.get_session(id).unwrap().working_directory
}

/// Waits up to `timeout` for `condition`.
pub fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    condition()
}

/// A test engine: runs `sh -c <script>` where the script records its environment and reads
/// stdin forever (so messages land in a file), and writes a marker file via the workspace.
/// The `model` option picks a variant: `exit` runs `true` (a program that exits at once),
/// `tough` ignores the hang-up a closed pane sends (as a CLI busy with its exit hooks might),
/// `data` names its conversation up front (as Claude's plan does), `refuse` is refused by
/// `configure`.
pub struct ShellStub;

/// The kind the stub registers as; a core with the stub has no Codex.
pub const STUB_KIND: EngineKind = EngineKind::Claude;
pub const STUB_ID: &str = "stub";

impl LlmEngine for ShellStub {
    /// A hook named after a state moves to it; `data` rewrites the engine data to the payload
    /// (`{}` clears nothing: it is the complete new value); any other name changes nothing.
    fn on_hook(
        &self,
        _: &tx_core::Session,
        hook: &tx_core::LlmEngineHook,
    ) -> tx_core::LlmEngineChange {
        if hook.name == "data" {
            return tx_core::LlmEngineChange::NONE.engine_data(hook.payload.clone());
        }
        tx_core::LlmEngineChange {
            state: tx_core::LlmEngineState::ALL
                .into_iter()
                .find(|state| state.as_str() == hook.name),
            engine_data: None,
        }
    }

    fn kind(&self) -> EngineKind {
        STUB_KIND
    }

    fn ext_key(&self) -> &'static str {
        STUB_ID
    }

    /// Merges `options` into the data; model `refuse` is refused, as a real engine refuses a
    /// bad option. The launch then reads `model` and `effort` from the data.
    fn configure(
        &self,
        session: &tx_core::Session,
        options: &serde_json::Value,
    ) -> Result<serde_json::Value, EngineError> {
        if options["model"] == "refuse" {
            return Err(EngineError::Refused("model refused".into()));
        }
        let mut data = tx_core::engine_data(self, session)
            .as_object()
            .cloned()
            .unwrap_or_default();
        data.extend(options.as_object().cloned().unwrap_or_default());
        Ok(serde_json::Value::Object(data))
    }

    fn start(&self, request: &LaunchRequest<'_>, prompt: &str) -> Result<LaunchPlan, EngineError> {
        Ok(self.plan(request, prompt, "start", ""))
    }

    /// A resume reads its conversation from the record, as a real engine does.
    fn resume(&self, request: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        let conversation = request.session.ext[STUB_ID]["conversation_id"]
            .as_str()
            .unwrap();
        Ok(self.plan(request, "", "resume", conversation))
    }

    fn fork(
        &self,
        request: &LaunchRequest<'_>,
        source: &tx_core::Session,
        prompt: &str,
    ) -> Result<LaunchPlan, EngineError> {
        Ok(self.plan(request, prompt, "fork", source.id.as_str()))
    }

    /// End of input: `cat` returns and the shell exits, as a CLI's own exit command would.
    /// The `tough` model ignores it, so core's kill fallback is what ends that one.
    fn stop(&self, _: &tx_core::Session) -> Option<Input> {
        Some(Input::Key(Key::ControlD))
    }

    /// The stub has no turns; what core types is visible in `input.txt`, and it says it is idle
    /// afterwards, as the real engines do.
    fn interrupt(&self, _: &tx_core::Session) -> Option<(Input, Option<tx_core::LlmEngineState>)> {
        Some((
            Input::line("INTERRUPT"),
            Some(tx_core::LlmEngineState::Idle),
        ))
    }

    /// The data's `transcript_path` mirrored to `<archive>/<conversation_id>.jsonl`, as Claude
    /// does: by offset while running, compared whole once the record is terminal; no path or no
    /// file yet archives nothing; the path `refuse` is refused, as an unreadable transcript
    /// would fail.
    fn archive(
        &self,
        session: &tx_core::Session,
        archive_directory: &Path,
    ) -> Result<(), EngineError> {
        let Some((conversation, transcript)) = stub_conversation(session) else {
            return Ok(());
        };
        if transcript == Path::new("refuse") {
            return Err(EngineError::Refused("archive refused".into()));
        }
        if !transcript.is_file() {
            return Ok(());
        }
        let archived = archive_directory.join(format!("{conversation}.jsonl"));
        if session.state == tx_core::LlmEngineState::Ended {
            tx_core::mirror_file_exact(&transcript, &archived)
        } else {
            tx_core::mirror_file(&transcript, &archived)
        }
    }

    /// The archived transcript back to `transcript_path` when the live one is gone.
    fn restore(
        &self,
        session: &tx_core::Session,
        archive_directory: &Path,
    ) -> Result<bool, EngineError> {
        let Some((conversation, transcript)) = stub_conversation(session) else {
            return Ok(false);
        };
        let archived = archive_directory.join(format!("{conversation}.jsonl"));
        if transcript.is_file() || !archived.is_file() {
            return Ok(false);
        }
        tx_core::mirror_file(&archived, &transcript)?;
        Ok(true)
    }
}

impl ShellStub {
    /// The plan every launch shares: the shell script with `prompt`, and `mode` / `resume` in
    /// the environment so a test can read how it was opened.
    fn plan(
        &self,
        request: &LaunchRequest<'_>,
        prompt: &str,
        mode: &str,
        resume: &str,
    ) -> LaunchPlan {
        let data = tx_core::engine_data(self, request.session);
        let option = |key: &str| data.get(key).and_then(serde_json::Value::as_str);
        let script = format!("env > env.txt; echo \"{prompt}\" > prompt.txt; cat > input.txt");
        let (program, args) = match option("model") {
            Some("exit") => ("true", Vec::new()),
            Some("tough") => (
                "sh",
                vec![
                    "-c".to_owned(),
                    "trap '' HUP TERM; env > env.txt; sleep 120".to_owned(),
                ],
            ),
            _ => ("sh", vec!["-c".to_owned(), script]),
        };
        let plan = LaunchPlan::new(program)
            .args(args)
            .env("STUB_PROFILE", request.profile.file().display().to_string())
            .env("STUB_EFFORT", option("effort").unwrap_or("none"))
            .env("FROM_PLAN", "plan")
            .env("STUB_MODE", mode)
            .env("STUB_RESUME", resume)
            .env("STUB_SESSION_ID", request.session.id.as_str())
            .workspace(WorkspaceAction::Write {
                path: "stub-marker.txt".into(),
                content: request.profile.text().to_owned(),
            });
        // Model `data`: the plan names the conversation up front, as Claude's does.
        match option("model") {
            Some("data") => plan.engine_data(serde_json::json!({
                "conversation_id": request.session.id.as_str(),
                "model": "data",
            })),
            _ => plan,
        }
    }
}

/// The stub's `(conversation_id, transcript_path)` from its data, when both are set.
fn stub_conversation(session: &tx_core::Session) -> Option<(String, PathBuf)> {
    let data = tx_core::engine_data(&ShellStub, session);
    let conversation = data.get("conversation_id")?.as_str()?.to_owned();
    let transcript = PathBuf::from(data.get("transcript_path")?.as_str()?);
    Some((conversation, transcript))
}

/// A stub agent that keeps stdin open and records what it is typed (`input.txt` in its
/// worktree, see [`typed`]) and its environment (`env.txt`). `cwd` must be a git checkout
/// (`Harness::repository`); the agent runs in a worktree of it (`cwd_of`).
pub fn agent(name: &str, cwd: &Path) -> CreateOptions {
    CreateOptions::new(
        name,
        cwd,
        LaunchOptions::agent(STUB_KIND).initial_prompt("start"),
    )
}

/// A stub agent whose program exits at once (`true`).
pub fn exiting(name: &str, cwd: &Path) -> CreateOptions {
    CreateOptions::new(
        name,
        cwd,
        LaunchOptions::agent(STUB_KIND)
            .initial_prompt("start")
            .option("model", "exit"),
    )
}

/// The lines typed into a session so far (its `input.txt`).
pub fn typed(cwd: &Path) -> String {
    std::fs::read_to_string(cwd.join("input.txt")).unwrap_or_default()
}
