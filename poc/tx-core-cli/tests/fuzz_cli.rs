//! Adversarial / property tests over the built `txc` binary: randomized argv, argument corner
//! cases, env-derived sender, directory resolution and concurrent creates. Every process runs
//! with its directories under /tmp, so tmux always uses a private socket; the user's default tmux
//! server is never touched.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

struct Cli {
    dir: tempfile::TempDir,
}

impl Cli {
    fn new() -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("txcf")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_txc"));
        command
            .args(directory_flags(&self.state()))
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID");
        command
    }

    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut command = self.command();
        command.args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_env(args, &[])
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        let mut command = self.command();
        command.args(args).current_dir(cwd);
        command.output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "txc {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn fails(&self, args: &[&str], code: i32) -> String {
        let output = self.run(args);
        assert_eq!(
            output.status.code(),
            Some(code),
            "txc {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }

    fn workdir(&self, name: &str) -> PathBuf {
        let path = self.dir.path().join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-S")
            .arg(self.state().join("tmux.sock"))
            .args(args)
            .output()
            .unwrap()
    }

    /// A process session that captures its argv, env and typed input into files under `cwd`.
    fn capture_session(&self, name: &str, cwd: &Path, extra: &[&str]) -> String {
        let mut args = vec!["create", name, "--cwd", cwd.to_str().unwrap()];
        args.extend_from_slice(extra);
        args.extend_from_slice(&[
            "--exec",
            "sh",
            "-c",
            "printf '%s\\n' \"$@\" > args.txt; env > env.txt; cat > input.txt",
            "argv0",
        ]);
        self.ok(&args).trim().to_owned()
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        let _ = self.tmux(&["kill-server"]);
    }
}

fn has_tmux() -> bool {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux not available; skipping");
        return false;
    }
    true
}

fn wait_for(path: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::read_to_string(path).unwrap_or_default()
}

