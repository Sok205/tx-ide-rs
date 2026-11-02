//! Views through the binary: `txc ui view open|ls|close`, the guard on an agent's session,
//! orphans closed on listing, and prefix+X on a view.

mod common;

use std::process::Command;

use common::{Cli, state_of, pane_id_of};

fn view_option(cli: &Cli, view: &str, option: &str) -> String {
    cli.tmux_out(&["show-options", "-vqt", &format!("={view}:"), option])
}

fn has_session(cli: &Cli, name: &str) -> bool {
    cli.tmux(&["has-session", "-t", &format!("={name}")])
        .status
        .success()
}

#[test]
fn view_open_ls_and_close() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    let cwd = cli.dir.path().join("work");
    std::fs::create_dir_all(&cwd).unwrap();
    assert_eq!(
        cli.ok(&[
            "ui",
            "view",
            "open",
            "qa-shell",
            "--kind",
            "shell",
            "--parent",
            "a",
            "--cwd",
            cwd.to_str().unwrap(),
            "--tag",
            "qa",
            "--tag",
            "x",
            "--bound",
            "b1",
            "--",
            "sleep",
            "600",
        ]),
        "qa-shell\n"
    );
    assert_eq!(view_option(&cli, "qa-shell", "@tx-view"), "1");
    assert_eq!(view_option(&cli, "qa-shell", "@tx-view-kind"), "shell");
    assert_eq!(view_option(&cli, "qa-shell", "@tx-view-parent"), id);
    assert_eq!(view_option(&cli, "qa-shell", "@tx-view-bound"), "b1");
    assert_eq!(view_option(&cli, "qa-shell", "@tx-view-tags"), "qa,x");
    assert!(!view_option(&cli, "qa-shell", "@tx-view-created").is_empty());
    assert_eq!(
        cli.tmux_out(&[
            "display-message",
            "-p",
            "-t",
            "=qa-shell:",
            "#{pane_start_command}"
        ]),
        "sleep 600"
    );
    // The view is not a core record: never in `txc ls`, listed by `ui view ls` with its parent's
    // name.
    assert!(!cli.ok(&["ls"]).contains("qa-shell"));
    let listed = cli.ok(&["ui", "view", "ls"]);
    assert_eq!(
        listed,
        format!("qa-shell\tshell\ta\t{}\tqa,x\tb1\n", cwd.display())
    );
    assert_eq!(cli.ok(&["ui", "view", "close", "qa-shell"]), "");
    assert!(!has_session(&cli, "qa-shell"));
    let output = cli.run(&["ui", "view", "close", "qa-shell"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "txc ui view close: no session named 'qa-shell'\n"
    );
    assert_ne!(state_of(&cli, "a"), "ended");
}

#[test]
fn view_open_runs_the_default_shell_in_the_current_directory_when_given_no_command() {
    require_tmux!();
    let cli = Cli::new();
    let cwd = cli.dir.path().join("here");
    std::fs::create_dir_all(&cwd).unwrap();
    let output = cli
        .command(&["ui", "view", "open", "plain", "--kind", "editor"], &[("SHELL", "sh")])
        .current_dir(&cwd)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        cli.tmux_out(&["display-message", "-p", "-t", "=plain:", "#{pane_start_command}"]),
        "sh"
    );
    assert_eq!(
        cli.tmux_out(&["display-message", "-p", "-t", "=plain:", "#{session_path}"]),
        cwd.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(cli.ok(&["ui", "view", "ls"]).split('\t').nth(2), Some(""));
}

#[test]
fn view_open_rejects_a_taken_name_a_missing_kind_and_an_unknown_parent() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    cli.ok(&["ui", "view", "open", "s", "--kind", "shell", "--", "sleep", "600"]);
    let taken = cli.run(
        &["ui", "view", "open", "s", "--kind", "shell", "--", "sleep", "600"],
        &[],
        "",
    );
    assert_eq!(taken.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&taken.stderr).contains("a session named 's' already exists"),
        "{taken:?}"
    );
    let usage = cli.fails(&["ui", "view", "open", "t"], 2);
    assert!(usage.contains("usage: txc ui view open"), "{usage}");
    cli.fails(&["ui", "view", "nope"], 2);
    let parent = cli.run(
        &["ui", "view", "open", "t", "--kind", "shell", "--parent", "ghost"],
        &[],
        "",
    );
    assert_eq!(parent.status.code(), Some(1));
    assert!(!has_session(&cli, "t"));
}

/// `txc ui view close` on an agent's tmux session is refused: the agent keeps running and only
/// core may stop it.
#[test]
fn view_close_refuses_an_agent_session() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    let output = cli.run(&["ui", "view", "close", "a"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("'a' is an agent's session"),
        "{stderr:?}"
    );
    assert!(has_session(&cli, "a"));
    assert_ne!(state_of(&cli, "a"), "ended");
    assert!(cli.ok(&["ui", "view", "ls"]).is_empty());
}

