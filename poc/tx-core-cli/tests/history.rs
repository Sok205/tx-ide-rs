//! The history plugin through the binary: Stop hooks mirror the agent's transcript, `history`
//! lists, `history-hooks` installs.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::Cli;

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../plugins/chat/tests/fixtures/transcript.jsonl"
);

impl Cli {
    fn show(&self, name: &str) -> serde_json::Value {
        serde_json::from_str(&self.ok(&["show", name])).unwrap()
    }
    /// A Claude session whose `claude` just stays alive.
    fn create_claude(&self, name: &str) -> String {
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
                "--prompt",
                "start",
            ],
            &[("PATH", path.as_str())],
            "",
        );
        assert!(output.status.success(), "{output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
    fn history_dir(&self) -> PathBuf {
        self.state().join("plugins/history")
    }
    /// What the agent's two Stop hooks run: `txi hook Stop` and `txc _history-ingest`.
    fn stop_hooks(&self, id: &str, payload: &str) {
        let env = [("TX_SESSION_ID", id)];
        assert!(self.txi(&["hook", "Stop"], &env, payload).status.success());
        let output = self.run(&["_history-ingest"], &env, payload);
        assert!(output.status.success(), "_history-ingest");
    }
}

fn payload(chat: &str, transcript: &Path) -> String {
    serde_json::json!({"session_id": chat, "transcript_path": transcript}).to_string()
}

