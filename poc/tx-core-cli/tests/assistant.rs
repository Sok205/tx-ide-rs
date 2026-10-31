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
fn warm_restarts_a_vanished_assistant() {
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
    let restarted = cli.run(&["assistant", "warm"], &env, "");
    assert_eq!(restarted.status.code(), Some(0));
    // The ended assistant keeps its name and its worktree, untouched; the new one is the next
    // free name, in a worktree of its own, and `warm` says which.
    assert_eq!(
        String::from_utf8_lossy(&restarted.stdout),
        "tx-assistant-2\n"
    );
    let listed = cli.ok(&["ls", "--tag", "tx-system"]);
    assert_eq!(listed.lines().count(), 2, "{listed:?}");
    assert_ne!(listed, first);
    assert_eq!(state_of(&cli, "tx-assistant"), "ended");
    assert_eq!(cwd_of(&cli, "tx-assistant"), first_cwd);
    assert_eq!(
        std::fs::read_to_string(first_cwd.join("note.txt")).unwrap(),
        "keep me\n"
    );
    assert_ne!(state_of(&cli, "tx-assistant-2"), "ended");
    let second_cwd = cwd_of(&cli, "tx-assistant-2");
    assert_ne!(second_cwd, first_cwd);
    assert!(
        second_cwd.ends_with("home--tx-assistant-2"),
        "{second_cwd:?}"
    );
    // Warm again: the running one is found by its role, not by the name `tx-assistant`.
    let again = cli.run(&["assistant", "warm"], &env, "");
    assert_eq!(String::from_utf8_lossy(&again.stdout), "tx-assistant-2\n");
    assert_eq!(cli.ok(&["ls", "--tag", "tx-system"]), listed);
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
            .starts_with("usage: txc assistant warm")
    );
    assert!(
        cli.fails(&["assistant", "send"], 2)
            .starts_with("usage: txc assistant warm")
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
