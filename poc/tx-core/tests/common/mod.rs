//! Shared harness for the integration suites: a temp state dir under /tmp (short socket path),
//! a shell-stub engine, and a kill-server on drop so no private server outlives its test.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tx_core::{
    CoreConfig, CreateOptions, EngineAdapter, EngineError, LaunchOptions, LaunchPlan,
    LaunchRequest, SessionId, TxCore, WorkspaceAction,
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

/// A state dir under /tmp; the tmux server behind it is killed on drop.
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

    pub fn state_dir(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    pub fn socket(&self) -> PathBuf {
        self.state_dir().join("tmux.sock")
    }

    pub fn config(&self) -> CoreConfig {
        CoreConfig::new(self.state_dir())
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
        std::fs::read_to_string(self.state_dir().join("log.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    pub fn record_bytes(&self, id: &SessionId) -> Vec<u8> {
        std::fs::read(self.state_dir().join(format!("sessions/{id}.json"))).unwrap()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
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
pub struct ShellStub;

pub const STUB_ID: &str = "stub";

impl EngineAdapter for ShellStub {
    fn id(&self) -> &str {
        STUB_ID
    }

    fn launch(&self, req: &LaunchRequest<'_>) -> Result<LaunchPlan, EngineError> {
        if req.model == Some("refuse") {
            return Err(EngineError::Refused("model refused".into()));
        }
        let script = format!(
            "env > env.txt; echo \"{}\" > prompt.txt; cat > input.txt",
            req.initial_prompt.unwrap_or("")
        );
        Ok(LaunchPlan::new("sh")
            .args(["-c".to_owned(), script])
            .env("STUB_PROFILE", req.profile.file().display().to_string())
            .env(
                "STUB_EFFORT",
                req.effort.map(|e| e.as_str()).unwrap_or("none"),
            )
            .env("FROM_PLAN", "plan")
            .env("STUB_RESUME", req.resume.unwrap_or(""))
            .workspace(WorkspaceAction::Write {
                path: "stub-marker.txt".into(),
                content: req.profile.text().to_owned(),
            }))
    }
}

/// A `Process` session that keeps stdin open and records what it is typed.
pub fn process(name: &str, cwd: &Path) -> CreateOptions {
    CreateOptions::new(
        name,
        cwd,
        LaunchOptions::process("sh", ["-c", "env > env.txt; cat > input.txt"]),
    )
}

pub fn agent(name: &str, cwd: &Path) -> CreateOptions {
    CreateOptions::new(name, cwd, LaunchOptions::agent(STUB_ID))
}

/// The lines typed into a session so far (its `input.txt`).
pub fn typed(cwd: &Path) -> String {
    std::fs::read_to_string(cwd.join("input.txt")).unwrap_or_default()
}
