//! The picker plugin through the binary: the feed, the nesting `pick`, the plugin switch.

mod common;

use std::process::Command;

use common::{Cli, pane_id_of};

fn assert_rows(feed: &str, expected: usize) {
    let lines: Vec<&str> = feed.lines().collect();
    assert_eq!(lines.len(), expected, "{feed:?}");
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 5, "{line:?}");
        assert!(fields[3].starts_with('$'), "{line:?}");
    }
}

#[test]
fn list_prints_one_row_per_running_session() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    cli.create("b");
    assert_rows(&cli.ok(&["_list"]), 2);
}

#[test]
fn list_tolerates_malformed_hooks_ext() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("a");
    cli.create("b");
    let record = cli.state().join("sessions").join(format!("{id}.json"));
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&record).unwrap()).unwrap();
    json["ext"] = serde_json::json!({"hooks": 5});
    std::fs::write(&record, json.to_string()).unwrap();
    assert_rows(&cli.ok(&["_list"]), 2);
}

#[test]
fn list_is_empty_newline_without_sessions() {
    require_tmux!();
    let cli = Cli::new();
    assert_eq!(cli.ok(&["_list"]), "\n");
}

#[test]
fn pick_nests_into_a_view_pane() {
    require_tmux!();
    if Command::new("fzf").arg("--version").output().is_err() {
        eprintln!("fzf not available; skipping");
        return;
    }
    let cli = Cli::new();
    let shell = if Command::new("zsh").arg("-c").arg(":").output().is_ok() {
        "zsh"
    } else {
        "sh"
    };
    let cwd = cli.dir.path().join("inner");
    std::fs::create_dir_all(&cwd).unwrap();
    cli.ok(&[
        "create",
        "inner",
        "--cwd",
        cwd.to_str().unwrap(),
        "--exec",
        "sleep",
        "300",
    ]);
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "Views", "-c", "/tmp", shell])
            .status
            .success()
    );
    let set = cli.tmux(&["set-option", "-t", "=Views:", "@tx-view", "1"]);
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    let pane = pane_id_of(&cli, "Views");
    // `run-shell` stands in for the prefix+t popup: like the popup it runs `pick` with `$TMUX`
    // set while the pane's foreground process is still the shell.
    let pick = format!(
        "FZF_DEFAULT_OPTS='--filter=inner' {} --state-dir {} pick",
        env!("CARGO_BIN_EXE_txc"),
        cli.state().display()
    );
    assert!(
        cli.tmux(&["run-shell", "-b", "-t", &pane, &pick])
            .status
            .success()
    );
    let mut label = String::new();
    let nested = cli.wait_for(5, || {
        label = cli.ok(&["_pane-label", &pane]);
        label.contains("#[bold]inner#[default]")
    });
    assert!(nested, "pane never showed the nested session: {label:?}");
}

#[test]
fn picker_is_listed_active() {
    let cli = Cli::new();
    assert!(
        cli.ok(&["plugins"])
            .lines()
            .any(|line| line == "picker\tactive"),
        "{}",
        cli.ok(&["plugins"])
    );
}

#[test]
fn disabled_picker_hides_its_verbs() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  picker: { enabled: false }\n");
    cli.fails(&["pick"], 2);
    cli.fails(&["_list"], 2);
    assert!(
        cli.ok(&["plugins"])
            .lines()
            .any(|line| line == "picker\tdisabled"),
        "{}",
        cli.ok(&["plugins"])
    );
}
