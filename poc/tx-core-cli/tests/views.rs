//! The views plugin through the binary: the picker feed `txc _list`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}

struct Cli {
    dir: tempfile::TempDir,
}

impl Cli {
    fn new() -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("txc")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }
    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_txc"));
        command
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID");
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }
    fn run(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
        let mut child = self
            .command(args, env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args, &[], "");
        assert!(
            output.status.success(),
            "txc {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    fn create(&self, name: &str) -> String {
        let cwd = self.dir.path().join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        self.ok(&[
            "create",
            name,
            "--cwd",
            cwd.to_str().unwrap(),
            "--exec",
            "sleep",
            "60",
        ])
        .trim()
        .to_owned()
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(self.state().join("tmux.sock"))
            .arg("kill-server")
            .output();
    }
}

macro_rules! require_tmux {
    () => {
        if !tmux_available() {
            eprintln!("tmux not available; skipping");
            return;
        }
    };
}

fn assert_rows(feed: &str, expected: usize) {
    let lines: Vec<&str> = feed.lines().collect();
    assert_eq!(lines.len(), expected, "{feed:?}");
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 5, "{line:?}");
        assert!(fields[3].starts_with('$'), "{line:?}");
    }
}

#[test]
fn list_prints_one_row_per_running_session() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    cli.create("b");
    assert_rows(&cli.ok(&["_list"]), 2);
}

#[test]
fn list_tolerates_malformed_hooks_ext() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    cli.create("b");
    let record = cli.state().join("sessions").join(format!("{id}.json"));
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    json["ext"] = serde_json::json!({"hooks": 5});
    std::fs::write(&record, json.to_string()).unwrap();
    assert_rows(&cli.ok(&["_list"]), 2);
}

#[test]
fn list_is_empty_newline_without_sessions() {
    require_tmux!();
    let cli = Cli::new();
    assert_eq!(cli.ok(&["_list"]), "\n");
}
