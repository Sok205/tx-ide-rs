//! The views plugin through the binary: the picker feed and the session verbs.

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
    fn sock(&self) -> PathBuf {
        self.state().join("tmux.sock")
    }
    fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-S")
            .arg(self.sock())
            .args(args)
            .env_remove("TMUX")
            .output()
            .unwrap()
    }
    fn tmux_out(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.tmux(args).stdout)
            .trim()
            .to_owned()
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

fn lifecycle_of(cli: &Cli, token: &str) -> String {
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["show", token])).unwrap();
    json["lifecycle"].as_str().unwrap().to_owned()
}

#[test]
fn kill_stops_and_reports() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert_eq!(cli.ok(&["kill", "a"]), "Killed 'a'\n");
    assert_eq!(lifecycle_of(&cli, "a"), "stopped");
}

#[test]
fn kill_unknown_exits_1() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    let output = cli.run(&["kill", "nope"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "session 'nope' not found\n"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn kill_view_home_without_record() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "Views"])
            .status
            .success()
    );
    assert!(
        cli.tmux(&["set-option", "-t", "=Views:", "@tx-view", "1"])
            .status
            .success()
    );
    assert_eq!(cli.ok(&["kill", "Views"]), "Killed 'Views'\n");
    assert!(!cli.tmux(&["has-session", "-t", "=Views"]).status.success());
}

#[test]
fn kill_does_not_touch_plain_tmux_sessions() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "plain"])
            .status
            .success()
    );
    assert_eq!(cli.run(&["kill", "plain"], &[], "").status.code(), Some(1));
    assert!(cli.tmux(&["has-session", "-t", "=plain"]).status.success());
}

#[test]
fn pane_info_two_lines() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    cli.ok(&["tag", "a", "x", "y"]);
    assert_eq!(cli.ok(&["_pane-info", &id]), "a\nx,y\n");
}

#[test]
fn pane_info_unknown_is_two_empty_lines() {
    require_tmux!();
    let cli = Cli::new();
    assert_eq!(cli.ok(&["_pane-info", "nope"]), "\n\n");
}

#[test]
fn tmux_name_prints_dollar_target_for_live_only() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    cli.create("b");
    let target = cli.ok(&["_tmux-name", "a"]);
    assert!(
        target.starts_with('$') && target.ends_with('\n') && target.trim().len() > 1,
        "{target:?}"
    );
    assert_eq!(
        target.trim(),
        cli.tmux_out(&["display-message", "-p", "-t", "=a:", "#{session_id}"])
    );
    cli.ok(&["stop", "b"]);
    assert_eq!(cli.ok(&["_tmux-name", "b"]), "");
    assert_eq!(cli.ok(&["_tmux-name", "nope"]), "");
}

#[test]
fn relabel_sets_tx_name_with_chips() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("n");
    cli.create("plain");
    cli.ok(&["tag", "n", "a", "b"]);
    assert_eq!(cli.ok(&["_relabel"]), "");
    assert_eq!(
        cli.tmux_out(&["show-options", "-vqt", "=n:", "@tx-name"]),
        "n [a,b]"
    );
    assert_eq!(
        cli.tmux_out(&["show-options", "-vqt", "=plain:", "@tx-name"]),
        "plain"
    );
}

#[test]
fn whoami_outside_server_exits_1() {
    require_tmux!();
    let cli = Cli::new();
    let output = cli.run(&["whoami"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "txc whoami: not inside the tx tmux server\n"
    );
}

fn pane_id_of(cli: &Cli, name: &str) -> String {
    cli.tmux_out(&[
        "display-message",
        "-p",
        "-t",
        &format!("={name}:"),
        "#{pane_id}",
    ])
}

#[test]
fn pane_label_dash_when_nothing_nested() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    let pane = pane_id_of(&cli, "a");
    assert_eq!(
        cli.ok(&["_pane-label", &pane, "0"]),
        "#[fg=colour240,nobold]\u{2014}#[default]"
    );
}

#[test]
fn pane_label_shows_nested_session() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("inner");
    cli.ok(&["tag", "inner", "t1"]);
    let shell = if Command::new("zsh").arg("-c").arg(":").output().is_ok() {
        "zsh"
    } else {
        "sh"
    };
    let cwd = cli.dir.path().join("outer");
    std::fs::create_dir_all(&cwd).unwrap();
    cli.ok(&[
        "create",
        "outer",
        "--cwd",
        cwd.to_str().unwrap(),
        "--exec",
        shell,
    ]);
    let pane = pane_id_of(&cli, "outer");
    let attach = format!(
        "TMUX= tmux -S {} attach -t '=inner'",
        cli.sock().to_str().unwrap()
    );
    assert!(
        cli.tmux(&["send-keys", "-t", "=outer:", &attach, "Enter"])
            .status
            .success()
    );
    let mut attached = false;
    for _ in 0..30 {
        if !cli.tmux_out(&["list-clients"]).is_empty() {
            attached = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(attached, "nested client never attached");
    let label = cli.ok(&["_pane-label", &pane]);
    assert!(label.contains("#[bold]inner#[default]"), "{label:?}");
    assert!(label.contains("[t1]"), "{label:?}");
    assert!(!label.ends_with('\n'), "{label:?}");
    cli.ok(&["stop", "outer"]);
}

#[test]
fn edit_session_on_view_home_exits_1() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "Views", "sleep", "60"])
            .status
            .success()
    );
    assert!(
        cli.tmux(&["set-option", "-t", "=Views:", "@tx-view", "1"])
            .status
            .success()
    );
    let pane = cli.tmux_out(&["display-message", "-p", "-t", "=Views:", "#{pane_id}"]);
    let output = cli.run(&["_edit-session", &pane, "nonexistent-client"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("prefix+e: no tx session in this pane (view homes cannot be edited)"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
