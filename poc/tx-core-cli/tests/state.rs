//! Agent state end to end through the binaries: the Claude adapter wires `txi hook <Event>`
//! into the command line it builds, the hooks feed `TxCore::hook`, and `show` reads `status`.

mod common;

use std::process::{Command, Output, Stdio};

use common::Cli;
use serde_json::Value;

impl Cli {
    fn show(&self, name: &str) -> Value {
        serde_json::from_str(&self.ok(&["show", name])).unwrap()
    }
    fn state_of(&self, name: &str) -> Value {
        self.show(name)["status"]["state"].clone()
    }
    fn txi_hook(&self, event: &str, id: &str, stdin: &str) -> Output {
        self.txi(&["hook", event], &[("TX_SESSION_ID", id)], stdin)
    }
    /// A Claude session whose `claude` writes its argv, NUL-separated, to `argv`, then stays
    /// alive.
    fn create_claude(&self, name: &str) -> String {
        let cwd = self.dir.path().join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        let argv = self.dir.path().join("argv");
        let path = self.shim_path(&[(
            "claude",
            &format!(
                "printf '%s\\0' \"$@\" > '{}'\nexec sleep 60",
                argv.display()
            ),
        )]);
        let output = self.run(
            &[
                "create",
                name,
                "--cwd",
                cwd.to_str().unwrap(),
                "--engine",
                "claude",
            ],
            &[("PATH", path.as_str())],
            "",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
    fn claude_argv(&self) -> Vec<String> {
        let path = self.dir.path().join("argv");
        assert!(self.wait_for(10, || path.exists()), "claude never started");
        let mut text = String::new();
        self.wait_for(5, || {
            text = std::fs::read_to_string(&path).unwrap_or_default();
            text.ends_with('\0')
        });
        text.trim_end_matches('\0')
            .split('\0')
            .map(str::to_owned)
            .collect()
    }
    fn state_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.state().join("log.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|line| line["type"] == "state-changed")
            .map(|line| line["msg"].as_str().unwrap().to_owned())
            .collect()
    }
}

#[test]
fn the_wired_hooks_move_a_claude_session() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create_claude("a");
    assert_eq!(cli.state_of("a"), "starting");

    // The adapter wired every hook to this very txc, through `--settings`.
    let argv = cli.claude_argv();
    let at = argv.iter().position(|arg| arg == "--settings").unwrap();
    let settings: Value = serde_json::from_str(&argv[at + 1]).unwrap();
    let command = settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        command,
        format!(
            "'{}' 'hook' 'SessionStart' || exit 1",
            env!("CARGO_BIN_EXE_txi")
        )
    );

    // Run that exact command the way Claude does: through a shell, payload on stdin, inside
    // the session's environment.
    let mut child = Command::new("sh")
        .args(["-c", &command])
        .env("TX_SESSION_ID", &id)
        .env("TX_CORE_HOME", cli.state())
        .env_remove("TMUX")
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"source":"startup"}"#)
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(cli.state_of("a"), "idle");

    assert!(cli.txi_hook("UserPromptSubmit", &id, "{}").status.success());
    assert_eq!(cli.state_of("a"), "working");
    assert!(
        cli.txi_hook(
            "Notification",
            &id,
            r#"{"notification_type":"permission_prompt","message":"needs Bash"}"#
        )
        .status
        .success()
    );
    let status = cli.show("a")["status"].clone();
    assert_eq!(status["state"], "idle");
    assert_eq!(status["note"], "needs Bash");
    assert!(cli.txi_hook("PostToolUse", &id, "{}").status.success());
    assert_eq!(cli.state_of("a"), "working");
    assert!(cli.txi_hook("Stop", &id, "not json").status.success());
    let status = cli.show("a")["status"].clone();
    assert_eq!(status["state"], "idle");
    assert!(status.get("note").is_none(), "{status}");

    assert_eq!(
        cli.state_log(),
        [
            "starting → idle",
            "idle → working",
            "working → idle: needs Bash",
            "idle → working",
            "working → idle"
        ]
    );

    cli.ok(&["stop", "a"]);
    assert_eq!(cli.state_of("a"), "ended");
    assert!(cli.txi_hook("UserPromptSubmit", &id, "{}").status.success());
    assert_eq!(cli.state_of("a"), "ended", "a hook after the end is stale");
}

#[test]
fn not_ours_exits_zero_and_writes_nothing() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create_claude("a");
    let before = cli.show("a");
    let output = cli.txi(&["hook", "Stop"], &[], "");
    assert!(output.status.success(), "no TX_SESSION_ID");
    let output = cli.txi_hook("Stop", "0f6b3b8a-1111-4222-8333-444444444444", "");
    assert!(output.status.success(), "unknown session");
    let output = cli.txi_hook("Stop", "not an id", "");
    assert!(output.status.success(), "malformed id");
    let output = cli.txi_hook("NoSuchEvent", &id, "[1, 2]");
    assert!(output.status.success(), "unknown event, odd payload");
    let output = cli.txi(&["hook", "Stop", "extra"], &[("TX_SESSION_ID", "nope")], "");
    assert!(output.status.success(), "extra arguments");
    assert_eq!(cli.show("a"), before);
}

#[test]
fn process_sessions_have_no_status() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("p");
    assert!(cli.show("p").get("status").is_none());
    assert!(cli.txi_hook("SessionStart", &id, "{}").status.success());
    assert!(cli.show("p").get("status").is_none());
}

#[test]
fn hooks_are_internal_to_txi() {
    let cli = Cli::new();
    let help = cli.ok(&["help"]);
    assert!(
        !help.contains("hook EVENT") && !help.contains("txi"),
        "{help}"
    );
    // `chat …` is the chat plugin's again; it has no `hook` subcommand.
    cli.fails(&["chat", "hook", "Stop"], 2);
    let usage = cli.txi(&[], &[], "");
    assert_eq!(usage.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&usage.stderr).contains("usage: txi hook <Event>"));
    assert_eq!(cli.txi(&["bogus"], &[], "").status.code(), Some(2));
}
