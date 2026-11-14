//! `tx gc`: the build output of agents ended longer ago than the threshold goes; running agents,
//! recent ones, tracked paths and paths leading out of the worktree stay.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{Cli, cwd_of, record_path};

/// A started agent, stopped, with its record dated `seconds_ago`; its worktree.
fn ended_agent(cli: &Cli, name: &str, seconds_ago: f64) -> (String, PathBuf) {
    let id = cli.create(name);
    let worktree = cwd_of(cli, &id);
    cli.ok(&["stop", &id]);
    age(cli, &id, seconds_ago);
    (id, worktree)
}

/// Sets the record's `last_modified_at` to `seconds_ago` before now.
fn age(cli: &Cli, id: &str, seconds_ago: f64) {
    let path = record_path(cli, id);
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    record["last_modified_at"] = serde_json::json!(now - seconds_ago);
    std::fs::write(&path, record.to_string()).unwrap();
}

/// `<worktree>/target/debug/build.bin` with 64 KB in it.
fn build_output(worktree: &Path) -> PathBuf {
    let file = worktree.join("target/debug/build.bin");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, vec![7u8; 64 * 1024]).unwrap();
    file
}

fn git(worktree: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const TWO_DAYS: f64 = 2.0 * 24.0 * 3600.0;

#[test]
fn ended_agents_lose_their_build_output_and_running_ones_keep_it() {
    require_tmux!();
    let cli = Cli::new();
    let (ended, ended_worktree) = ended_agent(&cli, "ended", TWO_DAYS);
    let running = cli.create("running");
    let running_worktree = cwd_of(&cli, &running);
    let ended_file = build_output(&ended_worktree);
    let running_file = build_output(&running_worktree);
    // Even dated long ago, a running record is never touched.
    age(&cli, &running, TWO_DAYS);

    let printed = cli.ok(&["gc"]);

    assert!(!ended_worktree.join("target").exists(), "{printed}");
    assert!(running_file.exists());
    assert!(!ended_file.exists());
    let lines: Vec<&str> = printed.lines().collect();
    assert_eq!(lines.len(), 2, "{printed}");
    let fields: Vec<&str> = lines[0].split('\t').collect();
    assert_eq!(fields[1], "ended", "{printed}");
    assert_eq!(Path::new(fields[2]), ended_worktree, "{printed}");
    assert!(fields[0].ends_with("KB"), "{printed}");
    assert_eq!(lines[1], format!("{}\ttotal", fields[0]), "{printed}");
    // The worktree, its files and the record stay, so the agent respawns.
    assert!(ended_worktree.join("README.md").exists());
    let path = cli.fake_claude_path();
    let respawned = cli.run(&["respawn", &ended], &[("PATH", path.as_str())], "");
    assert!(
        respawned.status.success(),
        "{}",
        String::from_utf8_lossy(&respawned.stderr)
    );
}

#[test]
fn only_agents_ended_before_the_threshold_are_cleaned() {
    require_tmux!();
    let cli = Cli::new();
    let (_, worktree) = ended_agent(&cli, "recent", 2.0 * 3600.0);
    let file = build_output(&worktree);

    assert_eq!(cli.ok(&["gc"]), "0 B\ttotal\n");
    assert_eq!(cli.ok(&["gc", "--older-than", "3h"]), "0 B\ttotal\n");
    assert!(file.exists());

    cli.ok(&["gc", "--older-than", "1h"]);
    assert!(!file.exists());
}

#[test]
fn a_dry_run_prints_the_same_and_deletes_nothing() {
    require_tmux!();
    let cli = Cli::new();
    let (_, worktree) = ended_agent(&cli, "dry", TWO_DAYS);
    let file = build_output(&worktree);

    let dry = cli.ok(&["gc", "--dry-run"]);
    assert!(file.exists());
    let real = cli.ok(&["gc"]);
    assert!(!file.exists());

    let size = |text: &str| text.lines().next().unwrap().to_owned();
    assert_eq!(size(&dry), size(&real), "{dry}\n{real}");
    assert!(dry.ends_with("\ttotal, dry run\n"), "{dry}");
    assert!(real.ends_with("\ttotal\n"), "{real}");
}

#[test]
fn a_path_git_tracks_is_left_alone() {
    require_tmux!();
    let cli = Cli::new();
    let (_, worktree) = ended_agent(&cli, "tracked", TWO_DAYS);
    let file = build_output(&worktree);
    git(&worktree, &["add", "-f", "target/debug/build.bin"]);
    git(&worktree, &["commit", "-q", "-m", "tracked build output"]);

    let output = cli.run(&["gc"], &[], "");

    assert!(output.status.success());
    assert!(file.exists());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("git tracks files under it"), "{stderr}");
    assert!(
        String::from_utf8_lossy(&output.stdout).ends_with("0 B\ttotal\n"),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
fn a_symlink_out_of_the_worktree_is_left_alone() {
    require_tmux!();
    let cli = Cli::new();
    let (_, worktree) = ended_agent(&cli, "linked", TWO_DAYS);
    let outside = cli.dir.path().join("outside");
    let precious = build_output(&outside);
    std::os::unix::fs::symlink(outside.join("target"), worktree.join("target")).unwrap();

    let stderr = String::from_utf8_lossy(&cli.run(&["gc"], &[], "").stderr).into_owned();

    assert!(precious.exists());
    assert!(stderr.contains("leads out of the worktree"), "{stderr}");
}

#[test]
fn a_configured_path_leaving_the_worktree_is_a_usage_error() {
    require_tmux!();
    let cli = Cli::new();
    let (_, worktree) = ended_agent(&cli, "dotdot", TWO_DAYS);
    let file = build_output(&worktree);

    for bad in ["../dotdot", "/tmp", "target/../.."] {
        cli.write_yaml(&format!("worktree:\n  build_outputs: [{bad}]\n"));
        let stderr = cli.fails(&["gc"], 2);
        assert!(stderr.contains("build_outputs"), "{bad}: {stderr}");
    }
    assert!(file.exists());
}
