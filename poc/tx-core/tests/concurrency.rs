//! Spec verification 2, 3, 4: concurrent create (OS processes) under one asked name, concurrent
//! updates (threads), per-recipient serialization of messages.

mod common;

use std::collections::HashSet;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::time::Duration;

use common::{Harness, ShellStub, agent, cwd_of, typed, wait_for};
use tx_core::{CoreConfig, CoreError, Directories, ListFilter, SendOptions, TxCore};

const CHILD_ENV: &str = "TX_CORE_TEST_CHILD";

/// The child role of the multi-process test: `create:<home>:<name>:<cwd>` → prints
/// `ok <id>` or `err <variant>`.
#[test]
fn child_entry() {
    let Ok(spec) = std::env::var(CHILD_ENV) else {
        return;
    };
    let parts: Vec<&str> = spec.splitn(4, ':').collect();
    assert_eq!(parts[0], "create");
    let mut core = TxCore::open(CoreConfig::new(Directories::under(parts[1]))).unwrap();
    core.register_engine(Box::new(ShellStub)).unwrap();
    match core.create_session(agent(parts[2], std::path::Path::new(parts[3]))) {
        Ok(id) => println!("\nTXC-VERDICT ok {id}"),
        Err(CoreError::NameTaken(_)) => println!("\nTXC-VERDICT err NameTaken"),
        Err(other) => println!("\nTXC-VERDICT err {other:?}"),
    }
}

/// Eight processes ask for the name `shared` at once: the create lock hands each its own name
/// (`shared`, `shared-2`, …), no record is lost and no name is given twice.
#[test]
fn concurrent_create_from_eight_processes_yields_eight_distinct_names() {
    require_tmux!();
    let h = Harness::new();
    h.open();
    let cwd = h.repository("w");
    let exe = std::env::current_exe().unwrap();
    let children: Vec<_> = (0..8)
        .map(|_| {
            Command::new(&exe)
                .args(["--exact", "child_entry", "--nocapture", "--test-threads=1"])
                .env(
                    CHILD_ENV,
                    format!("create:{}:shared:{}", h.home().display(), cwd.display()),
                )
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut ok = 0;
    let mut taken = 0;
    let mut other = Vec::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout
            .lines()
            .find_map(|line| line.strip_prefix("TXC-VERDICT "))
            .unwrap_or_else(|| panic!("no verdict in {stdout:?}"))
            .to_owned();
        if line.starts_with("ok ") {
            ok += 1;
        } else if line == "err NameTaken" {
            taken += 1;
        } else {
            other.push(line);
        }
    }
    assert!(other.is_empty(), "{other:?}");
    assert_eq!((ok, taken), (8, 0));
    let mut names = h.live_names();
    names.sort();
    let mut expected: Vec<String> = (1..=8)
        .map(|suffix| {
            if suffix == 1 {
                "shared".to_owned()
            } else {
                format!("shared-{suffix}")
            }
        })
        .collect();
    expected.sort();
    assert_eq!(names, expected);
    let core = h.open();
    assert_eq!(core.list_sessions(&ListFilter::default()).unwrap().len(), 8);
    assert_eq!(h.log_lines().len(), 8);
}

#[test]
fn concurrent_tag_and_rename_lose_no_update() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core
        .create_session(agent("target", &h.repository("w")))
        .unwrap();
    let barrier = Arc::new(Barrier::new(4));
    let handles: Vec<_> = (0..4)
        .map(|worker| {
            let barrier = Arc::clone(&barrier);
            let config = h.config();
            let id = id.clone();
            std::thread::spawn(move || {
                let core = TxCore::open(config).unwrap();
                barrier.wait();
                for i in 0..25 {
                    if worker % 2 == 0 {
                        core.set_session_tags(&id, vec![format!("w{worker}-{i}")])
                            .unwrap();
                    } else {
                        core.rename_session(&id, &format!("n{worker}-{i}")).unwrap();
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    let session = core.get_session(&id).unwrap();
    assert!(
        session.name == "n1-24" || session.name == "n3-24",
        "{}",
        session.name
    );
    assert!(
        session.tags == ["w0-24"] || session.tags == ["w2-24"],
        "{:?}",
        session.tags
    );
    assert_eq!(h.live_names(), std::slice::from_ref(&session.name));
    let log = h.log_lines();
    let tagged = log.iter().filter(|l| l["type"] == "tagged").count();
    let renamed = log.iter().filter(|l| l["type"] == "renamed").count();
    assert_eq!((tagged, renamed), (50, 50));
    let temp_files = std::fs::read_dir(h.data_dir().join("sessions"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        })
        .count();
    assert_eq!(temp_files, 0);
}

#[test]
fn parallel_senders_never_interleave_inside_one_recipient() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let id = core
        .create_session(agent("inbox", &h.repository("w")))
        .unwrap();
    let cwd = cwd_of(&core, &id);
    assert!(wait_for(Duration::from_secs(5), || cwd
        .join("env.txt")
        .exists()));
    let senders = 6;
    let per_sender = 8;
    let barrier = Arc::new(Barrier::new(senders));
    let handles: Vec<_> = (0..senders)
        .map(|worker| {
            let barrier = Arc::clone(&barrier);
            let config = h.config();
            let id = id.clone();
            std::thread::spawn(move || {
                let core = TxCore::open(config).unwrap();
                barrier.wait();
                for i in 0..per_sender {
                    let body = format!("{worker}:{i}:{}", "x".repeat(150));
                    core.send_message(&id, &body, SendOptions::default())
                        .unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    assert!(wait_for(Duration::from_secs(20), || typed(&cwd)
        .lines()
        .count()
        == senders * per_sender));
    let mut seen = HashSet::new();
    for line in typed(&cwd).lines() {
        let inner = line
            .strip_prefix("<from-user>")
            .and_then(|l| l.strip_suffix("</from-user>"))
            .unwrap_or_else(|| panic!("torn line: {line:?}"));
        let mut parts = inner.splitn(3, ':');
        let (w, i, pad) = (
            parts.next().unwrap(),
            parts.next().unwrap(),
            parts.next().unwrap(),
        );
        assert_eq!(pad, "x".repeat(150), "{line:?}");
        assert!(
            seen.insert((w.to_owned(), i.to_owned())),
            "duplicate {line:?}"
        );
    }
    assert_eq!(seen.len(), senders * per_sender);
    assert_eq!(
        h.log_lines()
            .iter()
            .filter(|l| l["type"] == "message-sent")
            .count(),
        senders * per_sender
    );
}

#[test]
fn refresh_all_never_marks_a_freshly_created_session_exited() {
    require_tmux!();
    let h = Harness::new();
    let core = h.open_with_engine();
    let cwd = h.repository("w");
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let refresher = {
        let stop = Arc::clone(&stop);
        let config = h.config();
        std::thread::spawn(move || {
            let core = TxCore::open(config).unwrap();
            let mut wrongly_exited = Vec::new();
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                for session in core.refresh_all().unwrap() {
                    wrongly_exited.push(session.name);
                }
            }
            wrongly_exited
        })
    };
    let mut ids = Vec::new();
    for i in 0..24 {
        ids.push(core.create_session(agent(&format!("s{i}"), &cwd)).unwrap());
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let wrongly_exited = refresher.join().unwrap();
    assert!(
        wrongly_exited.is_empty(),
        "marked exited while live: {wrongly_exited:?}"
    );
    assert_eq!(h.live_names().len(), 24);
    for id in &ids {
        assert_eq!(
            core.get_session(id).unwrap().lifecycle,
            tx_core::Lifecycle::Running
        );
    }
}