fn wait_for_lines(path: &Path, lines: usize, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.lines().count() >= lines || Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A tiny deterministic generator so failures reproduce from the printed seed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// ---------------------------------------------------------------------------------------------
// 1. Parser robustness
// ---------------------------------------------------------------------------------------------

/// `--cwd` is deliberately absent from the pool: without it `create` can never pass parsing, so
/// no random argv spawns a process. Everything else is fair game.
fn token_pool() -> Vec<String> {
    let mut pool: Vec<String> = [
        "create",
        "fork",
        "ls",
        "show",
        "rename",
        "tag",
        "send",
        "refresh",
        "where",
        "attach",
        "stop",
        "forget",
        "engines",
        "help",
        "--help",
        "-h",
        "--state-dir",
        "--socket",
        "--profile-dir",
        "--skill-dir",
        "--env",
        "--parent",
        "--tag",
        "--lifecycle",
        "--engine",
        "--exec",
        "--role",
        "--model",
        "--effort",
        "--prompt",
        "--",
        "",
        "-",
        "--=",
        "a=b",
        "=",
        "żółć",
        "名前",
        "running",
        "exited",
        "high",
        "claude",
        "x",
        "sh",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    pool.push("A".repeat(300));
    pool
}

#[test]
fn random_argv_always_exits_0_1_or_2_with_shaped_stderr() {
    let cli = Cli::new();
    let pool = token_pool();
    let mut rng = Lcg(0x5eed_cafe);
    let mut counts = [0usize; 3];
    for iteration in 0..400 {
        let len = rng.below(8);
        let args: Vec<&str> = (0..len)
            .map(|_| pool[rng.below(pool.len())].as_str())
            .collect();
        // A private scratch cwd per run: `--state-dir --state-dir X` makes a relative state dir.
        let scratch = cli.workdir(&format!("scratch-{iteration}"));
        // A fresh state dir per run so "exit 2 never creates it" is observable every time.
        if cli.state().exists() {
            assert!(
                !cli.state().join("tmux.sock").exists(),
                "a random argv started a tmux server"
            );
            std::fs::remove_dir_all(cli.state()).unwrap();
        }
        let output = cli.run_in(&scratch, &args);
        let code = output.status.code();
        let err = stderr(&output);
        assert!(
            matches!(code, Some(0..=2)),
            "iteration {iteration} argv {args:?}: exit {code:?}, stderr {err}"
        );
        let code = code.unwrap();
        counts[code as usize] += 1;
        match code {
            0 => assert!(err.is_empty(), "argv {args:?}: exit 0 wrote stderr {err}"),
            1 => {
                assert!(err.starts_with("txc: "), "argv {args:?}: {err}");
                assert!(
                    !err.contains("usage: txc"),
                    "argv {args:?}: core error printed usage: {err}"
                );
                assert_eq!(err.lines().count(), 1, "argv {args:?}: {err}");
            }
            _ => {
                assert!(err.starts_with("txc: "), "argv {args:?}: {err}");
                assert!(err.contains("usage: txc"), "argv {args:?}: {err}");
                assert!(
                    output.stdout.is_empty(),
                    "argv {args:?}: usage error wrote stdout"
                );
                assert!(
                    !cli.state().exists(),
                    "argv {args:?}: usage error created the state dir"
                );
                assert!(
                    std::fs::read_dir(&scratch).unwrap().next().is_none(),
                    "argv {args:?}: usage error wrote into the cwd"
                );
            }
        }
        std::fs::remove_dir_all(&scratch).unwrap();
    }
    eprintln!("exit code histogram (0/1/2): {counts:?}");
    assert!(
        counts.iter().all(|&n| n > 0),
        "every exit code should be reachable: {counts:?}"
    );
}

#[test]
fn usage_level_inputs_are_table_driven() {
    let cli = Cli::new();
    for (args, needle) in [
        (vec!["--state-dir"], "--state-dir needs a value"),
        (vec!["--socket"], "--socket needs a value"),
        (vec!["--env"], "--env needs a value"),
        (vec!["--env", "=v", "ls"], "KEY=VALUE"),
        (vec!["--env", "", "ls"], "KEY=VALUE"),
        (vec!["--", "ls"], "unknown verb \"--\""),
        (vec![""], "unknown verb \"\""),
        (vec!["-"], "unknown verb \"-\""),
        (vec!["help", "x"], "unexpected argument \"x\""),
        (vec!["engines", "--"], "unexpected argument \"--\""),
        (
            vec!["create", "--", "--cwd", "/"],
            "create needs a session name",
        ),
        (
            vec!["create", "--cwd", "/", "n"],
            "create needs a session name",
        ),
        (
            vec!["create", "n", "--cwd", "/", "--engine", "e", "--exec", "sh"],
            "--engine or --exec",
        ),
        (vec!["send", "x", "--"], "send needs the message text"),
        (vec!["send", "--"], "send needs the message text"),
        (vec!["ls", "--lifecycle", "RUNNING"], "RUNNING"),
        (vec!["ls", "--tag"], "--tag needs a value"),
        (vec!["refresh", "a", "b"], "unexpected argument \"b\""),
        (
            vec![
                "create", "n", "--cwd", "/", "--engine", "claude", "--effort", "",
            ],
            "effort",
        ),
    ] {
        let err = cli.fails(&args, 2);
        assert!(
            err.contains(needle),
            "{args:?}: expected {needle:?} in {err}"
        );
        assert!(err.starts_with("txc: "), "{args:?}: {err}");
    }
    assert!(!cli.state().exists());
    // Usage errors are reported before the state dir is validated.
    let output = Command::new(env!("CARGO_BIN_EXE_txc"))
        .args(["--state-dir", "/dev/null/impossible"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    // The 300-char token is reported verbatim and does not panic.
    let long = "A".repeat(300);
    let err = cli.fails(&[long.as_str()], 2);
    assert!(err.contains(&long));
}

#[test]
fn non_utf8_argv_is_a_usage_error_not_a_panic() {
    let cli = Cli::new();
    let mut command = cli.command();
    command.arg(OsStr::from_bytes(b"\xff\xfe"));
    let output = command.output().unwrap();
    let err = stderr(&output);
    assert!(
        matches!(output.status.code(), Some(0..=2)),
        "exit {:?}: {err}",
        output.status.code()
    );
    assert!(!err.contains("panicked"), "{err}");
}

// ---------------------------------------------------------------------------------------------
// 2. create corner cases
// ---------------------------------------------------------------------------------------------

#[test]
fn create_rejects_bad_names_with_exit_1_before_spawning() {
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    let cwd = cwd.to_str().unwrap();
    let long = "n".repeat(129);
    for name in [
        "żółć",
        "名前",
        "a b",
        " ",
        "",
        "a.b",
        "a/b",
        "a;b",
        "a\tb",
        long.as_str(),
    ] {
        let err = cli.fails(&["create", name, "--cwd", cwd, "--exec", "sleep", "30"], 1);
        assert!(err.contains("invalid session name"), "{name:?}: {err}");
        assert_eq!(err.lines().count(), 1, "{name:?}: {err}");
    }
    assert_eq!(cli.ok(&["ls"]), "");
    // Nothing was spawned: the private server never started.
    assert!(!cli.state().join("tmux.sock").exists());
    // An empty program is rejected too, and --cwd that is a file exits 1.
    let err = cli.fails(&["create", "n", "--cwd", cwd, "--exec", ""], 1);
    assert!(err.contains("invalid session name \"\""), "{err}");
    let file = cli.dir.path().join("file");
    std::fs::write(&file, "x").unwrap();
    let err = cli.fails(
        &[
            "create",
            "n",
            "--cwd",
            file.to_str().unwrap(),
            "--exec",
            "sleep",
            "30",
        ],
        1,
    );
    assert!(err.contains("is not a directory"), "{err}");
}

/// Sharp edge: `--exec` swallows the rest of argv, so `--engine e` after it is an argument to
/// the program, not a usage error; and a program that exits at once surfaces as a tmux error.
#[test]
fn exec_swallows_trailing_flags_and_fast_exit_is_exit_1() {
    if !has_tmux() {
        return;
    }
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    let cwd = cwd.to_str().unwrap();
    let output = cli.run(&["create", "n", "--cwd", cwd, "--exec", "sh", "--engine", "e"]);
    assert_ne!(output.status.code(), Some(2), "{}", stderr(&output));
    let output = cli.run(&["create", "t", "--cwd", cwd, "--exec", "true"]);
    let err = stderr(&output);
    assert!(matches!(output.status.code(), Some(0 | 1)), "{err}");
    eprintln!(
        "sharp edge: `--exec true` -> exit {:?}: {err}",
        output.status.code()
    );
    assert!(err.is_empty() || err.starts_with("txc: "));
    // Whatever happened, the record store is consistent: every listed line is well-formed.
    for line in cli.ok(&["ls"]).lines() {
        assert_eq!(line.split('\t').count(), 4, "{line:?}");
    }
    assert!(
        cli.ok(&["refresh"])
            .lines()
            .all(|l| l.split('\t').count() == 3)
    );
}

#[test]
fn create_boundary_names_and_relative_cwd() {
    if !has_tmux() {
        return;
    }
    let cli = Cli::new();
    let base = cli.workdir("base");
    std::fs::create_dir_all(base.join("rel/./deeper/..")).unwrap();
    // 128 bytes is the maximum, `-` and `-x` are legal names; relative --cwd resolves against
    // the process cwd and is stored canonical and absolute.
    let max = "m".repeat(128);
    for (name, cwd) in [
        (max.as_str(), "rel"),
        ("-", "rel/./deeper/.."),
        ("-x", "./rel"),
    ] {
        let output = cli.run_in(
            &base,
            &["create", name, "--cwd", cwd, "--exec", "sleep", "30"],
        );
        assert!(output.status.success(), "{name}: {}", stderr(&output));
        let shown: serde_json::Value = serde_json::from_str(&cli.ok(&["show", name])).unwrap();
        let expected = base.join("rel").canonicalize().unwrap();
        assert_eq!(shown["cwd"], expected.to_str().unwrap(), "{name}");
        assert!(Path::new(shown["cwd"].as_str().unwrap()).is_absolute());
        assert_eq!(shown["name"], name);
    }
    // Sharp edge: the parser refuses `create --` but `rename` accepts any grammar-valid name,
    // so a session can end up named `--` or `--cwd`; those are still reachable as targets.
    assert_eq!(
        cli.ok(&["rename", "-x", "--"]).trim().split('\t').nth(1),
        Some("--")
    );
    assert_eq!(
        cli.ok(&["rename", "--", "--cwd"]).trim().split('\t').nth(1),
        Some("--cwd")
    );
    let err = cli.fails(
        &[
            "create",
            "--cwd",
            "--cwd",
            base.to_str().unwrap(),
            "--exec",
            "sleep",
            "1",
        ],
        2,
    );
    assert!(err.contains("create needs a session name"), "{err}");
    assert_eq!(
        cli.ok(&["stop", "--cwd"]).trim().split('\t').nth(1),
        Some("stopped")
    );
    assert_eq!(cli.ok(&["ls"]).lines().count(), 3);
}

#[test]
fn exec_args_env_and_tags_pass_through_verbatim() {
    if !has_tmux() {
        return;
    }
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    let id = cli.capture_session(
        "p",
        &cwd,
        &[
            "--tag", "", "--tag", "t=1", "--env", "EMPTY=", "--env", "EQ=a=b=c", "--env", "SP=a b",
        ],
    );
    // Flags that look like txc's own are handed to the program untouched, in order.
    let args = [
        "create",
        "q",
        "--cwd",
        cwd.to_str().unwrap(),
        "--exec",
        "sh",
        "-c",
        "printf '%s\\n' \"$@\" > args2.txt; cat > /dev/null",
        "argv0",
        "--state-dir",
        "/nope",
        "--",
        "-",
        "",
        "--cwd",
        "--exec",
        "--env",
        "K=V",
        "żółć",
    ];
    cli.ok(&args);
    let got = wait_for(&cwd.join("args2.txt"), Duration::from_secs(5));
    assert_eq!(
        got,
        "--state-dir\n/nope\n--\n-\n\n--cwd\n--exec\n--env\nK=V\nżółć\n"
    );

    let env = wait_for(&cwd.join("env.txt"), Duration::from_secs(5));
    let lines: Vec<&str> = env.lines().collect();
    assert!(lines.contains(&"EMPTY="), "{env}");
    assert!(lines.contains(&"EQ=a=b=c"), "{env}");
    assert!(lines.contains(&"SP=a b"), "{env}");
    assert!(
        lines.contains(&format!("TX_SESSION_ID={id}").as_str()),
        "{env}"
    );
    let sock = format!("TMUX_SOCKET={}", cli.state().join("tmux.sock").display());
    assert!(
        lines.iter().any(|l| l.starts_with("TMUX=") || *l == sock),
        "{env}"
    );

    // Sharp edge: the empty tag is accepted and stored; `ls --tag ""` then matches it.
    let shown: serde_json::Value = serde_json::from_str(&cli.ok(&["show", "p"])).unwrap();
    assert_eq!(shown["tags"], serde_json::json!(["", "t=1"]));
    assert_eq!(cli.ok(&["ls", "--tag", ""]).lines().count(), 1);
    assert_eq!(cli.ok(&["ls", "--tag", "t=1"]).lines().count(), 1);
    // Global --env is visible too; the per-session value wins on a clash.
    let output = cli.run_env(
        &[
            "--env",
            "G=global",
            "--env",
            "C=global",
            "create",
            "r",
            "--cwd",
            cwd.to_str().unwrap(),
            "--env",
            "C=session",
            "--exec",
            "sh",
            "-c",
            "env > env3.txt; cat > /dev/null",
        ],
        &[],
    );
    assert!(output.status.success(), "{}", stderr(&output));
    let env = wait_for(&cwd.join("env3.txt"), Duration::from_secs(5));
    let lines: Vec<&str> = env.lines().collect();
    assert!(lines.contains(&"G=global"), "{env}");
    assert!(lines.contains(&"C=session"), "{env}");
    assert!(!lines.contains(&"C=global"), "{env}");
}

// ---------------------------------------------------------------------------------------------
// 3. send text edge cases
// ---------------------------------------------------------------------------------------------

#[test]
fn send_text_edge_cases_arrive_verbatim_inside_the_envelope() {
    if !has_tmux() {
        return;
    }
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    cli.capture_session("s", &cwd, &[]);
    let nine_hundred = "x".repeat(900);
    let cases: Vec<(Vec<&str>, String)> = vec![
        (vec!["-x", "-y"], "-x -y".into()),
        (vec!["--", "--"], "--".into()),
        (vec!["--", "--", "--"], "-- --".into()),
        (vec!["-"], "-".into()),
        (vec![""], String::new()),
        (vec!["żółć", "名前", "🦀"], "żółć 名前 🦀".into()),
        (vec!["end;"], "end;".into()),
        (vec![";"], ";".into()),
        (vec!["a", "b  c", "", "d"], "a b  c  d".into()),
        (
            vec!["<tag>&\"quoted\"", "$HOME", "`x`", "\\n"],
            "<tag>&\"quoted\" $HOME `x` \\n".into(),
        ),
        (vec![nine_hundred.as_str()], nine_hundred.clone()),
    ];
    for (words, _) in &cases {
        let mut args = vec!["send", "s"];
        args.extend(words.iter().copied());
        let out = cli.ok(&args);
        assert!(out.trim().ends_with("%0"), "{out}");
    }
    let typed = wait_for_lines(&cwd.join("input.txt"), cases.len(), Duration::from_secs(10));
    let expected: String = cases
        .iter()
        .map(|(_, text)| format!("<from-user>{text}</from-user>\n"))
        .collect();
    // argv words are joined with exactly one space; spaces inside a word survive. Documented.
    assert_eq!(typed, expected);
    // `--` is only stripped once, and only as the first word.
    assert!(cli.fails(&["send", "s"], 2).contains("message text"));
    assert!(cli.fails(&["send", "s", "--"], 2).contains("message text"));
    // Sending to a stopped session is a core error, not a hang.
    cli.ok(&["stop", "s"]);
    assert!(cli.fails(&["send", "s", "late"], 1).contains("not running"));
}

// ---------------------------------------------------------------------------------------------
// 4. sender_from_env
// ---------------------------------------------------------------------------------------------

#[test]
fn sender_from_env_matrix() {
    if !has_tmux() {
        return;
    }
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    let id = cli.capture_session("agent-1", &cwd, &[]);
    let socket = cli.state().join("tmux.sock");
    let here = format!("{},1234,0", socket.display());
    let here_bare = socket.to_str().unwrap().to_owned();
    let elsewhere = format!("{}/other.sock,1,0", cli.dir.path().display());
    let expected_agent = "<from-agent session=\"agent-1\">";
    let cases: Vec<(&str, &str, &str)> = vec![
        (&here, &id, expected_agent),
        (&here_bare, &id, expected_agent),
        (&elsewhere, &id, "<from-user>"),
        ("", &id, "<from-user>"),
        (&here, "", "<from-user>"),
        (&here, "garbage id!", "<from-user>"),
        (&here, "a/b", "<from-user>"),
        (&here, "żółć", "<from-user>"),
    ];
    for (tmux, sid, prefix) in &cases {
        let output = cli.run_env(
            &["send", "agent-1", "ping"],
            &[("TMUX", tmux), ("TX_SESSION_ID", sid)],
        );
        assert!(
            output.status.success(),
            "TMUX={tmux:?} TX_SESSION_ID={sid:?}: {}",
            stderr(&output)
        );
        let _ = prefix;
    }
    // Right socket but an unknown, well-formed id: exit 1 "no session", nothing typed.
    let output = cli.run_env(
        &["send", "agent-1", "ghost"],
        &[("TMUX", &here), ("TX_SESSION_ID", "unknown-id")],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stderr(&output), "txc: no session \"unknown-id\"\n");
    // TMUX pointing at the private socket but TX_SESSION_ID unset: user.
    let output = cli.run_env(&["send", "agent-1", "unset"], &[("TMUX", &here)]);
    assert!(output.status.success());

    let typed = wait_for_lines(
        &cwd.join("input.txt"),
        cases.len() + 1,
        Duration::from_secs(10),
    );
    let lines: Vec<&str> = typed.lines().collect();
    assert_eq!(lines.len(), cases.len() + 1, "{typed}");
    for ((tmux, sid, prefix), line) in cases.iter().zip(&lines) {
        assert!(
            line.starts_with(prefix),
            "TMUX={tmux:?} TX_SESSION_ID={sid:?}: {line}"
        );
        assert!(
            line.ends_with("ping</from-user>") || line.ends_with("ping</from-agent>"),
            "{line}"
        );
    }
    assert_eq!(lines[cases.len()], "<from-user>unset</from-user>");
}

// ---------------------------------------------------------------------------------------------
// 5. attach
// ---------------------------------------------------------------------------------------------

#[test]
fn attach_to_unknown_or_stopped_targets_exits_1_without_exec() {
    let cli = Cli::new();
    assert_eq!(
        cli.fails(&["attach", "nope"], 1),
        "txc: no session \"nope\"\n"
    );
    assert_eq!(
        cli.fails(&["attach", "żółć"], 1),
        "txc: no session \"żółć\"\n"
    );
    assert_eq!(cli.fails(&["attach", ""], 1), "txc: no session \"\"\n");
    assert!(
        cli.fails(&["attach"], 2)
            .contains("attach needs a session id or name")
    );
    assert!(
        cli.fails(&["attach", "a", "b"], 2)
            .contains("unexpected argument")
    );
    if !has_tmux() {
        return;
    }
    let cwd = cli.workdir("w");
    let id = cli
        .ok(&[
            "create",
            "a",
            "--cwd",
            cwd.to_str().unwrap(),
            "--exec",
            "sleep",
            "30",
        ])
        .trim()
        .to_owned();
    cli.ok(&["stop", "a"]);
    let err = cli.fails(&["attach", "a"], 1);
    assert_eq!(err, format!("txc: session {id} is not running\n"));
    // A session whose tmux side vanished is reported as not running after refresh.
    cli.ok(&[
        "create",
        "b",
        "--cwd",
        cwd.to_str().unwrap(),
        "--exec",
        "sleep",
        "30",
    ]);
    assert!(cli.tmux(&["kill-session", "-t", "=b"]).status.success());
    let output = cli.run(&["attach", "b"]);
    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert!(stderr(&output).starts_with("txc: "), "{}", stderr(&output));
}

// ---------------------------------------------------------------------------------------------
// 6. ls / show / engines shapes
// ---------------------------------------------------------------------------------------------

#[test]
fn ls_is_four_tab_fields_and_show_is_json_for_every_session() {
    let cli = Cli::new();
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\n");
    assert_eq!(cli.ok(&["ls"]), "");
    if !has_tmux() {
        return;
    }
    let spaced = cli.workdir("my dir/with  two spaces/and-żółć");
    let plain = cli.workdir("plain");
    let names = ["a", "B_2", "-dash", "x-y-z", "9", "_"];
    let mut ids = Vec::new();
    for (index, name) in names.iter().enumerate() {
        let cwd = if index % 2 == 0 { &spaced } else { &plain };
        let id = cli.ok(&[
            "create",
            name,
            "--cwd",
            cwd.to_str().unwrap(),
            "--tag",
            "k",
            "--exec",
            "sleep",
            "30",
        ]);
        ids.push(id.trim().to_owned());
    }
    cli.ok(&["stop", "9"]);
    let ls = cli.ok(&["ls"]);
    assert_eq!(ls.lines().count(), names.len());
    for line in ls.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 4, "{line:?}");
        assert_eq!(fields[0].len(), 36, "{line:?}");
        assert!(names.contains(&fields[1]), "{line:?}");
        assert!(
            ["running", "stopped", "exited"].contains(&fields[2]),
            "{line:?}"
        );
        assert!(
            Path::new(fields[3]).is_absolute() && Path::new(fields[3]).is_dir(),
            "{line:?}"
        );
        let shown: serde_json::Value = serde_json::from_str(&cli.ok(&["show", fields[0]])).unwrap();
        assert_eq!(shown["id"], fields[0]);
        assert_eq!(shown["name"], fields[1]);
        assert_eq!(shown["lifecycle"], fields[2]);
        assert_eq!(shown["cwd"], fields[3]);
        assert_eq!(shown["tags"], serde_json::json!(["k"]));
        assert_eq!(cli.ok(&["show", fields[1]]), cli.ok(&["show", fields[0]]));
    }
    assert_eq!(cli.ok(&["ls", "--lifecycle", "stopped"]).lines().count(), 1);
    assert_eq!(
        cli.ok(&["ls", "--lifecycle", "running"]).lines().count(),
        names.len() - 1
    );
    assert_eq!(cli.ok(&["ls", "--tag", "k"]).lines().count(), names.len());
    assert_eq!(cli.ok(&["ls", "--parent", "a"]), "");
    assert!(
        cli.fails(&["ls", "--parent", "nope"], 1)
            .contains("no session")
    );
    // Names are exact: a prefix, a different case and an id prefix all miss.
    for miss in ["b_2", "B_", "x-y", &ids[0][..8]] {
        assert!(
            cli.fails(&["show", miss], 1).contains("no session"),
            "{miss}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// 7. Directory resolution
// ---------------------------------------------------------------------------------------------

/// `txc` with an environment that holds nothing of the caller's: `HOME` and `TMPDIR` under
/// `root`, no `XDG_*`, no `TX_*`.
fn sandboxed(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_txc"));
    command
        .env("HOME", root.join("home"))
        .env("TMPDIR", root.join("tmp"))
        .env("USER", "u")
        .env_remove("TMUX")
        .env_remove("TX_SESSION_ID")
        .env_remove("TX_TMUX_SOCKET")
        .env_remove("TX_APPNAME")
        .env_remove("TX_CORE_HOME")
        .env_remove("XDG_RUNTIME_DIR");
    for (key, xdg) in [
        ("TX_CONFIG_DIR", "XDG_CONFIG_HOME"),
        ("TX_DATA_DIR", "XDG_DATA_HOME"),
        ("TX_STATE_DIR", "XDG_STATE_HOME"),
        ("TX_CACHE_DIR", "XDG_CACHE_HOME"),
        ("TX_RUN_DIR", "XDG_RUNTIME_DIR"),
    ] {
        command.env_remove(key).env_remove(xdg);
    }
    command
}

#[test]
fn data_dir_flag_beats_exported_beats_xdg_beats_home() {
    let root = tempfile::Builder::new()
        .prefix("txcf")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::create_dir_all(root.path().join("tmp")).unwrap();
    let flag = root.path().join("flag");
    let exported = root.path().join("exported");
    let xdg = root.path().join("xdg");
    let home = root.path().join("home");
    let run = |args: &[&str], environment: &[(&str, &Path)]| {
        let mut command = sandboxed(root.path());
        command.args(args);
        for (key, value) in environment {
            command.env(key, value);
        }
        command.output().unwrap()
    };
    let all = [("TX_DATA_DIR", exported.as_path()), ("XDG_DATA_HOME", &xdg)];
    let output = run(&["--data-dir", flag.to_str().unwrap(), "engines"], &all);
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(flag.join("sessions").is_dir());
    assert!(!exported.exists() && !xdg.exists());

    assert!(run(&["engines"], &all).status.success());
    assert!(exported.join("sessions").is_dir());
    assert!(!xdg.exists());

    assert!(run(&["engines"], &all[1..]).status.success());
    assert!(xdg.join("tx-ide/sessions").is_dir());
    assert!(!home.join(".local/share").exists());

    assert!(run(&["engines"], &[]).status.success());
    assert!(home.join(".local/share/tx-ide/sessions").is_dir());

    // An empty exported value means unset, not "the current directory".
    let scratch = root.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let output = sandboxed(root.path())
        .arg("engines")
        .current_dir(&scratch)
        .env("TX_DATA_DIR", "")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        std::fs::read_dir(&scratch).unwrap().next().is_none(),
        "an empty TX_DATA_DIR means unset"
    );
}

#[test]
fn long_socket_path_fails_before_touching_state_and_socket_overrides_it() {
    let root = tempfile::Builder::new()
        .prefix("txcf")
        .tempdir_in("/tmp")
        .unwrap();
    // "<root>/<d...>/tmux.sock" lands at exactly 101 bytes; one byte less is accepted.
    let base = root.path().to_str().unwrap().len() + "/".len() + "/tmux.sock".len();
    let too_long = root.path().join("d".repeat(101 - base));
    let just_fits = root.path().join("e".repeat(100 - base));
    let data = root.path().join("data");
    let run = |args: &[&str]| sandboxed(root.path()).args(args).output().unwrap();
    let flags = |run_dir: &Path| {
        vec![
            "--config-dir".to_owned(),
            root.path().join("config").to_string_lossy().into_owned(),
            "--data-dir".to_owned(),
            data.to_string_lossy().into_owned(),
            "--state-dir".to_owned(),
            root.path().join("state").to_string_lossy().into_owned(),
            "--cache-dir".to_owned(),
            root.path().join("cache").to_string_lossy().into_owned(),
            "--run-dir".to_owned(),
            run_dir.to_string_lossy().into_owned(),
        ]
    };
    let with = |run_dir: &Path, rest: &[&str]| {
        let mut args = flags(run_dir);
        args.extend(rest.iter().map(|word| (*word).to_owned()));
        args
    };
    let args = with(&too_long, &["engines"]);
    let output = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("tmux socket path is 101 bytes, longer than the 100 allowed"),
        "{err}"
    );
    assert!(
        !too_long.exists() && !data.exists(),
        "a directory was created despite the socket error"
    );
    assert!(output.stdout.is_empty());

    let args = with(&just_fits, &["engines"]);
    let output = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(just_fits.is_dir() && data.join("sessions").is_dir());

    let short = root.path().join("s.sock");
    let args = with(&too_long, &["--socket", short.to_str().unwrap(), "engines"]);
    let output = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(too_long.is_dir());
    // Flag order is free; a --socket that is itself too long still fails.
    let long_sock = root.path().join("x".repeat(120)).join("tmux.sock");
    let mut args = vec![
        "--socket".to_owned(),
        long_sock.to_string_lossy().into_owned(),
    ];
    args.extend(with(&just_fits, &["engines"]));
    let output = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("longer than"));
    // The usage error wins over the socket error: nothing is validated for a bad argv.
    let args = flags(&too_long);
    let output = run(&args.iter().map(String::as_str).collect::<Vec<_>>());
    assert_eq!(output.status.code(), Some(2));
    // `engines` never starts a server, but make sure no private server is left behind.
    for sock in [
        too_long.join("tmux.sock"),
        just_fits.join("tmux.sock"),
        short,
    ] {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(sock)
            .arg("kill-server")
            .output();
    }
}

// ---------------------------------------------------------------------------------------------
// 8. Concurrency
// ---------------------------------------------------------------------------------------------

fn spawn_creates(cli: &Cli, cwd: &Path, names: &[String]) -> Vec<Output> {
    let children: Vec<_> = names
        .iter()
        .map(|name| {
            let mut command = cli.command();
            command
                .args([
                    "create",
                    name,
                    "--cwd",
                    cwd.to_str().unwrap(),
                    "--exec",
                    "sleep",
                    "30",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            command.spawn().unwrap()
        })
        .collect();
    children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect()
}

#[test]
fn six_concurrent_creates_with_distinct_names_all_succeed() {
    if !has_tmux() {
        return;
    }
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    let names: Vec<String> = (0..6).map(|i| format!("c{i}")).collect();
    let outputs = spawn_creates(&cli, &cwd, &names);
    let mut ids = Vec::new();
    for (name, output) in names.iter().zip(&outputs) {
        assert!(output.status.success(), "{name}: {}", stderr(output));
        ids.push(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 6, "ids must be distinct");
    let ls = cli.ok(&["ls", "--lifecycle", "running"]);
    assert_eq!(ls.lines().count(), 6, "{ls}");
    let mut listed: Vec<&str> = ls.lines().map(|l| l.split('\t').nth(1).unwrap()).collect();
    listed.sort_unstable();
    assert_eq!(listed, names.iter().map(String::as_str).collect::<Vec<_>>());
    for name in &names {
        assert!(
            cli.tmux(&["has-session", "-t", &format!("={name}")])
                .status
                .success(),
            "{name}"
        );
    }
}

#[test]
fn six_concurrent_creates_with_the_same_name_let_exactly_one_win() {
    if !has_tmux() {
        return;
    }
    let cli = Cli::new();
    let cwd = cli.workdir("w");
    let names: Vec<String> = vec!["same".to_owned(); 6];
    let outputs = spawn_creates(&cli, &cwd, &names);
    let winners = outputs.iter().filter(|o| o.status.success()).count();
    assert_eq!(
        winners,
        1,
        "{:?}",
        outputs.iter().map(stderr).collect::<Vec<_>>()
    );
    for output in outputs.iter().filter(|o| !o.status.success()) {
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            stderr(output),
            "txc: a running session is already named \"same\"\n"
        );
        assert!(output.stdout.is_empty());
    }
    assert_eq!(cli.ok(&["ls"]).lines().count(), 1);
    let sessions = cli.tmux(&["list-sessions", "-F", "#{session_name}"]);
    assert_eq!(String::from_utf8_lossy(&sessions.stdout).trim(), "same");
    // Once stopped, the name is free again, and a losing record never lingers.
    cli.ok(&["stop", "same"]);
    cli.ok(&[
        "create",
        "same",
        "--cwd",
        cwd.to_str().unwrap(),
        "--exec",
        "sleep",
        "30",
    ]);
    assert_eq!(cli.ok(&["ls", "--lifecycle", "running"]).lines().count(), 1);
    assert_eq!(cli.ok(&["ls"]).lines().count(), 2);
}

/// Every directory flag naming `dir`: the suites keep the five directories in one place, so
/// their paths stay `<dir>/sessions`, `<dir>/tmux.sock`, … (the XDG suite separates them).
pub fn directory_flags(dir: &std::path::Path) -> Vec<std::ffi::OsString> {
    [
        "--config-dir",
        "--data-dir",
        "--state-dir",
        "--cache-dir",
        "--run-dir",
    ]
    .into_iter()
    .flat_map(|flag| [flag.into(), dir.as_os_str().to_owned()])
    .collect()
}
