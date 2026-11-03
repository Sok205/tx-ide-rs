//! The assistant plugin through the binary: `assistant warm`, and the plugin switch.

mod common;

use common::{Cli, cwd_of, state_of};

#[test]
fn warm_creates_once_and_reuses() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let path = cli.fake_claude_path();
    let env = [("PATH", path.as_str())];
    let first = cli.run(&["assistant", "warm"], &env, "");
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&first.stdout), "tx-assistant\n");
    assert_ne!(state_of(&cli, "tx-assistant"), "ended");
    let id = cli.ok(&["ls", "--tag", "tx-system"]);
    assert_eq!(id.lines().count(), 1, "{id:?}");
    let second = cli.run(&["assistant", "warm"], &env, "");
    assert_eq!(second.status.code(), Some(0));
    assert_eq!(cli.ok(&["ls", "--tag", "tx-system"]), id);
    let cwd = cwd_of(&cli, "tx-assistant");
    assert!(cwd.starts_with(cli.worktrees_dir()), "{cwd:?}");
    assert!(cwd.ends_with("home--tx-assistant"), "{cwd:?}");
    assert!(
        cwd.join(".claude/skills/tx-sessions").exists(),
        "skills link in the assistant's worktree"
    );
    let home = cli.state().join("plugins/assistant/home");
    assert!(home.join(".git").is_dir(), "the assistant's own repository");
    assert!(!home.join(".claude").exists());
}

#[test]
fn warm_respawns_a_vanished_assistant_in_place() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let path = cli.fake_claude_path();
    let env = [("PATH", path.as_str())];
    assert_eq!(
        cli.run(&["assistant", "warm"], &env, "").status.code(),
        Some(0)
    );
    let first = cli.ok(&["ls", "--tag", "tx-system"]);
    let first_cwd = cwd_of(&cli, "tx-assistant");
    std::fs::write(first_cwd.join("note.txt"), "keep me\n").unwrap();
    assert!(
        cli.tmux(&["kill-session", "-t", "=tx-assistant"])
            .status
            .success()
    );
    let respawned = cli.run(&["assistant", "warm"], &env, "");
    assert_eq!(
        respawned.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&respawned.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&respawned.stdout), "tx-assistant\n");
    assert_eq!(cli.ok(&["ls", "--tag", "tx-system"]), first);
    assert_ne!(state_of(&cli, "tx-assistant"), "ended");
    assert_eq!(cwd_of(&cli, "tx-assistant"), first_cwd);
    assert_eq!(
        std::fs::read_to_string(first_cwd.join("note.txt")).unwrap(),
        "keep me\n"
    );
}

#[test]
fn two_concurrent_warms_make_one_assistant() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let path = cli.fake_claude_path();
    let env = [("PATH", path.as_str())];
    let barrier = std::sync::Barrier::new(2);
    let outputs: Vec<std::process::Output> = std::thread::scope(|scope| {
        let warms: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    let mut command = cli.command(&["assistant", "warm"], &env);
                    barrier.wait();
                    command.output().unwrap()
                })
            })
            .collect();
        warms.into_iter().map(|warm| warm.join().unwrap()).collect()
    });
    for output in &outputs {
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "tx-assistant\n");
    }
    let listed = cli.ok(&["ls", "--tag", "tx-system"]);
    assert_eq!(listed.lines().count(), 1, "{listed:?}");
}

#[test]
fn a_failed_warm_leaves_its_reason_in_the_plugin_dir() {
    require_tmux!();
    let cli = Cli::new();
    let path = cli.fake_claude_path();
    let env = [("PATH", path.as_str())];
    let failure = cli.state().join("plugins/assistant/warm.failed");
    let output = cli.run(&["assistant", "warm"], &env, "");
    assert_eq!(output.status.code(), Some(1));
    let reason = std::fs::read_to_string(&failure).unwrap();
    assert!(reason.contains("COMMON"), "{reason:?}");
    cli.ok(&["ui", "install"]);
    let output = cli.run(&["assistant", "warm"], &env, "");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!failure.exists());
}

#[test]
fn a_failed_ask_leaves_its_reason_like_warm() {
    require_tmux!();
    let cli = Cli::new();
    let path = cli.fake_claude_path();
    let env = [("PATH", path.as_str())];
    let failure = cli.state().join("plugins/assistant/warm.failed");
    let output = cli.run(&["assistant", "ask", "hello"], &env, "");
    assert_eq!(output.status.code(), Some(1));
    let reason = std::fs::read_to_string(&failure).unwrap();
    assert!(reason.contains("COMMON"), "{reason:?}");
}

#[test]
fn ensure_home_seeds_claude_trust_on_every_warm() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let path = cli.fake_claude_path();
    let config_file = cli.claude_config_dir().join(".claude.json");
    std::fs::write(&config_file, r#"{"projects":{}}"#).unwrap();
    let env = [("PATH", path.as_str())];
    let home = cli.state().join("plugins/assistant/home");
    let trusted = || {
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_file).unwrap()).unwrap();
        json["projects"][home.to_str().unwrap()]["hasTrustDialogAccepted"] == true
    };
    for round in ["first", "second"] {
        let output = cli.run(&["assistant", "warm"], &env, "");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(trusted(), "{round} warm");
        std::fs::write(&config_file, r#"{"projects":{}}"#).unwrap();
        assert!(!trusted());
    }
}

#[test]
fn warm_without_the_roles_installed_fails() {
    require_tmux!();
    let cli = Cli::new();
    let path = cli.fake_claude_path();
    let output = cli.run(&["assistant", "warm"], &[("PATH", &path)], "");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("COMMON"), "{stderr:?}");
    assert!(output.stdout.is_empty());
}

#[test]
fn warm_takes_no_other_subcommand() {
    let cli = Cli::new();
    assert!(
        cli.fails(&["assistant"], 2)
            .starts_with("usage: tx assistant warm")
    );
    assert!(
        cli.fails(&["assistant", "send"], 2)
            .starts_with("usage: tx assistant warm")
    );
}

#[test]
fn disabled_assistant_hides_the_verb() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  assistant: { enabled: false }\n");
    cli.fails(&["assistant", "warm"], 2);
    assert!(
        cli.ok(&["plugins"])
            .lines()
            .any(|line| line == "assistant\tdisabled"),
        "{}",
        cli.ok(&["plugins"])
    );
}
