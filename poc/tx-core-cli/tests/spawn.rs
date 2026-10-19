//! The spawn plugin through the binary: `_new-agent` with a stand-in for nvim, and the plugin
//! switch.

mod common;

use common::{Cli, lifecycle_of};

/// An `nvim` that answers the form from `$TX_NEW_AGENT_ANSWERS` (cancels when it is unset).
fn fake_nvim() -> &'static str {
    "[ -n \"${TX_NEW_AGENT_ANSWERS:-}\" ] || exit 0\n\
     printf '%s' \"$TX_NEW_AGENT_ANSWERS\" > \"$TX_NEW_AGENT_RESULT\""
}

fn answers(cwd: &str) -> String {
    serde_json::json!({
        "name": "health",
        "prompt": "add GET /health",
        "engine": "claude",
        "model": "fable",
        "effort": "low",
        "role": "DEVELOPER",
        "tags": "tutor, api",
        "cwd": cwd,
    })
    .to_string()
}

#[test]
fn new_agent_creates_the_session_the_form_describes() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let path = cli.shim_path(&[("claude", "exec sleep 60"), ("nvim", fake_nvim())]);
    let cwd = cli.dir.path().join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let answers = answers(cwd.to_str().unwrap());
    let output = cli.run(
        &["_new-agent", "%0", "client-0"],
        &[("PATH", &path), ("TX_NEW_AGENT_ANSWERS", &answers)],
        "",
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stderr), "spawned health\n");
    assert_eq!(lifecycle_of(&cli, "health"), "running");
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["show", "health"])).unwrap();
    assert_eq!(json["tags"], serde_json::json!(["tutor", "api"]));
    assert_eq!(json["cwd"], cwd.canonicalize().unwrap().to_str().unwrap());
    assert_eq!(json["launch"]["engine"], "claude");
    assert_eq!(
        json["launch"]["profile"],
        serde_json::json!(["COMMON", "DEVELOPER"])
    );
    assert_eq!(json["launch"]["model"], serde_json::Value::Null);
    assert_eq!(json["launch"]["effort"], "low");
    assert!(cwd.join(".claude/skills/tx-diff-review").exists());
}

#[test]
fn a_cancelled_form_creates_nothing() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let path = cli.shim_path(&[("nvim", fake_nvim())]);
    let output = cli.run(&["_new-agent", "%0", "client-0"], &[("PATH", &path)], "");
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert!(cli.ok(&["ls"]).is_empty());
}

#[test]
fn a_failed_create_is_reported_on_the_status_line() {
    require_tmux!();
    let cli = Cli::new();
    let path = cli.shim_path(&[("claude", "exec sleep 60"), ("nvim", fake_nvim())]);
    let answers = answers("/nonexistent/dir");
    let output = cli.run(
        &["_new-agent", "%0", "client-0"],
        &[("PATH", &path), ("TX_NEW_AGENT_ANSWERS", &answers)],
        "",
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("spawn of health failed: "), "{stderr:?}");
    assert!(cli.ok(&["ls"]).is_empty());
}

#[test]
fn new_agent_wants_a_pane_and_a_client() {
    let cli = Cli::new();
    assert!(
        cli.fails(&["_new-agent", "%0"], 2)
            .starts_with("usage: txc _new-agent <pane> <client>")
    );
}

#[test]
fn disabled_spawn_hides_the_verb_and_the_key() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  spawn: { enabled: false }\n");
    cli.fails(&["_new-agent", "%0", "c"], 2);
    cli.ok(&["ui", "install"]);
    assert!(!cli.state().join("keys/spawn.tmux").exists());
    assert!(!cli.state().join("agents/DEVELOPER.md").exists());
    assert!(cli.state().join("agents/TX-ASSISTANT.md").exists());
}