fn source(cli: &Cli, chat: &str) -> PathBuf {
    let path = cli
        .dir
        .path()
        .join(format!("claude/projects/-x/{chat}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

#[test]
fn stop_hook_mirrors_the_claude_transcript_byte_for_byte() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create_claude("a");
    let src = source(&cli, "c1");
    std::fs::copy(FIXTURE, &src).unwrap();
    cli.stop_hooks(&id, &payload("c1", &src));

    let bundle = cli.history_dir().join(&id).join("c1");
    let want = std::fs::read(FIXTURE).unwrap();
    assert!(
        cli.wait_for(10, || std::fs::read(bundle.join("transcript.jsonl"))
            .is_ok_and(|got| got == want)),
        "mirror"
    );
    assert!(cli.wait_for(10, || {
        cli.show("a")["ext"]["history"]["chats"]["c1"]["bundle"] == bundle.to_str().unwrap()
    }));
    let shown = cli.show("a");
    assert_eq!(shown["state"], "idle");
    assert_eq!(
        shown["ext"]["history"]["chats"]["c1"]["transcript"],
        src.to_str().unwrap()
    );
}

#[test]
fn concurrent_ingests_leave_one_complete_copy() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    let src = source(&cli, "c1");
    std::fs::copy(FIXTURE, &src).unwrap();
    cli.stop_hooks(&id, &payload("c1", &src));

    let cli = Arc::new(cli);
    let append = Arc::new(Mutex::new(()));
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let (cli, append, src, id) = (cli.clone(), append.clone(), src.clone(), id.clone());
            std::thread::spawn(move || {
                for round in 0..3 {
                    {
                        let _guard = append.lock().unwrap();
                        let mut file = std::fs::OpenOptions::new().append(true).open(&src).unwrap();
                        std::io::Write::write_all(
                            &mut file,
                            format!("{{\"n\":{n},\"round\":{round}}}\n").as_bytes(),
                        )
                        .unwrap();
                    }
                    let output = cli.run(&["_history-ingest", "--wait", &id], &[], "");
                    assert!(output.status.success());
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert!(
        cli.run(&["_history-ingest", "--wait", &id], &[], "")
            .status
            .success()
    );
    let copy = cli.history_dir().join(&id).join("c1/transcript.jsonl");
    assert_eq!(
        std::fs::read(copy).unwrap(),
        std::fs::read(&src).unwrap(),
        "one complete, byte-identical copy"
    );
}

#[test]
fn history_lists_stopped_sessions_newest_first() {
    require_tmux!();
    let cli = Cli::new();
    assert_eq!(
        cli.ok(&["history"]),
        "HISTORY\n  (no exited or stopped sessions)\n"
    );
    cli.create("first");
    cli.create("second");
    cli.create("alive");
    cli.ok(&["stop", "first"]);
    cli.ok(&["stop", "second"]);
    let text = cli.ok(&["history"]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "HISTORY");
    assert_eq!(lines.len(), 3, "{text}");
    assert!(lines[1].starts_with("  second "), "{text}");
    assert!(lines[2].starts_with("  first "), "{text}");
    assert!(lines[1].contains(" stopped "), "{text}");
    assert!(lines[1].contains(" 0c "), "{text}");
    assert!(!text.contains("alive"));
    assert_eq!(
        cli.ok(&["history", "--tag", "nope"]),
        "HISTORY\n  (no exited or stopped sessions)\n"
    );
    assert!(cli.ok(&["history", "--cwd", "second"]).contains("second"));
    assert!(
        cli.fails(&["history", "--since", "yesterday"], 2)
            .contains("history: --since expects YYYY-MM-DD, got 'yesterday'")
    );
}

#[test]
fn history_session_lists_bundles() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    assert_eq!(cli.ok(&["history", "a"]), "(no mirrored chats)\n");
    let src = source(&cli, "c1");
    std::fs::copy(FIXTURE, &src).unwrap();
    cli.stop_hooks(&id, &payload("c1", &src));
    assert!(
        cli.run(&["_history-ingest", "--wait", &id], &[], "")
            .status
            .success()
    );
    let bundle = cli.history_dir().join(&id).join("c1");
    let bytes = std::fs::metadata(FIXTURE).unwrap().len();
    assert_eq!(
        cli.ok(&["history", "a"]),
        format!("c1  {bytes}  {}\n", bundle.display())
    );
    assert!(
        cli.fails(&["history", "nope"], 1)
            .contains("history: session 'nope' not found")
    );
}

#[test]
fn history_ingest_not_ours_exits_zero() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    let src = source(&cli, "c1");
    std::fs::copy(FIXTURE, &src).unwrap();
    let good = payload("c1", &src);
    let before = cli.show("a");
    for (env, stdin) in [
        (vec![], good.as_str()),
        (
            vec![("TX_SESSION_ID", "0f6b3b8a-1111-4222-8333-444444444444")],
            good.as_str(),
        ),
        (vec![("TX_SESSION_ID", "not-an-id")], good.as_str()),
        (vec![("TX_SESSION_ID", id.as_str())], "[1]"),
        (vec![("TX_SESSION_ID", id.as_str())], "garbage"),
        (
            vec![("TX_SESSION_ID", id.as_str())],
            r#"{"session_id":"c1"}"#,
        ),
    ] {
        let output = cli.run(&["_history-ingest"], &env, stdin);
        assert!(output.status.success(), "{env:?} {stdin}");
    }
    let unknown = cli.run(&["_history-ingest", "nope"], &[], "");
    assert!(unknown.status.success());
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(!cli.state().join("plugins/history").exists());
    assert_eq!(cli.show("a"), before);
}

#[test]
fn history_hooks_install_round_trip() {
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
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{"hooks":{"Stop":[{"matcher":"","hooks":[{"type":"command","command":"say done"}]}]}}"#,
    )
    .unwrap();
    let with_hooks = std::fs::read(&settings).unwrap();

    run(&["history-hooks", "install", "--dry-run"]);
    assert_eq!(std::fs::read(&settings).unwrap(), with_hooks, "dry run");
    run(&["history-hooks", "install"]);
    let both = std::fs::read_to_string(&settings).unwrap();
    assert!(both.contains("_history-ingest"));
    assert!(both.contains("say done"), "foreign entries survive");
    run(&["history-hooks", "install"]);
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        both,
        "idempotent"
    );

    run(&["history-hooks", "uninstall"]);
    assert_eq!(std::fs::read(&settings).unwrap(), with_hooks);
}
