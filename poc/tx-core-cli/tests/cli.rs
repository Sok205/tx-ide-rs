//! End-to-end over the built `txc` binary and a private tmux server.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

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

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_txc"));
        command
            .args(directory_flags(&self.state()))
            .args(args)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID");
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "txc {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn fails(&self, args: &[&str], code: i32) -> String {
        let output = self.run(args);
        assert_eq!(
            output.status.code(),
            Some(code),
            "txc {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    fn workdir(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-S")
            .arg(self.state().join("tmux.sock"))
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}

fn wait_for(path: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn usage_errors_exit_2_without_touching_state() {
    let cli = Cli::new();
    let stderr = cli.fails(&[], 2);
    assert!(
        stderr.contains("missing verb") && stderr.contains("usage: txc"),
        "{stderr}"
    );
    assert!(cli.fails(&["create", "x"], 2).contains("--cwd"));
    assert!(cli.fails(&["bogus"], 2).contains("bogus"));
    assert!(!cli.state().exists());
    assert!(cli.ok(&["help"]).starts_with("usage: txc"));
}

#[test]
fn engines_are_registered() {
    let cli = Cli::new();
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\n");
    assert!(cli.state().join("tmux.conf").is_file());
}

#[test]
fn core_errors_exit_1() {
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    let stderr = cli.fails(&["show", "nope"], 1);
    assert_eq!(stderr, "txc: no session \"nope\"\n");
    let stderr = cli.fails(
        &[
            "create",
            "a",
            "--cwd",
            cwd.to_str().unwrap(),
            "--engine",
            "ghost",
            "--prompt",
            "start",
        ],
        1,
    );
    assert!(stderr.contains("ghost"), "{stderr}");
}

#[test]
fn process_session_round_trip() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux not available; skipping");
        return;
    }
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    let cwd_str = cwd.to_str().unwrap();
    let id = cli
        .ok(&[
            "create",
            "sh-1",
            "--cwd",
            cwd_str,
            "--tag",
            "t",
            "--env",
            "GREETING=hi",
            "--exec",
            "sh",
            "-c",
            "echo $GREETING > env.txt; cat > input.txt",
        ])
        .trim()
        .to_owned();
    assert_eq!(id.len(), 36);
    assert_eq!(
        wait_for(&cwd.join("env.txt"), Duration::from_secs(5)),
        "hi\n"
    );

    let ls = cli.ok(&["ls"]);
    let fields: Vec<&str> = ls.trim().split('\t').collect();
    assert_eq!(fields[0], id);
    assert_eq!(fields[1], "sh-1");
    assert_eq!(fields[2], "running");
    assert_eq!(fields[3], cwd.canonicalize().unwrap().to_str().unwrap());
    assert_eq!(cli.ok(&["ls", "--tag", "zzz"]), "");
    assert_eq!(cli.ok(&["ls", "--lifecycle", "running"]).lines().count(), 1);

    let shown: serde_json::Value = serde_json::from_str(&cli.ok(&["show", "sh-1"])).unwrap();
    assert_eq!(shown["id"], id);
    assert_eq!(shown["tags"], serde_json::json!(["t"]));
    assert_eq!(cli.ok(&["show", &id]), cli.ok(&["show", "sh-1"]));

    assert_eq!(
        cli.ok(&["rename", "sh-1", "sh-2"]).trim(),
        format!("{id}\tsh-2")
    );
    assert_eq!(
        cli.ok(&["tag", "sh-2", "a", "b"]).trim(),
        format!("{id}\ta,b")
    );
    assert_eq!(
        cli.ok(&["tag", "sh-2"]).trim_end_matches('\n'),
        format!("{id}\t")
    );

    let where_ = cli.ok(&["where", "sh-2"]);
    let fields: Vec<&str> = where_.trim().split('\t').collect();
    assert_eq!(fields[0], cli.state().join("tmux.sock").to_str().unwrap());
    assert!(
        fields[1].starts_with('$') && fields[2].starts_with('%'),
        "{where_}"
    );

    cli.ok(&["send", "sh-2", "hello", "there;"]);
    cli.ok(&["send", "sh-2", "--", "--flag"]);
    // Inside tx's server, `send` carries the caller's id; elsewhere it does not.
    let fake_tmux = format!("{},0,0", cli.state().join("tmux.sock").display());
    let output = cli.run_env(
        &["send", "sh-2", "from me"],
        &[("TMUX", &fake_tmux), ("TX_SESSION_ID", &id)],
    );
    assert!(output.status.success());
    let output = cli.run_env(
        &["send", "sh-2", "not me"],
        &[("TMUX", "/tmp/other,0,0"), ("TX_SESSION_ID", &id)],
    );
    assert!(output.status.success());
    let typed = loop {
        let text = wait_for(&cwd.join("input.txt"), Duration::from_secs(5));
        if text.lines().count() >= 4 {
            break text;
        }
    };
    assert_eq!(
        typed,
        "<from-user>hello there;</from-user>\n<from-user>--flag</from-user>\n<from-agent session=\"sh-2\">from me</from-agent>\n<from-user>not me</from-user>\n"
    );

    assert_eq!(cli.ok(&["refresh"]), "");
    assert_eq!(cli.ok(&["refresh", "sh-2"]), "");
    assert_eq!(cli.ok(&["stop", "sh-2"]).trim(), format!("{id}\tstopped"));
    assert!(cli.fails(&["where", "sh-2"], 1).contains("not running"));
    assert!(!cli.tmux(&["has-session", "-t", "=sh-2"]).status.success());
    assert_eq!(
        cli.ok(&["forget", "sh-2"]).trim(),
        format!("{id}\tforgotten")
    );
    assert!(cli.fails(&["forget", "sh-2"], 1).contains("no session"));
    assert_eq!(cli.ok(&["ls"]), "");
}

