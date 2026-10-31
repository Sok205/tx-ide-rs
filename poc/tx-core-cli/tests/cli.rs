//! End-to-end over the built `txc` binary and a private tmux server.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Cli, cwd_of};

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
    assert!(
        cli.fails(&["create", "x", "--cwd", "/"], 2)
            .contains("create needs --engine ID")
    );
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
    let cwd = cli.repository("w");
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

/// A `"kind": "process"` record left by a build before core held agents only is skipped by every
/// read, and named on stderr once per invocation.
#[test]
fn process_records_on_disk_are_skipped_and_named_on_stderr() {
    let cli = Cli::new();
    let id = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0";
    let sessions = cli.state().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join(format!("{id}.json")),
        format!(
            r#"{{"version":1,"id":"{id}","name":"old-shell","parent":null,"tags":[],"cwd":"/","created_at":1.0,"ended_at":null,"lifecycle":"running","launch":{{"kind":"process","program":"sh","args":[]}}}}"#
        ),
    )
    .unwrap();
    let output = cli.run(&["ls"], &[], "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(stderr.contains("skipped 1 process record"), "{stderr}");
    assert!(stderr.contains(id), "{stderr}");
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        cli.fails(&["show", "old-shell"], 1)
            .ends_with("no session \"old-shell\"\n")
    );
    assert!(
        sessions.join(format!("{id}.json")).is_file(),
        "nothing is deleted"
    );
}

