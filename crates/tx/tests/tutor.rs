//! The shipped Python pack: it loads, its checks are not vacuous against the bare skeleton,
//! and a complete reference solution passes every automatable check of every lesson.

use std::path::{Path, PathBuf};
use std::process::Command;

use tx::tutor::checks::{Context, run};
use tx::tutor::pack::{Check, Pack};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn pack() -> Pack {
    Pack::load("python", &repo().join("tutor/python")).unwrap()
}

fn has_python() -> bool {
    Command::new("python3").arg("--version").output().is_ok()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Every check a machine can decide without tmux records or a human.
fn automatable(pack: &Pack, id: &str) -> Vec<Check> {
    let lesson = &pack.lessons[pack.index_of(id).unwrap()];
    lesson
        .checks
        .iter()
        .filter(|check| !matches!(check, Check::Tx(_) | Check::Confirm(_)))
        .cloned()
        .collect()
}

fn passes(pack: &Pack, project: &Path, id: &str) -> bool {
    let sessions = || Ok(Vec::new());
    let mut confirm = |_: &str| true;
    let report = run(
        &automatable(pack, id),
        &mut Context { project, run: &pack.meta.run, sessions: &sessions, confirm: &mut confirm },
    );
    if !report.passed() {
        eprintln!("{id}:\n{}", report.render());
    }
    report.passed()
}

#[test]
fn the_python_pack_loads_with_twelve_lessons() {
    let pack = pack();
    assert_eq!(pack.lessons.len(), 12);
    assert_eq!(pack.meta.run, ["python3", "server.py"]);
    assert!(pack.skeleton().join("server.py").is_file());
}

#[test]
fn the_bare_skeleton_passes_lesson_02_only() {
    if !has_python() {
        eprintln!("skipped: python3 not on PATH");
        return;
    }
    let pack = pack();
    let project = tempfile::tempdir().unwrap();
    copy_dir(&pack.skeleton(), project.path());
    assert!(passes(&pack, project.path(), "02-request-response"));
    for id in ["04-first-route", "06-review-and-merge", "07-not-found", "08-fork-post", "10-assistant-tests"] {
        assert!(!passes(&pack, project.path(), id), "{id} passed against the bare skeleton");
    }
}

#[test]
fn the_reference_solution_completes_the_curriculum() {
    if !has_python() {
        eprintln!("skipped: python3 not on PATH");
        return;
    }
    let pack = pack();
    let project = tempfile::tempdir().unwrap();
    copy_dir(&pack.skeleton(), project.path());
    copy_dir(&repo().join("crates/tx/tests/fixtures/tutor-python"), project.path());
    for lesson in &pack.lessons {
        assert!(passes(&pack, project.path(), &lesson.id), "{} failed", lesson.id);
    }
}

// ----- CLI tests (`tx tutor …`) --------------------------------------------------------------

fn tx(home: &Path, args: &[&str]) -> std::process::Output {
    let tx_home = home.join("tx-home");
    std::fs::create_dir_all(&tx_home).unwrap();
    Command::new(env!("CARGO_BIN_EXE_tx"))
        .args(args)
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TX_IDE_HOME", &tx_home)
        .env("TMUX_TMPDIR", home)
        .output()
        .unwrap()
}

fn out(output: &std::process::Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// A started tutorial without tmux: the skeleton copied into `<home>/proj`, progress on lesson 01.
fn started(home: &Path) -> PathBuf {
    let project = home.join("proj");
    copy_dir(&pack().skeleton(), &project);
    std::fs::write(project.join(".tx-tutor"), "python\n").unwrap();
    let progress = serde_json::json!({"dir": project, "current": "01-what-is-a-server", "passed": []});
    std::fs::create_dir_all(home.join("tx-home/tutor")).unwrap();
    std::fs::write(home.join("tx-home/tutor/python.json"), progress.to_string()).unwrap();
    project
}

#[test]
fn verbs_need_a_started_tutorial() {
    let home = tempfile::tempdir().unwrap();
    let output = tx(home.path(), &["tutor", "status"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(out(&output).contains("no python tutorial in progress; run `tx tutor start`"), "{}", out(&output));
}

#[test]
fn goto_status_hint_move_through_lessons() {
    let home = tempfile::tempdir().unwrap();
    started(home.path());
    let goto = tx(home.path(), &["tutor", "goto", "4"]);
    assert_eq!(goto.status.code(), Some(0), "{}", out(&goto));
    assert!(out(&goto).contains("Lesson 4/12: 04-first-route"));
    let status = out(&tx(home.path(), &["tutor", "status"]));
    assert!(status.contains("▶  4. 04-first-route"), "{status}");
    assert!(status.contains("   1. 01-what-is-a-server"), "{status}");
    assert!(out(&tx(home.path(), &["tutor", "hint"])).contains("self.path == \"/hello\""));
    assert!(out(&tx(home.path(), &["tutor", "next"])).contains("Lesson 5/12: 05-spawn-worker"));
    assert!(out(&tx(home.path(), &["tutor", "hint"])).contains("no hint for this lesson"));
    assert!(out(&tx(home.path(), &["tutor", "prev"])).contains("Lesson 4/12"));
    assert_eq!(tx(home.path(), &["tutor", "goto", "99"]).status.code(), Some(1));
}

#[test]
fn check_advances_only_on_pass() {
    if !has_python() {
        eprintln!("skipped: python3 not on PATH");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    started(home.path());
    tx(home.path(), &["tutor", "goto", "4"]);
    let failed = tx(home.path(), &["tutor", "check"]);
    assert_eq!(failed.status.code(), Some(1), "{}", out(&failed));
    assert!(out(&failed).contains("✗ GET /hello answers 200 with 'hello'"));
    // A failed check never advances progress (global constraint): still on lesson 4.
    assert!(out(&tx(home.path(), &["tutor", "status"])).contains("▶  4. 04-first-route"));
    tx(home.path(), &["tutor", "goto", "2"]);
    let passed = tx(home.path(), &["tutor", "check"]);
    assert_eq!(passed.status.code(), Some(0), "{}", out(&passed));
    assert!(out(&passed).contains("✓ GET / answers 200"));
    assert!(out(&passed).contains("Lesson 3/12: 03-sessions"));
    assert!(out(&tx(home.path(), &["tutor", "status"])).contains("✓  2. 02-request-response"));
}

#[test]
fn reset_rewinds_progress() {
    let home = tempfile::tempdir().unwrap();
    started(home.path());
    tx(home.path(), &["tutor", "goto", "6"]);
    let reset = tx(home.path(), &["tutor", "reset"]);
    assert_eq!(reset.status.code(), Some(0), "{}", out(&reset));
    assert!(out(&tx(home.path(), &["tutor", "status"])).contains("▶  1. 01-what-is-a-server"));
}

/// A stale `current` (no longer in the pack) must not be a dead end: `reset` is the way out.
#[test]
fn reset_recovers_from_a_stale_lesson() {
    let home = tempfile::tempdir().unwrap();
    let project = started(home.path());
    let progress = serde_json::json!({"dir": project, "current": "99-gone", "passed": []});
    std::fs::write(home.path().join("tx-home/tutor/python.json"), progress.to_string()).unwrap();
    let status = tx(home.path(), &["tutor", "status"]);
    assert_eq!(status.status.code(), Some(1), "{}", out(&status));
    assert!(out(&status).contains("tx tutor reset"), "{}", out(&status));
    let reset = tx(home.path(), &["tutor", "reset"]);
    assert_eq!(reset.status.code(), Some(0), "{}", out(&reset));
    assert!(out(&tx(home.path(), &["tutor", "status"])).contains("▶  1. 01-what-is-a-server"));
}

/// `start --dir` must refuse to silently split an in-progress tutorial across two directories.
#[test]
fn start_refuses_a_different_dir_than_the_saved_one() {
    let home = tempfile::tempdir().unwrap();
    let project = started(home.path());
    let other = home.path().join("elsewhere");
    let start = tx(
        home.path(),
        &["tutor", "start", "--dir", other.to_str().unwrap()],
    );
    assert_eq!(start.status.code(), Some(1), "{}", out(&start));
    let message = out(&start);
    assert!(message.contains("already in progress"), "{message}");
    assert!(message.contains(&project.display().to_string()), "{message}");
    assert!(message.contains("tx tutor reset --hard"), "{message}");
    assert!(message.contains("tutor/python.json"), "{message}");
    assert!(!other.exists(), "start must not touch the other dir");
}

#[test]
fn the_verb_can_be_switched_off() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("tx-home")).unwrap();
    std::fs::write(home.path().join("tx-home/config.json"), r#"{"plugins": {"verbs.tutor": {"disabled": true}}}"#).unwrap();
    let help = out(&tx(home.path(), &["--help"]));
    assert!(!help.contains("tutor"), "{help}");
}
