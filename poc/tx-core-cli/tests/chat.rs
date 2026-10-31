//! The chat plugin through the binary: `chat ls` (the engine's conversation), `doctor
//! --fix-hooks` (the retired global hooks) and prefix+f (`_fork-pane`) over core's built-in
//! `fork`.

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
        let cwd = self.repository(name);
        let path = self.argv_path();
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
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
    /// A `claude` that writes its argv one per line to `argv/<pid>`, then stays alive.
    fn argv_path(&self) -> String {
        let dir = self.dir.path().join("argv");
        std::fs::create_dir_all(&dir).unwrap();
        self.shim_path(&[(
            "claude",
            &format!(
                "printf '%s\\n' \"$@\" > '{}/'$$\nexec sleep 60",
                dir.display()
            ),
        )])
    }
    /// Every recorded `claude` argv.
    fn argvs(&self) -> Vec<Vec<String>> {
        let Ok(entries) = std::fs::read_dir(self.dir.path().join("argv")) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|e| {
                std::fs::read_to_string(e.path())
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_owned)
                    .collect()
            })
            .collect()
    }
    fn wait_argv(&self, mut probe: impl FnMut(&[String]) -> bool) -> Vec<String> {
        let mut found = None;
        let ok = self.wait_for(10, || {
            found = self.argvs().into_iter().find(|argv| probe(argv));
            found.is_some()
        });
        assert!(ok, "no matching argv in {:?}", self.argvs());
        found.unwrap()
    }
    /// `txc` with the recording shim on PATH and the engines' homes inside the test dir.
    fn op(&self, args: &[&str], extra: &[(&str, &str)]) -> std::process::Output {
        let path = self.argv_path();
        let home = self.dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let home = home.to_str().unwrap().to_owned();
        let mut env = vec![
            ("PATH", path.as_str()),
            ("HOME", home.as_str()),
            ("CLAUDE_CONFIG_DIR", home.as_str()),
            ("CODEX_HOME", home.as_str()),
            ("TMUX_PANE", ""),
        ];
        env.extend_from_slice(extra);
        self.run(args, &env, "")
    }
    fn op_ok(&self, args: &[&str], extra: &[(&str, &str)]) -> String {
        let output = self.op(args, extra);
        assert!(
            output.status.success(),
            "txc {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    fn records_named(&self, name: &str) -> Vec<Value> {
        std::fs::read_dir(self.state().join("sessions"))
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|e| serde_json::from_slice::<Value>(&std::fs::read(e.path()).ok()?).ok())
            .filter(|record| record["name"] == name)
            .collect()
    }
}

#[test]
fn chat_ls_prints_the_conversation_the_engine_recorded() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create_agent("a");
    let transcript = cli.show("a")["ext"]["claude"]["transcript_path"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        transcript.ends_with(&format!("/{id}.jsonl")),
        "{transcript}"
    );
    assert_eq!(
        cli.ok(&["chat", "ls", "a"]),
        format!("a — conversation {id}\n  transcript {transcript}\n")
    );
    let path = cli.record_path(&id);
    let mut record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record["ext"] = json!({"claude": {"model": "sonnet"}});
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert_eq!(cli.ok(&["chat", "ls", "a"]), "a — no conversation yet\n");
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
    cli.fails(&["chat", "hooks", "install"], 2);
}

#[test]
fn help_no_longer_lists_the_hook_installers_or_the_mirror() {
    let help = Cli::new().ok(&["help"]);
    for gone in ["chat hooks", "history"] {
        assert!(!help.contains(gone), "{gone}: {help}");
    }
    assert!(help.contains("doctor --fix-hooks"), "{help}");
}

