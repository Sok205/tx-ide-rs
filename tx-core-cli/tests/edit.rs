//! The edit plugin through the binary: `_edit-session`, and the plugin switch.

mod common;

use common::Cli;

#[test]
fn edit_session_on_view_home_exits_1() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "Views", "sleep", "60"])
            .status
            .success()
    );
    assert!(
        cli.tmux(&["set-option", "-t", "=Views:", "@tx-view", "1"])
            .status
            .success()
    );
    let pane = cli.tmux_out(&["display-message", "-p", "-t", "=Views:", "#{pane_id}"]);
    let output = cli.run(&["_edit-session", &pane, "nonexistent-client"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("prefix+e: no tx session in this pane (view homes cannot be edited)"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn edit_is_listed_active() {
    let cli = Cli::new();
    assert!(
        cli.ok(&["plugins"])
            .lines()
            .any(|line| line == "edit\tactive"),
        "{}",
        cli.ok(&["plugins"])
    );
}

#[test]
fn disabled_edit_hides_its_verbs() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  edit: { enabled: false }\n");
    cli.fails(&["_edit-session", "%0", "c"], 2);
    cli.fails(&["_edit-tag", "x"], 2);
    assert!(
        cli.ok(&["plugins"])
            .lines()
            .any(|line| line == "edit\tdisabled"),
        "{}",
        cli.ok(&["plugins"])
    );
}
