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
