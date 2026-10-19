//! The views plugin through the binary: the picker feed and the session verbs.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}

struct Cli {
    dir: tempfile::TempDir,
}

impl Cli {
    fn new() -> Self {
        Self {
            dir: tempfile::Builder::new()
                .prefix("txc")
                .tempdir_in("/tmp")
                .unwrap(),
        }
    }
    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_txc"));
        command
            .arg("--state-dir")
            .arg(self.state())
            .args(args)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID");
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }
    fn run(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
        let mut child = self
            .command(args, env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args, &[], "");
        assert!(
            output.status.success(),
            "txc {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    fn sock(&self) -> PathBuf {
        self.state().join("tmux.sock")
    }
    /// A second tmux server standing in for "the user's own tmux".
    fn outer_sock(&self) -> PathBuf {
        self.dir.path().join("outer.sock")
    }
    /// Run `command` in a pane of the outer server (its `$TMUX` names the outer socket).
    fn in_outer_pane(&self, command: &str) {
        let outer = |args: &[&str]| {
            Command::new("tmux")
                .arg("-S")
                .arg(self.outer_sock())
                .args(["-f", "/dev/null"])
                .args(args)
                .env_remove("TMUX")
                .output()
                .unwrap()
        };
        assert!(
            outer(&[
                "new-session",
                "-d",
                "-s",
                "outer",
                "-x",
                "120",
                "-y",
                "40",
                "sh"
            ])
            .status
            .success()
        );
        assert!(
            outer(&["send-keys", "-t", "=outer:", command, "Enter"])
                .status
                .success()
        );
    }
    /// Poll `probe` for up to `seconds`.
    fn wait_for(&self, seconds: u64, mut probe: impl FnMut() -> bool) -> bool {
        for _ in 0..seconds * 10 {
            if probe() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        probe()
    }
    fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-S")
            .arg(self.sock())
            .args(args)
            .env_remove("TMUX")
            .output()
            .unwrap()
    }
    fn tmux_out(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.tmux(args).stdout)
            .trim()
            .to_owned()
    }
    fn create(&self, name: &str) -> String {
        let cwd = self.dir.path().join(name);
        std::fs::create_dir_all(&cwd).unwrap();
        self.ok(&[
            "create",
            name,
            "--cwd",
            cwd.to_str().unwrap(),
            "--exec",
            "sleep",
            "60",
        ])
        .trim()
        .to_owned()
    }
}

impl Drop for Cli {
    fn drop(&mut self) {
        for sock in [self.state().join("tmux.sock"), self.outer_sock()] {
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(sock)
                .arg("kill-server")
                .output();
        }
    }
}

macro_rules! require_tmux {
    () => {
        if !tmux_available() {
            eprintln!("tmux not available; skipping");
            return;
        }
    };
}

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

fn lifecycle_of(cli: &Cli, token: &str) -> String {
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["show", token])).unwrap();
    json["lifecycle"].as_str().unwrap().to_owned()
}

#[test]
fn kill_stops_and_reports() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert_eq!(cli.ok(&["kill", "a"]), "Killed 'a'\n");
    assert_eq!(lifecycle_of(&cli, "a"), "stopped");
}

#[test]
fn kill_unknown_exits_1() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    let output = cli.run(&["kill", "nope"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "session 'nope' not found\n"
    );
    assert!(output.stdout.is_empty());
}

#[test]
fn kill_view_home_without_record() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "Views"])
            .status
            .success()
    );
    assert!(
        cli.tmux(&["set-option", "-t", "=Views:", "@tx-view", "1"])
            .status
            .success()
    );
    assert_eq!(cli.ok(&["kill", "Views"]), "Killed 'Views'\n");
    assert!(!cli.tmux(&["has-session", "-t", "=Views"]).status.success());
}

#[test]
fn kill_does_not_touch_plain_tmux_sessions() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "plain"])
            .status
            .success()
    );
    assert_eq!(cli.run(&["kill", "plain"], &[], "").status.code(), Some(1));
    assert!(cli.tmux(&["has-session", "-t", "=plain"]).status.success());
}

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

