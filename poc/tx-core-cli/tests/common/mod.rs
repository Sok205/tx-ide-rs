//! Shared harness for the CLI suites: one temp dir under /tmp holding all five directories (short socket path), helpers
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

/// Every directory flag naming `dir`: the suites keep the five directories in one place, so
/// their paths stay `<dir>/sessions`, `<dir>/tmux.sock`, … (the XDG suite separates them).
pub fn directory_flags(dir: &std::path::Path) -> Vec<std::ffi::OsString> {
    [
        "--config-dir",
        "--data-dir",
        "--state-dir",
        "--cache-dir",
        "--run-dir",
    ]
    .into_iter()
    .flat_map(|flag| [flag.into(), dir.as_os_str().to_owned()])
    .collect()
}

/// The variables core exports into every session, all naming `dir`.
pub fn directory_environment(dir: &std::path::Path) -> Vec<(&'static str, std::path::PathBuf)> {
    [
        "TX_CONFIG_DIR",
        "TX_DATA_DIR",
        "TX_STATE_DIR",
        "TX_CACHE_DIR",
        "TX_RUN_DIR",
    ]
    .into_iter()
    .map(|key| (key, dir.to_path_buf()))
    .collect()
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
    /// The directory flags, space-separated, as a shell command line carries them.
    pub fn arguments(&self) -> String {
        directory_flags(&self.state())
            .iter()
            .map(|word| word.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }
    pub fn write_yaml(&self, text: &str) {
        std::fs::create_dir_all(self.state()).unwrap();
        std::fs::write(self.state().join("plugins.yaml"), text).unwrap();
    }
    pub fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_txc"));
        command
            .args(directory_flags(&self.state()))
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
    /// `txi` (the internal binary the wired hooks run) against this state dir, the way a hook
    /// inside a session runs it: the exported `TX_*_DIR`, no flags.
    pub fn txi(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_txi"));
        command
            .args(args)
            .envs(directory_environment(&self.state()))
            .env_remove("TX_TMUX_SOCKET")
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().unwrap();
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
    /// A directory of stand-in executables under `<dir>/shim`, returned as a PATH value that
    /// puts it first.
    pub fn shim_path(&self, shims: &[(&str, &str)]) -> String {
        shim_path_in(&self.dir.path().join("shim"), shims)
    }
    /// A `claude` that just stays alive, so agent sessions can be created without the engine.
    pub fn fake_claude_path(&self) -> String {
        self.shim_path(&[("claude", "exec sleep 60")])
    }
    /// A git repository with one commit under `<dir>/<name>`: an agent's starting checkout.
    pub fn repository(&self, name: &str) -> PathBuf {
        repository_at(&self.dir.path().join(name))
    }
    /// `<data>/worktrees`, canonical: where every agent's worktree lands.
    pub fn worktrees_dir(&self) -> PathBuf {
        let dir = self.state().join("worktrees");
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }
    /// A Claude agent named `name`, started from a fresh repository `<dir>/<name>` with the
    /// fake `claude` on PATH; its id.
    pub fn create(&self, name: &str) -> String {
        let cwd = self.repository(name);
        let path = self.fake_claude_path();
        let output = self.run(
            &[
                "create",
                name,
                "--cwd",
                cwd.to_str().unwrap(),
                "--engine",
                "claude",
                "--prompt",
                "start",
            ],
            &[("PATH", path.as_str())],
            "",
        );
        assert!(
            output.status.success(),
            "txc create {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}

/// Stand-in executables (`#!/bin/sh` scripts with the given bodies) written into `dir`, returned
/// as a PATH value that puts `dir` first.
pub fn shim_path_in(dir: &std::path::Path, shims: &[(&str, &str)]) -> String {
    std::fs::create_dir_all(dir).unwrap();
    for (name, body) in shims {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// A git repository with one commit at `repository` (created if missing, left alone if it is one
/// already): an agent's starting checkout, isolated from the host's git config.
pub fn repository_at(repository: &std::path::Path) -> PathBuf {
    if repository.join(".git").exists() {
        return repository.to_path_buf();
    }
    std::fs::create_dir_all(repository).unwrap();
    std::fs::write(repository.join("README.md"), "hello\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "README.md"],
        &["commit", "-q", "-m", "init"],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    repository.to_path_buf()
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

/// The record's `state`: `starting`, `idle`, `working` or `ended`.
pub fn state_of(cli: &Cli, token: &str) -> String {
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["show", token])).unwrap();
    json["state"].as_str().unwrap().to_owned()
}

/// Where a session runs: its record's `working_directory`, the agent's worktree.
pub fn cwd_of(cli: &Cli, token: &str) -> PathBuf {
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["show", token])).unwrap();
    PathBuf::from(json["working_directory"].as_str().unwrap())
}

/// `<data>/agents/<id>/agent.json`: where the record of `id` lives.
pub fn record_path(cli: &Cli, id: &str) -> PathBuf {
    cli.state().join("agents").join(id).join("agent.json")
}

/// Writes a version-2 record of an ended agent by hand, as a build before this one might have
/// left it: `engine` is any string, so an unknown engine can be planted too.
pub fn plant_record(cli: &Cli, id: &str, name: &str, engine: &str) -> PathBuf {
    let path = record_path(cli, id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 2, "id": id, "name": name, "engine": engine, "working_directory": "/",
            "roles": [], "state": "ended", "created_at": 1.0, "last_modified_at": 2.0,
            "archived_at": null, "tags": [], "parent_id": null,
        })
        .to_string(),
    )
    .unwrap();
    path
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
