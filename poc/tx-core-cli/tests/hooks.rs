//! The hooks plugin through the binary: `txc hook <event>` inside a session, `hook status`.

mod common;

use std::process::{Command, Output};

use common::Cli;

impl Cli {
    fn show(&self, name: &str) -> serde_json::Value {
        serde_json::from_str(&self.ok(&["show", name])).unwrap()
    }
    fn hook(&self, event: &str, id: &str, stdin: &str) -> Output {
        self.run(&["hook", event], &[("TX_SESSION_ID", id)], stdin)
    }
}

#[test]
fn create_seeds_idle_and_hooks_move_the_state() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "idle");
    assert!(cli.hook("prompt-submit", &id, "").status.success());
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "working");
    let since = cli.show("a")["ext"]["hooks"]["since"].as_f64().unwrap();
    assert!(cli.hook("working", &id, "").status.success());
    assert_eq!(
        cli.show("a")["ext"]["hooks"]["since"].as_f64().unwrap(),
        since,
        "same state → no write"
    );
    assert!(cli.hook("stop", &id, "{}").status.success());
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "waiting");
    assert!(
        cli.hook("notification", &id, r#"{"notification_type":"other"}"#)
            .status
            .success()
    );
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "waiting");
    assert!(cli.hook("prompt-submit", &id, "").status.success());
    assert!(
        cli.hook(
            "notification",
            &id,
            r#"{"notification_type":"permission_prompt"}"#
        )
        .status
        .success()
    );
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "waiting");
    assert!(cli.hook("session-end", &id, "garbage").status.success());
    assert_eq!(cli.show("a")["ext"]["hooks"]["state"], "idle");
}

#[test]
fn not_ours_exits_zero_and_writes_nothing() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    let before = cli.show("a");
    let output = cli.run(&["hook", "stop"], &[], "");
    assert!(output.status.success(), "no TX_SESSION_ID");
    let output = cli.hook("stop", "0f6b3b8a-1111-4222-8333-444444444444", "");
    assert!(output.status.success(), "unknown session");
    let output = cli.hook("stop", "not-an-id", "");
    assert!(output.status.success(), "malformed id");
    let output = cli.hook("no-such-event", &id, "");
    assert!(output.status.success(), "unknown event");
    assert_eq!(cli.show("a"), before);
}

#[test]
fn unknown_session_is_not_ours() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    cli.ok(&["stop", "a"]);
    cli.ok(&["forget", "a"]);
    let output = cli.hook("stop", &id, "");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).is_empty());
}

#[test]
fn hook_without_event_is_usage() {
    let cli = Cli::new();
    let output = cli.run(&["hook"], &[], "");
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn status_lists_sessions_and_flags_stuck() {
    require_tmux!();
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  hooks: { settings: { stuck_after_seconds: 0 } }\n");
    let id = cli.create("a");
    cli.create("b");
    let status = cli.ok(&["hook", "status"]);
    assert!(status.contains("a\tidle\t"), "{status}");
    assert!(status.contains("b\tidle\t"), "{status}");
    assert!(cli.hook("prompt-submit", &id, "").status.success());
    std::thread::sleep(std::time::Duration::from_millis(20));
    let status = cli.ok(&["hook", "status"]);
    assert!(status.contains("a\tstuck\t"), "{status}");
    assert!(status.contains("b\tidle\t"), "{status}");
}

#[test]
fn bad_settings_fail_at_startup() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  hooks: { settings: { stuck_after_seconds: soon } }\n");
    let output = cli.run(&["engines"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("stuck_after_seconds"));
}

#[test]
fn disabled_hooks_plugin_has_no_verb_and_no_seed() {
    require_tmux!();
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  hooks: { enabled: false }\n");
    cli.create("a");
    assert!(cli.show("a").get("ext").is_none());
    assert_eq!(cli.run(&["hook", "status"], &[], "").status.code(), Some(2));
}

#[test]
fn install_then_uninstall_round_trips_under_a_temp_home() {
    let cli = Cli::new();
    let home = cli.dir.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let text = "{\"theme\": \"dark\"}\n";
    std::fs::write(home.join(".claude/settings.json"), text).unwrap();
    let env = [("HOME", home.to_str().unwrap())];
    let output = cli.run(&["hook", "install"], &env, "");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap())
            .unwrap();
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(command.ends_with("txc' hook stop || exit 1"), "{command}");
    let exe = command.trim_start_matches('\'').split('\'').next().unwrap();
    assert!(std::path::Path::new(exe).is_absolute(), "{exe}");
    let output = cli.run(&["hook", "uninstall"], &env, "");
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        text
    );
}

/// Claude Code treats exit 2 from a hook as "block"; the installed command must never produce
/// it, whatever the state of plugins.yaml.
#[test]
fn installed_command_never_exits_2() {
    let cli = Cli::new();
    let home = cli.dir.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let env = [("HOME", home.to_str().unwrap())];
    assert!(cli.run(&["hook", "install"], &env, "").status.success());
    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(home.join(".claude/settings.json")).unwrap())
            .unwrap();
    let command = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_owned();
    let run = |yaml: &str| {
        cli.write_yaml(yaml);
        Command::new("sh")
            .arg("-c")
            .arg(&command)
            .env("TX_CORE_HOME", cli.state())
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID")
            .output()
            .unwrap()
            .status
            .code()
    };
    assert_ne!(run("plugins: [\n"), Some(2), "malformed yaml");
    assert_ne!(
        run("plugins:\n  hooks: { enabled: false }\n"),
        Some(2),
        "plugin disabled"
    );
    assert_ne!(run("plugins:\n  nope: {}\n"), Some(2), "unknown id");
    assert_eq!(run(""), Some(0), "healthy, not ours");
}

#[test]
fn hook_subcommands_reject_stray_arguments() {
    let cli = Cli::new();
    let home = cli.dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let env = [("HOME", home.to_str().unwrap())];
    for args in [
        ["hook", "install", "--dryrun"],
        ["hook", "install", "--dry_run"],
        ["hook", "uninstall", "extra"],
        ["hook", "status", "extra"],
    ] {
        let output = cli.run(&args, &env, "");
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
    assert!(
        !home.join(".claude").exists(),
        "a typo never writes to HOME"
    );
}