/// A view whose parent was stopped is an orphan; `ls` closes it and says so.
#[test]
fn view_ls_closes_the_views_of_a_stopped_agent() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    cli.create("b");
    for (view, parent) in [("va", "a"), ("vb", "b")] {
        cli.ok(&[
            "ui", "view", "open", view, "--kind", "shell", "--parent", parent, "--", "sleep",
            "600",
        ]);
    }
    cli.ok(&["ui", "view", "open", "free", "--kind", "shell", "--", "sleep", "600"]);
    cli.ok(&["kill", "a"]);
    // A stopped agent still resolves, so a late view can name it; the listing closes it too.
    cli.ok(&[
        "ui", "view", "open", "va2", "--kind", "shell", "--parent", "a", "--", "sleep", "600",
    ]);
    let output = cli.run(&["ui", "view", "ls"], &[], "");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut listed: Vec<&str> = stdout
        .lines()
        .map(|line| line.split('\t').next().unwrap())
        .collect();
    listed.sort_unstable();
    assert_eq!(listed, ["free", "vb"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("closed orphaned view 'va'"), "{stderr:?}");
    assert!(stderr.contains("closed orphaned view 'va2'"), "{stderr:?}");
    assert!(!has_session(&cli, "va"));
    assert!(!has_session(&cli, "va2"));
    assert!(has_session(&cli, "vb"));
    assert!(has_session(&cli, "free"));
}

/// `txc kill <view>` keeps closing a view by name, and refuses anything core owns.
#[test]
fn kill_closes_a_view_by_name() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    cli.ok(&["ui", "view", "open", "s", "--kind", "shell", "--", "sleep", "600"]);
    assert_eq!(cli.ok(&["kill", "s"]), "Killed 's'\n");
    assert!(!has_session(&cli, "s"));
}

/// `ui install`, then a server started from its conf so the kill script finds `@tx-bin` and
/// `@tx-arguments`.
fn installed_server(cli: &Cli) {
    cli.write_yaml("plugins:\n  assistant: { enabled: false }\n");
    cli.ok(&["ui", "install"]);
    let started = Command::new("tmux")
        .arg("-S")
        .arg(cli.sock())
        .arg("-f")
        .arg(cli.state().join("tmux.conf"))
        .args(["new-session", "-d", "-s", "probe", "sleep", "600"])
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
}

fn outer_screen(cli: &Cli) -> String {
    let captured = Command::new("tmux")
        .arg("-S")
        .arg(cli.outer_sock())
        .args(["capture-pane", "-p", "-t", "=outer:"])
        .env_remove("TMUX")
        .output()
        .unwrap();
    String::from_utf8_lossy(&captured.stdout).into_owned()
}

fn outer_send_keys(cli: &Cli, key: &str) {
    let sent = Command::new("tmux")
        .arg("-S")
        .arg(cli.outer_sock())
        .args(["send-keys", "-t", "=outer:", key])
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert!(sent.status.success(), "{sent:?}");
}

/// Runs the installed prefix+X script against `view` the way the binding does, from inside the
/// server with a client attached to the view. In the background: the script's `confirm-before`
/// returns only once the prompt is answered.
fn press_prefix_x(cli: &Cli, view: &str) {
    let pane = pane_id_of(cli, view);
    let script = format!(
        "{} '{pane}' '{view}'",
        cli.state().join("bin/tmux-kill-tx-session").display()
    );
    let run = cli.tmux(&["run-shell", "-b", "-t", &pane, &script]);
    assert!(run.status.success(), "{run:?}");
}

fn attach_outer_client(cli: &Cli, view: &str) {
    cli.in_outer_pane(&format!(
        "TMUX= tmux -S {} attach -t ={view}",
        cli.sock().display()
    ));
    assert!(
        cli.wait_for(5, || cli
            .tmux_out(&["list-clients", "-F", "#{client_session}"])
            .lines()
            .any(|line| line == view)),
        "no client attached to {view}"
    );
}

#[test]
fn prefix_x_closes_a_shell_view_after_confirming() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli);
    cli.create("a");
    cli.ok(&["ui", "view", "open", "qa-shell", "--kind", "shell", "--", "sleep", "600"]);
    attach_outer_client(&cli, "qa-shell");
    press_prefix_x(&cli, "qa-shell");
    assert!(
        cli.wait_for(5, || outer_screen(&cli).contains("close shell view")),
        "no confirmation prompt: {}",
        outer_screen(&cli)
    );
    outer_send_keys(&cli, "y");
    assert!(
        cli.wait_for(5, || !has_session(&cli, "qa-shell")),
        "the shell view is still there: {}",
        outer_screen(&cli)
    );
    assert_ne!(state_of(&cli, "a"), "ended");
    assert!(has_session(&cli, "probe"));
}

#[test]
fn prefix_x_refuses_the_home_view() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli);
    cli.ok(&["ui", "view", "open", "home", "--kind", "home", "--", "sleep", "600"]);
    attach_outer_client(&cli, "home");
    press_prefix_x(&cli, "home");
    assert!(
        cli.wait_for(5, || outer_screen(&cli).contains("is a view home")),
        "no refusal shown: {}",
        outer_screen(&cli)
    );
    outer_send_keys(&cli, "y");
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert!(has_session(&cli, "home"), "the home view was closed");
}

#[test]
fn ui_lists_the_view_subcommand_in_its_usage() {
    let cli = Cli::new();
    let usage = cli.fails(&["ui"], 2);
    assert!(usage.contains("view <open|ls|close>"), "{usage}");
}