#[test]
fn doctor_fix_hooks_removes_only_the_retired_entries_and_is_idempotent() {
    let cli = Cli::new();
    let home = cli.dir.path().join("home");
    let claude = home.join(".claude");
    std::fs::create_dir_all(&claude).unwrap();
    let env = [("HOME", home.to_str().unwrap())];
    let block =
        |command: &str| json!({"matcher": "", "hooks": [{"type": "command", "command": command}]});
    let ours = "'/opt/txc' _capture stop || exit 1";
    let settings = claude.join("settings.json");
    std::fs::write(
        &settings,
        json!({"hooks": {"Stop": [block("say done"), block(ours)]}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        claude.join(".tx-chat-managed.json"),
        json!({"version": 1, "backup": null, "added": [{"event": "Stop", "command": ours}]})
            .to_string(),
    )
    .unwrap();
    let first = cli.run(&["doctor", "--fix-hooks"], &env, "");
    assert!(first.status.success(), "{first:?}");
    let read = || -> Value { serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap() };
    assert_eq!(read(), json!({"hooks": {"Stop": [block("say done")]}}));
    assert!(!claude.join(".tx-chat-managed.json").exists());
    let second = cli.run(&["doctor", "--fix-hooks"], &env, "");
    assert!(second.status.success(), "{second:?}");
    assert!(String::from_utf8_lossy(&second.stdout).contains("nothing to do"));
    assert_eq!(read(), json!({"hooks": {"Stop": [block("say done")]}}));
    assert_eq!(cli.run(&["doctor"], &env, "").status.code(), Some(2));
}

fn has_run(argv: &[String], run: &[&str]) -> bool {
    argv.windows(run.len())
        .any(|w| w.iter().zip(run).all(|(a, b)| a == b))
}

#[test]
fn fork_launches_the_source_conversation_forked_in_a_new_worktree() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create_agent("a");
    let source = cli.show("a");
    let fork_id = cli.op_ok(&["fork", "a", "--prompt", "go on"], &[]);
    let fork_id = fork_id.trim();
    let fork = cli.show("a-fork");
    assert_eq!(fork["id"], fork_id);
    assert_eq!(fork["parent"], id.as_str());
    assert_eq!(fork["launch"]["kind"], "agent");
    assert_eq!(fork["launch"]["engine"], "claude");
    assert_eq!(fork["state"], "starting");
    let cwd = Path::new(fork["cwd"].as_str().unwrap());
    assert_ne!(cwd, Path::new(source["cwd"].as_str().unwrap()));
    assert!(cwd.starts_with(cli.worktrees_dir().canonicalize().unwrap()));
    assert!(cwd.join(".git").exists(), "{}", cwd.display());
    assert_eq!(fork["ext"]["claude"]["conversation_id"], fork_id);
    let argv = cli.wait_argv(|argv| {
        has_run(
            argv,
            &["--resume", &id, "--fork-session", "--session-id", fork_id],
        )
    });
    assert_eq!(argv.last().map(String::as_str), Some("go on"));
    let second = cli.op_ok(&["fork", "a", "--prompt", "again"], &[]);
    assert_eq!(cli.show("a-fork-2")["id"], second.trim());
    let types: Vec<String> = std::fs::read_to_string(cli.state().join("log.jsonl"))
        .unwrap()
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(types, ["created", "forked", "forked"]);
}

#[test]
fn fork_refusals() {
    require_tmux!();
    let cli = Cli::new();
    let missing = cli.op(&["fork", "nope", "--prompt", "go"], &[]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("no session \"nope\""));
    assert_eq!(cli.op(&["fork"], &[]).status.code(), Some(2));
    assert_eq!(cli.op(&["fork", "p"], &[]).status.code(), Some(2));
}

#[test]
fn fork_pane_background_half_forks_the_pane_session_and_reports() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create_agent("a");
    let pane = common::pane_id_of(&cli, "a").trim().to_owned();
    let output = cli.op(&["_fork-pane", &pane, "nosuchclient", "a-f"], &[]);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("forked a → a-f"),
        "{output:?}"
    );
    let fork = cli.show("a-f");
    assert_eq!(fork["parent"], id.as_str());
}

#[test]
fn fork_pane_on_an_untracked_session_reports_and_exits_1() {
    require_tmux!();
    let cli = Cli::new();
    cli.create_agent("a");
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "plain"])
            .status
            .success()
    );
    let pane = cli
        .tmux_out(&["display-message", "-p", "-t", "=plain:", "#{pane_id}"])
        .trim()
        .to_owned();
    let output = cli.op(&["_fork-pane", &pane, "nosuchclient", "x"], &[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("prefix+f: no tx session in this pane"),
        "{output:?}"
    );
    assert!(cli.records_named("x").is_empty());
}

#[test]
fn fork_pane_usage_exits_2() {
    let cli = Cli::new();
    for args in [
        &["_fork-pane"][..],
        &["_fork-pane", "%0"],
        &["_fork-pane", "%0", "c", "n", "extra"],
    ] {
        let output = cli.run(args, &[], "");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "usage: txc _fork-pane PANE CLIENT [NAME]\n"
        );
    }
}

#[test]
fn ui_install_binds_prefix_f() {
    require_tmux!();
    use std::os::unix::fs::PermissionsExt;
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let state = cli.state();
    let script = state.join("bin/tmux-fork-tx-session");
    assert_eq!(
        std::fs::metadata(&script).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let fragment = std::fs::read_to_string(state.join("keys/chat.tmux")).unwrap();
    assert_eq!(fragment.lines().nth(1), Some("# keys: f"));
    let conf = state.join("tmux.conf");
    let server = |args: &[&str]| {
        std::process::Command::new("tmux")
            .arg("-S")
            .arg(cli.sock())
            .arg("-f")
            .arg(&conf)
            .args(args)
            .env_remove("TMUX")
            .output()
            .unwrap()
    };
    assert!(
        server(&["new-session", "-d", "-s", "probe", "sleep", "60"])
            .status
            .success()
    );
    assert!(
        cli.wait_for(5, || {
            cli.tmux_out(&["list-keys", "-T", "prefix", "f"])
                .contains("tmux-fork-tx-session")
        }),
        "prefix+f never bound"
    );
    cli.write_yaml("plugins:\n  chat: { enabled: false }\n");
    cli.ok(&["ui", "install"]);
    assert!(!state.join("keys/chat.tmux").exists());
    assert!(!state.join("bin/tmux-fork-tx-session").exists());
    assert!(
        !cli.tmux_out(&["list-keys", "-T", "prefix", "f"])
            .contains("tmux-fork-tx-session")
    );
}