fn pane_id_of(cli: &Cli, name: &str) -> String {
    cli.tmux_out(&[
        "display-message",
        "-p",
        "-t",
        &format!("={name}:"),
        "#{pane_id}",
    ])
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

impl Cli {
    fn conf(&self) -> PathBuf {
        self.state().join("tmux.conf")
    }
    fn tmux_with_conf(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-S")
            .arg(self.sock())
            .arg("-f")
            .arg(self.conf())
            .args(args)
            .env_remove("TMUX")
            .output()
            .unwrap()
    }
}

#[test]
fn ui_install_writes_conf_and_fragment_loads() {
    require_tmux!();
    let cli = Cli::new();
    let poc = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_txc")).unwrap();
    let out = cli.ok(&["ui", "install", "--poc", poc.to_str().unwrap()]);
    assert_eq!(out, format!("{}\n", cli.conf().display()));
    let poc = poc.display();
    assert_eq!(
        std::fs::read_to_string(cli.conf()).unwrap(),
        format!(
            "# tx-core server config, written by `txc ui install`. Re-run to update.\n\
             set -s exit-empty on\n\
             set -g history-limit 50000\n\
             set -g detach-on-destroy off\n\
             set -g @tx-bin \"{}\"\n\
             set -g @tx-state \"{}\"\n\
             source-file \"{poc}/tmux/tmux.conf\"\n\
             run-shell \"{poc}/tmux/tx-ide.tmux\"\n",
            exe.display(),
            cli.state().display()
        )
    );
    assert!(
        cli.tmux_with_conf(&["new-session", "-d", "-s", "probe", "sleep", "60"])
            .status
            .success()
    );
    let mut bound = String::new();
    for _ in 0..30 {
        bound = String::from_utf8_lossy(
            &cli.tmux_with_conf(&["list-keys", "-T", "prefix", "t"])
                .stdout,
        )
        .into_owned();
        if bound.contains("pick") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(bound.contains("pick"), "{bound:?}");
    let kill =
        String::from_utf8_lossy(&cli.tmux(&["list-keys", "-T", "prefix", "X"]).stdout).into_owned();
    assert!(kill.contains("tmux-kill-tx-session"), "{kill:?}");
    assert_eq!(
        cli.tmux_out(&["show-option", "-gv", "@tx-bin"]),
        exe.to_str().unwrap()
    );
    let new_agent = cli.tmux_out(&["list-keys", "-T", "prefix", "n"]);
    assert!(!new_agent.contains("_new-agent"), "{new_agent:?}");
    cli.tmux(&["kill-server"]);
}

#[test]
fn ui_install_without_fragment_fails_and_writes_nothing() {
    let cli = Cli::new();
    let empty = tempfile::tempdir().unwrap();
    let output = cli.run(
        &["ui", "install", "--poc", empty.path().to_str().unwrap()],
        &[],
        "",
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "txc ui install: no tmux/tx-ide.tmux under {}\n",
            empty.path().display()
        )
    );
    let conf = std::fs::read_to_string(cli.conf()).unwrap_or_default();
    assert!(!conf.contains("@tx-bin"), "{conf:?}");
}

#[test]
fn ui_install_refuses_unsafe_paths() {
    let cli = Cli::new();
    let poc = cli.dir.path().join("with space");
    std::fs::create_dir_all(poc.join("tmux")).unwrap();
    std::fs::write(poc.join("tmux/tx-ide.tmux"), "").unwrap();
    let output = cli.run(&["ui", "install", "--poc", poc.to_str().unwrap()], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "txc ui install: path contains unsafe characters: {}\n",
            std::fs::canonicalize(&poc).unwrap().display()
        )
    );
    let conf = std::fs::read_to_string(cli.conf()).unwrap_or_default();
    assert!(!conf.contains("@tx-bin"), "{conf:?}");
}

#[test]
fn kill_view_home_reports_a_failed_kill() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    assert!(
        cli.tmux(&["new-session", "-d", "-s", "Views"])
            .status
            .success()
    );
    assert!(
        cli.tmux(&["set-option", "-t", "=Views:", "@tx-view", "1"])
            .status
            .success()
    );
    let real =
        String::from_utf8(Command::new("which").arg("tmux").output().unwrap().stdout).unwrap();
    let shim_dir = cli.dir.path().join("shim");
    std::fs::create_dir_all(&shim_dir).unwrap();
    let shim = shim_dir.join("tmux");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do [ \"$a\" = kill-session ] && exit 1; done\nexec {} \"$@\"\n",
            real.trim()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        shim_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = cli.run(&["kill", "Views"], &[("PATH", &path)], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "{:?}", output.stdout);
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "session 'Views' not found\n"
    );
}

fn attach_command(cli: &Cli) -> String {
    format!(
        "{} --state-dir {} ui attach",
        env!("CARGO_BIN_EXE_txc"),
        cli.state().display()
    )
}

#[test]
fn ui_attach_lands_in_a_view_home() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    cli.in_outer_pane(&attach_command(&cli));
    assert!(
        cli.wait_for(5, || cli.tmux_out(&[
            "list-clients",
            "-F",
            "#{client_session}"
        ]) == "home"),
        "no client attached to home"
    );
    let option = |name: &str| cli.tmux_out(&["show-options", "-vt", "=home:", name]);
    assert_eq!(option("@tx-view"), "1");
    assert_eq!(option("status"), "on");
    assert_eq!(
        cli.tmux_out(&["show-window-options", "-vt", "=home:", "pane-border-status"]),
        "top"
    );
}

#[test]
fn ui_attach_inside_another_tmux_says_its_prefix_wins() {
    require_tmux!();
    let cli = Cli::new();
    let output = cli.run(
        &["ui", "attach"],
        &[("TMUX", "/tmp/not-the-core.sock,1,0")],
        "",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.starts_with(
            "txc ui attach: nesting inside another tmux (socket /tmp/not-the-core.sock); its prefix wins \u{2014} press the prefix twice to reach the IDE's keys\n"
        ),
        "{stderr:?}"
    );
    assert!(cli.tmux(&["has-session", "-t", "=home"]).status.success());
}

#[test]
fn ui_attach_inside_the_core_switches_the_client() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("other");
    cli.in_outer_pane(&format!(
        "TMUX= tmux -S {} attach -t =other",
        cli.sock().display()
    ));
    assert!(
        cli.wait_for(5, || cli.tmux_out(&[
            "list-clients",
            "-F",
            "#{client_session}"
        ]) == "other"),
        "client never attached"
    );
    let inside = format!("{},1,0", cli.sock().display());
    let output = cli.run(&["ui", "attach"], &[("TMUX", &inside)], "");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(
        cli.tmux_out(&["list-clients", "-F", "#{client_session}"]),
        "home"
    );
    assert_eq!(
        cli.tmux_out(&["show-options", "-vt", "=home:", "@tx-view"]),
        "1"
    );
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
