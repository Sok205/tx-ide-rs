//! `txc tutor`: the scaffold, the `tutor-<lang>` view, progress and lesson checks.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

use common::{Cli, lists_verb};

/// A test harness with stand-in `nvim` and `claude` (they just stay alive), the real `git`,
/// `tmux` and `python3` behind them.
struct Tutor {
    cli: Cli,
    path: String,
}

impl Tutor {
    fn new() -> Self {
        let cli = Cli::new();
        let path = cli.shim_path(&[("nvim", "exec sleep 60"), ("claude", "exec sleep 60")]);
        Self { cli, path }
    }
    fn project(&self) -> PathBuf {
        self.cli.dir.path().join("project")
    }
    fn run(&self, args: &[&str], stdin: &str) -> Output {
        self.cli.run(args, &[("PATH", self.path.as_str())], stdin)
    }
    fn ok(&self, args: &[&str], stdin: &str) -> String {
        let output = self.run(args, stdin);
        assert!(
            output.status.success(),
            "txc {args:?}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    /// `(exit code, stdout, stderr)`.
    fn code(&self, args: &[&str], stdin: &str) -> (Option<i32>, String, String) {
        let output = self.run(args, stdin);
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
    fn start(&self) -> String {
        let project = self.project();
        self.ok(
            &[
                "tutor",
                "start",
                "python",
                "--dir",
                project.to_str().unwrap(),
                "--detach",
            ],
            "",
        )
    }
    fn progress(&self) -> serde_json::Value {
        let path = self.cli.state().join("plugins/tutor/python.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }
    fn create(&self, args: &[&str]) {
        let cwd = self.project();
        let mut argv = vec!["create"];
        argv.extend_from_slice(&args[..1]);
        argv.extend(["--cwd", cwd.to_str().unwrap()]);
        argv.extend_from_slice(&args[1..]);
        self.ok(&argv, "");
    }
}

fn python3_available() -> bool {
    Command::new("python3").arg("--version").output().is_ok()
}

#[test]
fn help_lists_tutor() {
    let cli = Cli::new();
    let help = cli.ok(&["help"]);
    assert!(lists_verb(&help, "tutor"), "{help}");
    assert!(help.contains("tutor start [LANG]"), "{help}");
}

#[test]
fn misuse_is_exit_2() {
    let cli = Cli::new();
    for args in [&["tutor"][..], &["tutor", "fly"], &["tutor", "goto"]] {
        let stderr = cli.fails(args, 2);
        assert!(stderr.starts_with("usage: txc tutor"), "{stderr}");
    }
}

#[test]
fn an_unknown_pack_names_the_available_ones() {
    let cli = Cli::new();
    let stderr = cli.fails(&["tutor", "start", "cobol"], 1);
    assert!(
        stderr.contains("no tutorial pack 'cobol' (available: python)"),
        "{stderr}"
    );
    let stderr = cli.fails(&["tutor", "status", "--lang", "../etc"], 1);
    assert!(stderr.contains("no tutorial pack"), "{stderr}");
}

#[test]
fn every_action_but_start_needs_a_tutorial_in_progress() {
    let cli = Cli::new();
    for action in ["check", "next", "prev", "status", "hint", "reset"] {
        let stderr = cli.fails(&["tutor", action], 1);
        assert!(
            stderr.contains("no python tutorial in progress; run `txc tutor start`"),
            "{action}: {stderr}"
        );
    }
}

#[test]
fn start_refuses_when_a_required_program_is_missing() {
    let cli = Cli::new();
    let shim = cli.shim_path(&[]);
    let dir = shim.split(':').next().unwrap();
    let tmux = String::from_utf8(
        Command::new("sh")
            .args(["-c", "command -v tmux"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    std::os::unix::fs::symlink(tmux.trim(), PathBuf::from(dir).join("tmux")).unwrap();
    let path = format!("{dir}:/usr/bin:/bin");
    if Command::new("sh")
        .args(["-c", "command -v nvim"])
        .env("PATH", &path)
        .output()
        .unwrap()
        .status
        .success()
    {
        eprintln!("nvim in /usr/bin or /bin; skipping");
        return;
    }
    let project = cli.dir.path().join("project");
    let output = cli.run(
        &[
            "tutor",
            "start",
            "--dir",
            project.to_str().unwrap(),
            "--detach",
        ],
        &[("PATH", path.as_str())],
        "",
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("missing required program(s): ") && stderr.contains("nvim"),
        "{stderr}"
    );
    assert!(!project.exists(), "nothing scaffolded");
}

/// Gate (adapted from `tx start --tutor python`): `txc tutor start python` scaffolds the
/// project and opens the tutor view.
#[test]
fn tutor_start_python_scaffolds_and_opens_the_tutor_view() {
    require_tmux!();
    let tutor = Tutor::new();
    let out = tutor.start();
    let project = tutor.project();
    assert!(
        out.contains(&format!("Scaffolded {}", project.display())),
        "{out}"
    );
    assert!(out.contains("tutor-python view created."), "{out}");
    assert!(project.join(".tx-tutor").is_file());
    assert!(project.join("server.py").is_file());
    let log = Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(&project)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 1);

    let progress = tutor.progress();
    assert_eq!(progress["dir"], project.to_str().unwrap());
    assert_eq!(progress["current"], "01-what-is-a-server");

    let cli = &tutor.cli;
    assert_eq!(
        cli.tmux_out(&["show-options", "-vt", "=tutor-python:", "@tx-view"]),
        "1"
    );
    let panes = cli.tmux_out(&[
        "list-panes",
        "-t",
        "=tutor-python:",
        "-F",
        "#{pane_current_command}",
    ]);
    assert_eq!(panes.lines().count(), 4, "{panes}");
    let lesson_pane = cli.tmux_out(&[
        "display-message",
        "-p",
        "-t",
        "=tutor-python:.{top-left}",
        "#{pane_start_command}",
    ]);
    assert!(lesson_pane.contains("nvim --listen"), "{lesson_pane}");
    assert!(
        lesson_pane.contains("01-what-is-a-server.md"),
        "{lesson_pane}"
    );
    assert!(
        lesson_pane.contains(&cli.sock().to_string_lossy().into_owned()),
        "lua gets the core's socket: {lesson_pane}"
    );
    let packs = cli.state().join("plugins/tutor/packs");
    assert!(
        packs
            .join("python/lessons/01-what-is-a-server.md")
            .is_file()
    );
    assert!(packs.join("lesson.lua").is_file());
    let keys = std::fs::read_to_string(cli.state().join("plugins/tutor/keys.txt")).unwrap();
    assert!(keys.contains("picker"), "{keys}");
    assert!(!keys.contains("{prefix}"), "{keys}");

    // A second start reuses the project and the view.
    let again = tutor.start();
    assert!(
        !again.contains("Scaffolded") && !again.contains("view created"),
        "{again}"
    );
    let views = cli.tmux_out(&["list-sessions", "-F", "#{session_name}"]);
    assert_eq!(
        views.lines().filter(|name| *name == "tutor-python").count(),
        1
    );
}

#[test]
fn start_elsewhere_while_a_tutorial_is_in_progress_is_refused() {
    require_tmux!();
    let tutor = Tutor::new();
    tutor.start();
    let other = tutor.cli.dir.path().join("other");
    let (code, _, stderr) = tutor.code(
        &[
            "tutor",
            "start",
            "--dir",
            other.to_str().unwrap(),
            "--detach",
        ],
        "",
    );
    assert_eq!(code, Some(1));
    assert!(stderr.contains("already in progress in"), "{stderr}");
    assert!(!other.exists());
}

#[test]
fn start_refuses_a_foreign_non_empty_dir() {
    require_tmux!();
    let tutor = Tutor::new();
    std::fs::create_dir_all(tutor.project()).unwrap();
    std::fs::write(tutor.project().join("mine.txt"), "x").unwrap();
    let project = tutor.project();
    let (code, _, stderr) = tutor.code(
        &[
            "tutor",
            "start",
            "--dir",
            project.to_str().unwrap(),
            "--detach",
        ],
        "",
    );
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("is not empty and not a tx tutor project"),
        "{stderr}"
    );
}

/// Gate: lesson 1's checks pass (its one check is the confirm).
#[test]
fn lesson_1_checks_pass_and_advance() {
    require_tmux!();
    let tutor = Tutor::new();
    tutor.start();
    let (code, out, _) = tutor.code(&["tutor", "check"], "n\n");
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains("✗ you moved between the lesson and the shell"),
        "{out}"
    );
    assert!(out.contains("Not yet"), "{out}");
    assert_eq!(tutor.progress()["current"], "01-what-is-a-server");

    let out = tutor.ok(&["tutor", "check"], "y\n");
    assert!(
        out.contains("Checking lesson 1/13: 01-what-is-a-server"),
        "{out}"
    );
    assert!(out.contains("Did you jump between the two panes"), "{out}");
    assert!(
        out.contains("✓ you moved between the lesson and the shell"),
        "{out}"
    );
    assert!(out.contains("Lesson 2/13: 02-request-response"), "{out}");
    let progress = tutor.progress();
    assert_eq!(progress["current"], "02-request-response");
    assert_eq!(
        progress["passed"],
        serde_json::json!(["01-what-is-a-server"])
    );
}

/// Gate: the session checks pass against a spawned worker (lesson 5: a Claude worker tagged
/// `tutor`), and lesson 3's against a session tagged `http-tutor`.
#[test]
fn session_checks_pass_against_a_spawned_worker() {
    require_tmux!();
    let tutor = Tutor::new();
    tutor.start();
    tutor.ok(&["tutor", "goto", "03"], "");
    let (code, out, _) = tutor.code(&["tutor", "check"], "");
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains("expected at least 1 matching sessions, found 0"),
        "{out}"
    );
    tutor.create(&[
        "server",
        "--tag",
        "http-tutor",
        "--engine",
        "claude",
        "--prompt",
        "start",
    ]);
    let out = tutor.ok(&["tutor", "check"], "");
    assert!(out.contains("✓ server is tagged http-tutor"), "{out}");

    tutor.ok(&["tutor", "goto", "05-spawn-worker"], "");
    let (code, out, _) = tutor.code(&["tutor", "check"], "");
    assert_eq!(
        code,
        Some(1),
        "an agent without the tutor tag is not a worker: {out}"
    );
    tutor.create(&[
        "worker", "--engine", "claude", "--prompt", "start", "--tag", "tutor",
    ]);
    let out = tutor.ok(&["tutor", "check"], "");
    assert!(
        out.contains("✓ a Claude worker tagged tutor exists"),
        "{out}"
    );
    assert!(out.contains("Lesson 6/13"), "{out}");

    // Lesson 13: the worker must be gone and archived.
    tutor.ok(&["tutor", "goto", "13"], "");
    let (code, out, _) = tutor.code(&["tutor", "check"], "");
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains("expected no matching sessions, found: worker"),
        "{out}"
    );
    tutor.ok(&["stop", "worker"], "");
    let out = tutor.ok(&["tutor", "check"], "");
    assert!(
        out.contains("You finished the Python (stdlib http.server) tutorial."),
        "{out}"
    );
}

#[test]
fn http_checks_run_the_pack_server_in_the_project() {
    require_tmux!();
    if !python3_available() {
        eprintln!("python3 not on PATH; skipping");
        return;
    }
    let tutor = Tutor::new();
    tutor.start();
    tutor.ok(&["tutor", "goto", "2"], "");
    let out = tutor.ok(&["tutor", "check"], "");
    assert!(out.contains("✓ GET / answers 200"), "{out}");
    // Lesson 4 needs a /hello route the skeleton does not have yet.
    tutor.ok(&["tutor", "goto", "4"], "");
    let (code, out, _) = tutor.code(&["tutor", "check"], "");
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains("✗ GET /hello answers 200 with 'hello'"),
        "{out}"
    );
}

#[test]
fn navigation_status_and_hint() {
    require_tmux!();
    let tutor = Tutor::new();
    tutor.start();
    assert_eq!(
        tutor.ok(&["tutor", "prev"], ""),
        "Lesson 1/13: 01-what-is-a-server\n"
    );
    assert_eq!(
        tutor.ok(&["tutor", "next"], ""),
        "Lesson 2/13: 02-request-response\n"
    );
    assert_eq!(
        tutor.ok(&["tutor", "goto", "04-first"], ""),
        "Lesson 4/13: 04-first-route\n"
    );
    let status = tutor.ok(&["tutor", "status"], "");
    assert_eq!(status.lines().count(), 13, "{status}");
    assert!(status.contains("▶  4. 04-first-route\n"), "{status}");
    let hint = tutor.ok(&["tutor", "hint"], "");
    let solution = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tutor/python/solutions/04-first-route.py"),
    )
    .unwrap();
    assert_eq!(hint, solution);
    tutor.ok(&["tutor", "goto", "13"], "");
    assert_eq!(
        tutor.ok(&["tutor", "next"], ""),
        "Lesson 13/13: 13-cleanup\n"
    );
    assert!(
        tutor
            .ok(&["tutor", "hint"], "")
            .starts_with("no hint for this lesson")
    );
    let (code, _, stderr) = tutor.code(&["tutor", "goto", "99"], "");
    assert_eq!(code, Some(1));
    assert!(stderr.contains("no lesson '99'"), "{stderr}");
}

#[test]
fn reset_goes_back_to_lesson_1_and_hard_rescaffolds_on_yes() {
    require_tmux!();
    let tutor = Tutor::new();
    tutor.start();
    tutor.ok(&["tutor", "check"], "y\n");
    let server = tutor.project().join("server.py");
    let original = std::fs::read_to_string(&server).unwrap();
    std::fs::write(&server, "broken\n").unwrap();

    assert_eq!(
        tutor.ok(&["tutor", "reset"], ""),
        "Lesson 1/13: 01-what-is-a-server\n"
    );
    assert_eq!(tutor.progress()["passed"], serde_json::json!([]));
    assert_eq!(std::fs::read_to_string(&server).unwrap(), "broken\n");

    let (code, _, _) = tutor.code(&["tutor", "reset", "--hard"], "n\n");
    assert_eq!(code, Some(1));
    assert_eq!(std::fs::read_to_string(&server).unwrap(), "broken\n");
    let out = tutor.ok(&["tutor", "reset", "--hard"], "y\n");
    assert!(out.contains("and start over? [y/N]"), "{out}");
    assert_eq!(std::fs::read_to_string(&server).unwrap(), original);
}

#[test]
fn a_stale_saved_lesson_is_reported_and_reset_recovers() {
    require_tmux!();
    let tutor = Tutor::new();
    tutor.start();
    let path = tutor.cli.state().join("plugins/tutor/python.json");
    let mut progress = tutor.progress();
    progress["current"] = "99-gone".into();
    std::fs::write(&path, progress.to_string()).unwrap();
    let (code, _, stderr) = tutor.code(&["tutor", "status"], "");
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("saved lesson '99-gone' is not in the pack"),
        "{stderr}"
    );
    tutor.ok(&["tutor", "reset"], "");
    assert_eq!(tutor.progress()["current"], "01-what-is-a-server");
}

#[test]
fn a_corrupt_progress_file_is_reported_by_tutor_and_nothing_else() {
    let cli = Cli::new();
    let dir = cli.state().join("plugins/tutor");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("python.json"), "{not json").unwrap();
    let stderr = cli.fails(&["tutor", "status"], 1);
    assert!(stderr.contains("python.json"), "{stderr}");
    cli.ok(&["ls"]);
}

#[test]
fn a_disabled_tutor_has_no_verb() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  tutor: { enabled: false }\n");
    let help = cli.ok(&["help"]);
    assert!(!lists_verb(&help, "tutor"), "{help}");
    cli.fails(&["tutor", "status"], 2);
}
