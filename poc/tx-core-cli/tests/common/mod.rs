//! Shared harness for the CLI suites: a temp state dir under /tmp (short socket path), helpers
//! that run `txc` against it, and a kill-server on drop so no private server outlives its test.
#![allow(dead_code)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

/// Whether `txc help` lists `verb` on a line of its own: after the indent, the line is the verb
/// or starts with the verb and a space. A bare substring would match flags and prose.
pub fn lists_verb(help: &str, verb: &str) -> bool {
    help.lines().any(|line| {
        let line = line.trim_start();
        line == verb || line.starts_with(&format!("{verb} "))
    })
}

pub fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}

pub struct Cli {
    pub dir: tempfile::TempDir,
}

impl Cli {
    pub fn new() -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("txc")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }
    pub fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    pub fn write_yaml(&self, text: &str) {
        std::fs::create_dir_all(self.state()).unwrap();
        std::fs::write(self.state().join("plugins.yaml"), text).unwrap();
    }
    pub fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
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
    pub fn run(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
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
    pub fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args, &[], "");
        assert!(
            output.status.success(),
            "txc {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    pub fn fails(&self, args: &[&str], code: i32) -> String {
        let output = self.run(args, &[], "");
        assert_eq!(
            output.status.code(),
            Some(code),
            "txc {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
    pub fn sock(&self) -> PathBuf {
        self.state().join("tmux.sock")
    }
    /// A second tmux server standing in for "the user's own tmux".
    pub fn outer_sock(&self) -> PathBuf {
        self.dir.path().join("outer.sock")
    }
    /// Run `command` in a pane of the outer server (its `$TMUX` names the outer socket).
    pub fn in_outer_pane(&self, command: &str) {
        let outer = |args: &[&str]| {
            Command::new("tmux")
                .arg("-S")
                .arg(self.outer_sock())
                .args(["-f", "/dev/null"])
                .args(args)
                .env_remove("TMUX")
                .output()
                .unwrap()
        };
        assert!(
            outer(&[
                "new-session",
                "-d",
                "-s",
                "outer",
                "-x",
                "120",
                "-y",
                "40",
                "sh"
            ])
            .status
            .success()
        );
        assert!(
            outer(&["send-keys", "-t", "=outer:", command, "Enter"])
                .status
                .success()
        );
    }
    /// Poll `probe` for up to `seconds`.
    pub fn wait_for(&self, seconds: u64, mut probe: impl FnMut() -> bool) -> bool {
        for _ in 0..seconds * 10 {
            if probe() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        probe()
    }
    pub fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-S")
            .arg(self.sock())
            .args(args)
            .env_remove("TMUX")
            .output()
            .unwrap()
    }
    pub fn tmux_out(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.tmux(args).stdout)
            .trim()
            .to_owned()
    }
    pub fn create(&self, name: &str) -> String {
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
        for sock in [self.state().join("tmux.sock"), self.outer_sock()] {
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(sock)
                .arg("kill-server")
                .output();
        }
    }
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

pub fn lifecycle_of(cli: &Cli, token: &str) -> String {
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["show", token])).unwrap();
    json["lifecycle"].as_str().unwrap().to_owned()
}

pub fn pane_id_of(cli: &Cli, name: &str) -> String {
    cli.tmux_out(&[
        "display-message",
        "-p",
        "-t",
        &format!("={name}:"),
        "#{pane_id}",
    ])
}
