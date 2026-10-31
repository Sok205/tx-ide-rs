//! The kill plugin through the binary: `txc kill`, and the plugin switch.

mod common;

use std::process::Command;

use common::{Cli, lifecycle_of};

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
fn kill_stops_and_reports() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert_eq!(cli.ok(&["kill", "a"]), "Killed 'a'\n");
    assert_eq!(lifecycle_of(&cli, "a"), "stopped");
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
fn kill_view_home_reports_a_failed_kill() {
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
    let real =
        String::from_utf8(Command::new("which").arg("tmux").output().unwrap().stdout).unwrap();
    let shim_dir = cli.dir.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("tmux");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = kill-session ] && exit 1; done\nexec {} \"$@\"\n",
            real.trim()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = cli.run(&["kill", "Views"], &[("PATH", &path)], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("kill-session"), "{stderr:?}");
    assert!(cli.tmux(&["has-session", "-t", "=Views"]).status.success());
}

/// The guard: a session core marked `@tx-id` whose record is gone is still never killed here.
#[test]
fn kill_refuses_an_agent_session_without_a_record() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "ghost", "sleep", "60"])
            .status
            .success()
    );
    assert!(
        cli.tmux(&["set-option", "-t", "=ghost:", "@tx-id", "0000-forgotten"])
            .status
            .success()
    );
    let output = cli.run(&["kill", "ghost"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("'ghost' is an agent's session"), "{stderr:?}");
    assert!(cli.tmux(&["has-session", "-t", "=ghost"]).status.success());
}

#[test]
fn kill_is_listed_active() {
    let cli = Cli::new();
    assert!(
        cli.ok(&["plugins"])
            .lines()
            .any(|line| line == "kill\tactive"),
        "{}",
        cli.ok(&["plugins"])
    );
}

#[test]
fn disabled_kill_hides_the_verb() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  kill: { enabled: false }\n");
    cli.fails(&["kill", "x"], 2);
    assert!(
        cli.ok(&["plugins"])
            .lines()
            .any(|line| line == "kill\tdisabled"),
        "{}",
        cli.ok(&["plugins"])
    );
}