#[test]
fn an_agent_reaches_its_own_core_through_the_session_environment() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    // Relative directory flags still yield absolute paths inside the session environment.
    let rel = Command::new(env!("CARGO_BIN_EXE_txc"))
        .current_dir(cli.dir.path())
        .args([
            "--config-dir",
            "config",
            "--data-dir",
            "data",
            "--state-dir",
            "state",
            "--cache-dir",
            "cache",
            "--run-dir",
            "run",
            "create",
            "me",
            "--cwd",
            cwd.to_str().unwrap(),
            "--exec",
            "sh",
            "-c",
            "env > env.txt; cat > input.txt",
        ])
        .env_remove("TMUX")
        .env_remove("TX_SESSION_ID")
        .env_remove("TX_APPNAME")
        .output()
        .unwrap();
    assert!(
        rel.status.success(),
        "{}",
        String::from_utf8_lossy(&rel.stderr)
    );
    let id = String::from_utf8_lossy(&rel.stdout).trim().to_owned();
    let env = wait_for(&cwd.join("env.txt"), Duration::from_secs(5));
    let get = |key: &str| {
        env.lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .map(str::to_owned)
    };
    let socket = get("TX_TMUX_SOCKET").unwrap();
    for (key, name) in [
        ("TX_CONFIG_DIR", "config"),
        ("TX_DATA_DIR", "data"),
        ("TX_STATE_DIR", "state"),
        ("TX_CACHE_DIR", "cache"),
        ("TX_RUN_DIR", "run"),
    ] {
        let value = get(key).unwrap_or_else(|| panic!("{key} not exported"));
        assert!(Path::new(&value).is_absolute(), "{key}={value}");
        // config and cache are not created by opening the core; compare through the parent.
        let value = Path::new(&value);
        assert_eq!(value.file_name().unwrap(), name, "{key}");
        assert_eq!(
            value.parent().unwrap().canonicalize().unwrap(),
            cli.dir.path().canonicalize().unwrap(),
            "{key}"
        );
    }
    assert_eq!(get("TX_APPNAME").as_deref(), Some("tx-ide"));
    assert_eq!(get("TX_CORE_HOME"), None);
    assert!(Path::new(&socket).is_absolute());
    assert_eq!(
        Path::new(&socket).canonicalize().unwrap(),
        cli.dir.path().join("run/tmux.sock").canonicalize().unwrap()
    );
    // From inside the session: no flags, only the exported environment.
    let mut inner = Command::new(env!("CARGO_BIN_EXE_txc"));
    inner
        .args(["send", "me", "hello self"])
        .env("TX_TMUX_SOCKET", &socket)
        .env("TMUX", format!("{socket},0,0"))
        .env("TX_SESSION_ID", &id)
        .env_remove("HOME")
        .current_dir("/");
    for key in [
        "TX_CONFIG_DIR",
        "TX_DATA_DIR",
        "TX_STATE_DIR",
        "TX_CACHE_DIR",
        "TX_RUN_DIR",
    ] {
        inner.env(key, get(key).unwrap());
    }
    let inner = inner.output().unwrap();
    let typed = wait_for(&cwd.join("input.txt"), Duration::from_secs(5));
    let _ = Command::new("tmux")
        .arg("-S")
        .arg(&socket)
        .arg("kill-server")
        .output();
    assert!(
        inner.status.success(),
        "{}",
        String::from_utf8_lossy(&inner.stderr)
    );
    assert_eq!(
        typed,
        "<from-agent session=\"me\">hello self</from-agent>\n"
    );
}

#[test]
fn refresh_reports_vanished_sessions() {
    if Command::new("tmux").arg("-V").output().is_err() {
        return;
    }
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    let id = cli
        .ok(&[
            "create",
            "v",
            "--cwd",
            cwd.to_str().unwrap(),
            "--exec",
            "sleep",
            "30",
        ])
        .trim()
        .to_owned();
    assert!(cli.tmux(&["kill-session", "-t", "=v"]).status.success());
    assert_eq!(cli.ok(&["refresh"]).trim(), format!("{id}\tv\texited"));
    assert_eq!(cli.ok(&["refresh"]), "");
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
