//! The chat plugin's capture through the binary: `_chat-hook`, `chat ls`, `chat hooks`.

mod common;

use std::path::Path;

use common::Cli;
use serde_json::{Value, json};

impl Cli {
    fn show(&self, name: &str) -> Value {
        serde_json::from_str(&self.ok(&["show", name])).unwrap()
    }
    fn record_path(&self, id: &str) -> std::path::PathBuf {
        self.state().join("sessions").join(format!("{id}.json"))
    }
    fn create_agent(&self, name: &str) -> String {
        let cwd = self.dir.path().join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        let path = self.fake_claude_path();
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
    fn hook(&self, event: &str, id: &str, payload: &str) -> std::process::Output {
        self.run(&["_chat-hook", event], &[("TX_SESSION_ID", id)], payload)
    }
}

type Case<'a> = (Vec<(&'a str, &'a str)>, &'a str, &'a str);

fn payload(chat: &str, transcript: &Path) -> String {
    json!({"session_id": chat, "transcript_path": transcript}).to_string()
}

#[test]
fn chat_hook_captures_the_primary_chat() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create_agent("a");
    let transcript = cli.dir.path().join("c1.jsonl");
    let body = payload("c1", &transcript);
    assert!(cli.hook("session-start", &id, &body).status.success());
    let shown = cli.show("a");
    let chat = &shown["ext"]["chat"];
    assert_eq!(chat["engine"], "claude");
    let first = &chat["chats"][0];
    assert_eq!(first["id"], "c1");
    assert_eq!(first["role"], "primary");
    assert_eq!(first["how"], "spawn");
    assert_eq!(first["transcript"], transcript.to_str().unwrap());
    assert_eq!(chat["chats"].as_array().unwrap().len(), 1);

    let before = std::fs::read(cli.record_path(&id)).unwrap();
    assert!(cli.hook("session-start", &id, &body).status.success());
    assert!(cli.hook("stop", &id, &body).status.success());
    assert_eq!(std::fs::read(cli.record_path(&id)).unwrap(), before);
    assert!(
        cli.ok(&["chat", "ls", "a"])
            .starts_with("a — 1 chat(s)\n  c1 ")
    );
}

#[test]
fn a_chat_owned_by_another_session_is_never_bound() {
    require_tmux!();
    let cli = Cli::new();
    let a = cli.create_agent("a");
    let b = cli.create_agent("b");
    let body = payload("c1", &cli.dir.path().join("c1.jsonl"));
    assert!(cli.hook("session-start", &a, &body).status.success());
    assert!(cli.hook("prompt-submit", &b, &body).status.success());
    let ingest = cli.run(&["_history-ingest"], &[("TX_SESSION_ID", &b)], &body);
    assert!(ingest.status.success());
    std::thread::sleep(std::time::Duration::from_millis(500));
    let shown = cli.show("b");
    assert!(shown["ext"].get("chat").is_none(), "{shown}");
    assert!(shown["ext"].get("history").is_none(), "{shown}");
    assert!(
        !cli.state()
            .join("plugins/history")
            .join(&b)
            .join("c1")
            .exists()
    );
}

#[test]
fn chat_hook_not_ours_or_bad_payload_writes_nothing() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create_agent("a");
    let good = payload("c1", &cli.dir.path().join("c1.jsonl"));
    let record = || std::fs::read(cli.record_path(&id)).unwrap();
    let before = record();
    let other = "0f6b3b8a-1111-4222-8333-444444444444";
    let cases: Vec<Case> = vec![
        (vec![], "session-start", good.as_str()),
        (
            vec![("TX_SESSION_ID", other)],
            "session-start",
            good.as_str(),
        ),
        (vec![("TX_SESSION_ID", "not-an-id")], "stop", good.as_str()),
        (vec![("TX_SESSION_ID", id.as_str())], "stop", ""),
        (vec![("TX_SESSION_ID", id.as_str())], "stop", "[1]"),
        (
            vec![("TX_SESSION_ID", id.as_str())],
            "stop",
            r#"{"session_id":3}"#,
        ),
        (
            vec![("TX_SESSION_ID", id.as_str())],
            "stop",
            r#"{"session_id":"c1"}"#,
        ),
        (vec![("TX_SESSION_ID", id.as_str())], "bogus", good.as_str()),
    ];
    for (env, event, stdin) in cases {
        let output = cli.run(&["_chat-hook", event], &env, stdin);
        assert!(output.status.success(), "{env:?} {event} {stdin}");
        assert_eq!(record(), before, "{env:?} {event} {stdin}");
    }
}

#[test]
fn malformed_ext_chat_is_reported_and_never_breaks_listing() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create_agent("a");
    let path = cli.record_path(&id);
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["ext"] = json!({"chat": 5});
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

    let error = cli.fails(&["chat", "ls", "a"], 1);
    assert!(error.contains("malformed"), "{error}");
    assert!(error.contains("ext.chat of 'a' is malformed"), "{error}");
    cli.ok(&["ls"]);
    cli.ok(&["show", "a"]);
    cli.ok(&["_list"]);
    let body = payload("c1", &cli.dir.path().join("c1.jsonl"));
    assert!(cli.hook("session-start", &id, &body).status.success());
    assert_eq!(cli.show("a")["ext"]["chat"], 5);
}

#[test]
fn chat_ls_unknown_session_exits_1() {
    require_tmux!();
    let cli = Cli::new();
    assert!(
        cli.fails(&["chat", "ls", "nope"], 1)
            .contains("chat ls: session 'nope' not found")
    );
    cli.fails(&["chat", "ls"], 2);
    cli.fails(&["chat", "bogus"], 2);
}

#[test]
fn chat_hooks_install_coexists_with_hook_install() {
    require_tmux!();
    let cli = Cli::new();
    let home = cli.dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let env = [("HOME", home.to_str().unwrap())];
    let run = |args: &[&str]| {
        let output = cli.run(args, &env, "");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let settings = home.join(".claude/settings.json");
    let stop_commands = || -> Vec<String> {
        let json: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        json["hooks"]["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|b| b["hooks"].as_array().unwrap().clone())
            .map(|h| h["command"].as_str().unwrap().to_owned())
            .collect()
    };
    run(&["hook", "install"]);
    let with_hooks = std::fs::read(&settings).unwrap();
    run(&["chat", "hooks", "install", "--dry-run"]);
    assert_eq!(std::fs::read(&settings).unwrap(), with_hooks, "dry run");
    run(&["chat", "hooks", "install"]);
    run(&["history-hooks", "install"]);
    let commands = stop_commands();
    for needle in [" hook stop", " _chat-hook stop", " _history-ingest"] {
        assert!(
            commands.iter().any(|c| c.contains(needle)),
            "{needle}: {commands:?}"
        );
    }
    run(&["chat", "hooks", "uninstall"]);
    let commands = stop_commands();
    assert!(
        !commands.iter().any(|c| c.contains("_chat-hook")),
        "{commands:?}"
    );
    assert!(commands.iter().any(|c| c.contains("_history-ingest")));
    assert!(commands.iter().any(|c| c.contains(" hook stop")));
}