#[test]
fn agent_session_round_trip() {
    require_tmux!();
    let cli = Cli::new();
    let repository = cli.repository("w");
    // The agent writes its environment's greeting, then captures what is typed at it.
    let path = cli.shim_path(&[("claude", "echo $GREETING > env.txt; exec cat > input.txt")]);
    let output = cli.run(
        &[
            "create",
            "ag-1",
            "--cwd",
            repository.to_str().unwrap(),
            "--tag",
            "t",
            "--env",
            "GREETING=hi",
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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert_eq!(id.len(), 36);
    let cwd = cwd_of(&cli, "ag-1");
    assert_ne!(cwd, repository, "an agent runs in its own worktree");
    assert_eq!(
        wait_for(&cwd.join("env.txt"), Duration::from_secs(5)),
        "hi\n"
    );

    let ls = cli.ok(&["ls"]);
    let fields: Vec<&str> = ls.trim().split('\t').collect();
    assert_eq!(fields[0], id);
    assert_eq!(fields[1], "ag-1");
    assert_eq!(fields[2], "running");
    assert_eq!(fields[3], cwd.to_str().unwrap());
    assert_eq!(cli.ok(&["ls", "--tag", "zzz"]), "");
    assert_eq!(cli.ok(&["ls", "--lifecycle", "running"]).lines().count(), 1);

    let shown: serde_json::Value = serde_json::from_str(&cli.ok(&["show", "ag-1"])).unwrap();
    assert_eq!(shown["id"], id);
    assert_eq!(shown["tags"], serde_json::json!(["t"]));
    assert_eq!(cli.ok(&["show", &id]), cli.ok(&["show", "ag-1"]));

    assert_eq!(
        cli.ok(&["rename", "ag-1", "ag-2"]).trim(),
        format!("{id}\tag-2")
    );
    assert_eq!(
        cli.ok(&["tag", "ag-2", "a", "b"]).trim(),
        format!("{id}\ta,b")
    );
    assert_eq!(
        cli.ok(&["tag", "ag-2"]).trim_end_matches('\n'),
        format!("{id}\t")
    );

    let where_ = cli.ok(&["where", "ag-2"]);
    let fields: Vec<&str> = where_.trim().split('\t').collect();
    assert_eq!(fields[0], cli.sock().to_str().unwrap());
    assert!(
        fields[1].starts_with('$') && fields[2].starts_with('%'),
        "{where_}"
    );

    cli.ok(&["send", "ag-2", "hello", "there;"]);
    cli.ok(&["send", "ag-2", "--", "--flag"]);
    // Inside tx's server, `send` carries the caller's id; elsewhere it does not.
    let fake_tmux = format!("{},0,0", cli.sock().display());
    let output = cli.run(
        &["send", "ag-2", "from me"],
        &[("TMUX", &fake_tmux), ("TX_SESSION_ID", &id)],
        "",
    );
    assert!(output.status.success());
    let output = cli.run(
        &["send", "ag-2", "not me"],
        &[("TMUX", "/tmp/other,0,0"), ("TX_SESSION_ID", &id)],
        "",
    );
    assert!(output.status.success());
    // The operator names where they typed with `--via`; from inside a view the view names it.
    cli.ok(&["send", "--via", "plan.md", "ag-2", "from a buffer"]);
    cli.ok(&[
        "ui", "view", "open", "notes", "--kind", "shell", "--", "sleep", "600",
    ]);
    let view_pane = cli.tmux_out(&["display-message", "-p", "-t", "=notes:", "#{pane_id}"]);
    let output = cli.run(
        &["send", "--via", "ignored", "ag-2", "from notes"],
        &[("TMUX", &fake_tmux), ("TMUX_PANE", &view_pane)],
        "",
    );
    assert!(output.status.success());
    // An id core does not know is no agent: the view still names the sender.
    let output = cli.run(
        &["send", "ag-2", "unknown id"],
        &[
            ("TMUX", &fake_tmux),
            ("TMUX_PANE", &view_pane),
            ("TX_SESSION_ID", "unknown-id"),
        ],
        "",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let typed = loop {
        let text = wait_for(&cwd.join("input.txt"), Duration::from_secs(5));
        if text.lines().count() >= 7 {
            break text;
        }
    };
    assert_eq!(
        typed,
        "<from-user>hello there;</from-user>\n<from-user>--flag</from-user>\n<from-agent session=\"ag-2\">from me</from-agent>\n<from-user>not me</from-user>\n<from-user session=\"plan.md\">from a buffer</from-user>\n<from-user session=\"notes\">from notes</from-user>\n<from-user session=\"notes\">unknown id</from-user>\n"
    );

    assert_eq!(cli.ok(&["refresh"]), "");
    assert_eq!(cli.ok(&["refresh", "ag-2"]), "");
    assert_eq!(cli.ok(&["stop", "ag-2"]).trim(), format!("{id}\tstopped"));
    assert!(cli.fails(&["where", "ag-2"], 1).contains("not running"));
    assert!(!cli.tmux(&["has-session", "-t", "=ag-2"]).status.success());
    assert_eq!(
        cli.ok(&["archive", "ag-2"]).trim(),
        format!("{id}\tarchived")
    );
    assert_eq!(cli.ok(&["ls"]), "");
    assert!(
        cli.ok(&["ls", "--archived"])
            .starts_with(&format!("{id}\tag-2\tstopped\t"))
    );
    assert!(cli.ok(&["show", "ag-2"]).contains("\"archived_at\""));
    assert_eq!(
        cli.ok(&["archive", "ag-2"]).trim(),
        format!("{id}\tarchived"),
        "archiving again changes nothing"
    );
}

#[test]
fn an_agent_reaches_its_own_core_through_the_session_environment() {
    require_tmux!();
    let cli = Cli::new();
    let repository = cli.repository("w");
    let path = cli.shim_path(&[("claude", "env > env.txt; exec cat > input.txt")]);
    // `txc` with relative directory flags, run from the temp dir.
    let relative = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_txc"))
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
            ])
            .args(args)
            .env("PATH", &path)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID")
            .env_remove("TX_APPNAME")
            .output()
            .unwrap()
    };
    // Relative directory flags still yield absolute paths inside the session environment.
    let rel = relative(&[
        "create",
        "me",
        "--cwd",
        repository.to_str().unwrap(),
        "--engine",
        "claude",
        "--prompt",
        "start",
    ]);
    assert!(
        rel.status.success(),
        "{}",
        String::from_utf8_lossy(&rel.stderr)
    );
    let id = String::from_utf8_lossy(&rel.stdout).trim().to_owned();
    let shown = relative(&["show", "me"]);
    assert!(shown.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    let cwd = Path::new(shown["cwd"].as_str().unwrap());
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
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("v");
    assert!(cli.tmux(&["kill-session", "-t", "=v"]).status.success());
    assert_eq!(cli.ok(&["refresh"]).trim(), format!("{id}\tv\texited"));
    assert_eq!(cli.ok(&["refresh"]), "");
}
