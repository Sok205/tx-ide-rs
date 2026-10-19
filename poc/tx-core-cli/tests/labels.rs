//! The labels plugin through the binary: the pane-border and tree-label feeds, and `whoami`.

mod common;

use std::process::Command;

use common::{Cli, lists_verb, pane_id_of};

#[test]
fn pane_info_two_lines() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    cli.ok(&["tag", "a", "x", "y"]);
    assert_eq!(cli.ok(&["_pane-info", &id]), "a\nx,y\n");
}

#[test]
fn pane_info_unknown_is_two_empty_lines() {
    require_tmux!();
    let cli = Cli::new();
    assert_eq!(cli.ok(&["_pane-info", "nope"]), "\n\n");
}

#[test]
fn tmux_name_prints_dollar_target_for_live_only() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    cli.create("b");
    let target = cli.ok(&["_tmux-name", "a"]);
    assert!(
        target.starts_with('$') && target.ends_with('\n') && target.trim().len() > 1,
        "{target:?}"
    );
    assert_eq!(
        target.trim(),
        cli.tmux_out(&["display-message", "-p", "-t", "=a:", "#{session_id}"])
    );
    cli.ok(&["stop", "b"]);
    assert_eq!(cli.ok(&["_tmux-name", "b"]), "");
    assert_eq!(cli.ok(&["_tmux-name", "nope"]), "");
}

#[test]
fn relabel_sets_tx_name_with_chips() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("n");
    cli.create("plain");
    cli.ok(&["tag", "n", "a", "b"]);
    assert_eq!(cli.ok(&["_relabel"]), "");
    assert_eq!(
        cli.tmux_out(&["show-options", "-vqt", "=n:", "@tx-name"]),
        "n [a,b]"
    );
    assert_eq!(
        cli.tmux_out(&["show-options", "-vqt", "=plain:", "@tx-name"]),
        "plain"
    );
}

#[test]
fn pane_label_dash_when_nothing_nested() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    let pane = pane_id_of(&cli, "a");
    assert_eq!(
        cli.ok(&["_pane-label", &pane, "0"]),
        "#[fg=colour240,nobold]\u{2014}#[default]"
    );
}

#[test]
fn pane_label_shows_nested_session() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("inner");
    cli.ok(&["tag", "inner", "t1"]);
    let shell = if Command::new("zsh").arg("-c").arg(":").output().is_ok() {
        "zsh"
    } else {
        "sh"
    };
    let cwd = cli.dir.path().join("outer");
    std::fs::create_dir_all(&cwd).unwrap();
    cli.ok(&[
        "create",
        "outer",
        "--cwd",
        cwd.to_str().unwrap(),
        "--exec",
        shell,
    ]);
    let pane = pane_id_of(&cli, "outer");
    let attach = format!(
        "TMUX= tmux -S {} attach -t '=inner'",
        cli.sock().to_str().unwrap()
    );
    assert!(
        cli.tmux(&["send-keys", "-t", "=outer:", &attach, "Enter"])
            .status
            .success()
    );
    let mut attached = false;
    for _ in 0..30 {
        if !cli.tmux_out(&["list-clients"]).is_empty() {
            attached = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(attached, "nested client never attached");
    let label = cli.ok(&["_pane-label", &pane]);
    assert!(label.contains("#[bold]inner#[default]"), "{label:?}");
    assert!(label.contains("[t1]"), "{label:?}");
    assert!(!label.ends_with('\n'), "{label:?}");
    cli.ok(&["stop", "outer"]);
}

#[test]
fn whoami_outside_server_exits_1() {
    require_tmux!();
    let cli = Cli::new();
    let output = cli.run(&["whoami"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "txc whoami: not inside the tx tmux server\n"
    );
}

#[test]
fn labels_is_listed_active() {
    let cli = Cli::new();
    assert!(
        cli.ok(&["plugins"])
            .lines()
            .any(|line| line == "labels\tactive"),
        "{}",
        cli.ok(&["plugins"])
    );
}

#[test]
fn disabled_labels_hides_its_verbs_but_not_kill() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  labels: { enabled: false }\n");
    for verb in [
        "whoami",
        "_pane-info",
        "_tmux-name",
        "_relabel",
        "_pane-label",
    ] {
        cli.fails(&[verb], 2);
    }
    let help = cli.ok(&["help"]);
    assert!(lists_verb(&help, "kill"), "{help}");
}
