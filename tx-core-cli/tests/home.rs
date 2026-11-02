//! The home plugin through the binary: the floor (`_home-install`, `_home-tui`, `_home-perform`),
//! prefix+m, hidden items.

mod common;

use std::process::{Command, Output};

use common::Cli;

fn outer(cli: &Cli, args: &[&str]) -> Output {
    Command::new("tmux")
        .arg("-S")
        .arg(cli.outer_sock())
        .args(["-f", "/dev/null"])
        .args(args)
        .env_remove("TMUX")
        .output()
        .unwrap()
}

fn press(cli: &Cli, keys: &[&str]) {
    let mut args = vec!["send-keys", "-t", "=outer:"];
    args.extend_from_slice(keys);
    assert!(outer(cli, &args).status.success());
}

/// What the outer terminal shows: the IDE's client, with any popup drawn over it.
fn screen(cli: &Cli) -> String {
    String::from_utf8_lossy(&outer(cli, &["capture-pane", "-p", "-t", "=outer:"]).stdout)
        .into_owned()
}

fn clients(cli: &Cli) -> Vec<String> {
    let mut clients: Vec<String> = cli
        .tmux_out(&["list-clients", "-F", "#{client_session}"])
        .lines()
        .map(str::to_owned)
        .collect();
    clients.sort();
    clients
}

/// `ui install`, then a server started from its conf with every fragment loaded. Whatever the
/// server runs (the floor's TUI, a warm it spawns) sees the suite's Claude config directory.
fn installed_server(cli: &Cli, yaml: &str) {
    cli.write_yaml(yaml);
    cli.ok(&["ui", "install"]);
    let started = Command::new("tmux")
        .arg("-S")
        .arg(cli.sock())
        .arg("-f")
        .arg(cli.state().join("tmux.conf"))
        .args(["new-session", "-d", "-s", "probe", "sleep", "600"])
        .env_remove("TMUX")
        .env("CLAUDE_CONFIG_DIR", cli.claude_config_dir())
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    assert!(
        cli.wait_for(5, || cli
            .tmux_out(&["list-keys", "-T", "prefix", "m"])
            .contains("_home-open")),
        "prefix+m never bound"
    );
}

const NO_ASSISTANT: &str = "plugins:\n  assistant: { enabled: false }\n";

#[test]
fn ui_install_binds_prefix_m_only() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    let bound = cli.tmux_out(&["list-keys", "-T", "prefix", "m"]);
    assert!(bound.contains("run-shell -b"), "{bound}");
    assert!(bound.contains("_home-open"), "{bound}");
    assert!(bound.contains("#{pane_id}"), "{bound}");
    assert!(bound.contains("#{client_name}"), "{bound}");
    let hook = cli.tmux_out(&["show-hooks", "-g", "client-attached"]);
    assert!(!hook.contains("_home"), "{hook}");
    assert!(
        std::fs::read_to_string(cli.state().join("keys/.installed"))
            .unwrap()
            .lines()
            .any(|line| line == "home\tkeys=m\tunset=\tunhook=")
    );

    cli.write_yaml("plugins:\n  assistant: { enabled: false }\n  home: { enabled: false }\n");
    cli.ok(&["ui", "install"]);
    assert!(
        !cli.tmux_out(&["list-keys", "-T", "prefix", "m"])
            .contains("_home-open")
    );
}

/// prefix+m from the home floor: the same screen in a popup, which `q` closes.
#[test]
fn prefix_m_opens_the_home_popup_and_q_closes_it() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    cli.create("a");
    floor(&cli);
    assert!(cli.wait_for(5, || home_shown(&cli)), "{}", screen(&cli));
    let popup_shown = || screen(&cli).contains("q close");
    press(&cli, &["C-a", "m"]);
    assert!(cli.wait_for(5, popup_shown), "no popup:\n{}", screen(&cli));
    press(&cli, &["q"]);
    assert!(cli.wait_for(5, || !popup_shown()), "q did not close it:\n{}", screen(&cli));
    assert!(home_shown(&cli), "{}", screen(&cli));
}

#[test]
fn home_outside_tx_ide_says_where_to_run_it() {
    let cli = Cli::new();
    let stderr = cli.fails(&["home"], 1);
    assert!(stderr.contains("prefix+m"), "{stderr}");
    cli.fails(&["home", "x"], 2);
    cli.fails(&["_home-open", "%1"], 2);
    cli.fails(&["_home-tui", "%1", "c"], 2);
    cli.fails(&["_home-tui"], 2);
    cli.fails(&["_home-tui", "%1", "--choice"], 2);
    cli.fails(&["_home-install"], 2);
    cli.fails(&["_home-perform"], 2);
}

/// The home TUI fills the outer terminal: its header on the first line, its footer last (it
/// says `enter resume` while the cursor is on an ended agent).
fn home_shown(cli: &Cli) -> bool {
    let shown = screen(cli);
    let lines: Vec<&str> = shown
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    lines.first().is_some_and(|line| line.contains("tx-ide"))
        && lines
            .last()
            .is_some_and(|line| line.contains("enter attach") || line.contains("enter resume"))
}

/// `text` without its escape sequences.
fn plain(text: &str) -> String {
    let mut plain = String::new();
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\x1b' {
            plain.push(character);
            continue;
        }
        if characters.next() == Some('[') {
            for code in characters.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
        }
    }
    plain
}

/// Whether `line` turns reverse video on: an SGR sequence with parameter 7 (the parameters
/// of an extended colour, `38;5;n` / `38;2;r;g;b`, are skipped).
fn is_reversed(line: &str) -> bool {
    line.split("\x1b[").skip(1).any(|sequence| {
        let Some(end) = sequence.find(|c: char| !(c.is_ascii_digit() || c == ';')) else {
            return false;
        };
        if !sequence[end..].starts_with('m') {
            return false;
        }
        let mut parameters = sequence[..end].split(';');
        while let Some(parameter) = parameters.next() {
            match parameter {
                "7" => return true,
                "38" | "48" | "58" => {
                    let skip = if parameters.next() == Some("5") { 1 } else { 3 };
                    parameters.nth(skip - 1);
                }
                _ => {}
            }
        }
        false
    })
}

/// The outer terminal's reversed line: the home's cursor row.
fn selected(cli: &Cli) -> String {
    let shown = outer(cli, &["capture-pane", "-p", "-e", "-t", "=outer:"]);
    String::from_utf8_lossy(&shown.stdout)
        .lines()
        .find(|line| is_reversed(line))
        .map(plain)
        .unwrap_or_default()
}

/// A home view made by hand, as an older install left it: its pane runs `program`.
fn hand_made_home(cli: &Cli, program: &str) {
    assert!(
        cli.tmux(&[
            "new-session",
            "-d",
            "-s",
            "home",
            "-x",
            "120",
            "-y",
            "40",
            program
        ])
        .status
        .success()
    );
    for (option, value) in [("@tx-view", "1"), ("@tx-view-kind", "home")] {
        assert!(
            cli.tmux(&["set-option", "-t", "=home:", option, value])
                .status
                .success()
        );
    }
}

fn home_pane(cli: &Cli) -> String {
    cli.tmux_out(&["display-message", "-p", "-t", "=home:", "#{pane_id}"])
}

fn home_command(cli: &Cli) -> String {
    cli.tmux_out(&[
        "display-message",
        "-p",
        "-t",
        "=home:",
        "#{pane_current_command}",
    ])
}

/// The client attached to `session`.
fn client_on(cli: &Cli, session: &str) -> String {
    cli.tmux_out(&["list-clients", "-F", "#{client_session}\t#{client_name}"])
        .lines()
        .find_map(|line| {
            line.split_once('\t')
                .filter(|(attached, _)| *attached == session)
                .map(|(_, client)| client.to_owned())
        })
        .unwrap_or_default()
}

/// The outer terminal attached to `home`.
fn attach_home(cli: &Cli) {
    let sock = cli.sock().display().to_string();
    cli.in_outer_pane(&format!("TMUX= tmux -S {sock} attach -t =home"));
}

/// A shell home turned into the floor, the outer terminal attached to it, the TUI drawn.
fn floor(cli: &Cli) {
    hand_made_home(cli, "sh");
    cli.ok(&["_home-install", "home"]);
    attach_home(cli);
    assert!(
        cli.wait_for(10, || home_shown(cli)),
        "no home TUI:\n{}",
        screen(cli)
    );
}

#[test]
fn home_install_turns_a_shell_home_into_the_tui() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    hand_made_home(&cli, "sh");
    assert_eq!(cli.ok(&["_home-install", "home"]), "");
    assert!(
        cli.wait_for(5, || home_command(&cli) == "tx"),
        "{}",
        home_command(&cli)
    );
    let pane = cli.tmux_out(&["display-message", "-p", "-t", "=home:", "#{pane_id}"]);
    let hooks = cli.tmux_out(&["show-hooks", "-p", "-t", &pane]);
    assert!(hooks.contains("pane-died"), "{hooks}");
    assert!(hooks.contains("_home-perform"), "{hooks}");
    assert_eq!(
        cli.tmux_out(&["show-options", "-pv", "-t", &pane, "remain-on-exit"]),
        "on"
    );
    // A second install leaves the running TUI alone.
    let pid = cli.tmux_out(&["display-message", "-p", "-t", "=home:", "#{pane_pid}"]);
    cli.ok(&["_home-install", "home"]);
    assert_eq!(
        cli.tmux_out(&["display-message", "-p", "-t", "=home:", "#{pane_pid}"]),
        pid
    );
}

#[test]
fn an_editor_in_the_home_pane_is_left_alone() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    hand_made_home(&cli, "sleep 600");
    cli.ok(&["_home-install", "home"]);
    assert!(!cli.wait_for(1, || home_command(&cli) == "tx"));
    assert_eq!(home_command(&cli), "sleep");
}

/// Enter on a row nests the session into the home pane; detaching that nested client brings
/// the TUI back with the row selected. The view's name carries characters tmux formats and
/// shells treat specially (agent names cannot).
#[test]
fn enter_nests_into_the_picked_session_and_detach_returns_to_the_tui() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    cli.create("a");
    let odd = "v#1 it's";
    assert!(
        cli.tmux(&["new-session", "-d", "-s", odd, "sleep", "600"])
            .status
            .success()
    );
    for (option, value) in [("@tx-view", "1"), ("@tx-view-kind", "shell")] {
        assert!(
            cli.tmux(&["set-option", "-t", &format!("={odd}:"), option, value])
                .status
                .success()
        );
    }
    floor(&cli);
    assert!(
        cli.wait_for(5, || screen(&cli).contains(odd)),
        "{}",
        screen(&cli)
    );
    for _ in 0..5 {
        if selected(&cli).contains(odd) {
            break;
        }
        press(&cli, &["j"]);
        cli.wait_for(1, || selected(&cli).contains(odd));
    }
    assert!(selected(&cli).contains(odd), "{}", screen(&cli));
    press(&cli, &["Enter"]);
    assert!(
        cli.wait_for(10, || clients(&cli) == ["home", odd]),
        "{:?}\n{}",
        clients(&cli),
        screen(&cli)
    );
    assert!(
        cli.tmux(&["detach-client", "-t", &client_on(&cli, odd)])
            .status
            .success()
    );
    assert!(
        cli.wait_for(10, || home_shown(&cli) && selected(&cli).contains(odd)),
        "{}\nselected: {:?}",
        screen(&cli),
        selected(&cli)
    );
    assert_eq!(clients(&cli), ["home"]);
}

/// The picker's Enter replaces the TUI with a nested attach and a shell; once that shell is
/// exited, the TUI is back.
#[test]
fn a_picker_enter_then_exit_returns_to_the_tui() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    cli.create("a");
    floor(&cli);
    let sock = cli.sock().display().to_string();
    let nest = format!("bash -c 'TMUX= tmux -S {sock} attach -t =a; exec sh'");
    assert!(
        cli.tmux(&["respawn-pane", "-k", "-t", &home_pane(&cli), &nest])
            .status
            .success()
    );
    assert!(
        cli.wait_for(10, || clients(&cli) == ["a", "home"]),
        "{:?}",
        clients(&cli)
    );
    assert!(
        cli.tmux(&["kill-session", "-t", "=a"]).status.success(),
        "kill a"
    );
    assert!(
        // macOS's `/bin/sh` runs as `bash`.
        cli.wait_for(10, || ["sh", "bash"].contains(&home_command(&cli).as_str())),
        "{}",
        home_command(&cli)
    );
    press(&cli, &["exit", "Enter"]);
    assert!(
        cli.wait_for(10, || home_shown(&cli)),
        "no TUI after the shell exited:\n{}",
        screen(&cli)
    );
}

#[test]
fn the_first_open_warms_the_assistant_once_and_the_header_follows() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, "");
    let path = cli.fake_claude_path();
    assert!(
        cli.tmux(&["set-environment", "-g", "PATH", &path])
            .status
            .success()
    );
    hand_made_home(&cli, "sh");
    cli.ok(&["_home-install", "home"]);
    cli.ok(&["_home-install", "home"]);
    attach_home(&cli);
    // Starting the assistant (a repository, its worktree, the agent) can take over ten seconds
    // while the other suites load the machine.
    assert!(
        cli.wait_for(30, || cli.ok(&["ls", "--tag", "tx-system"]).lines().count()
            == 1),
        "{}",
        screen(&cli)
    );
    assert!(
        cli.wait_for(10, || screen(&cli).contains("assistant ◐ starting")),
        "{}",
        screen(&cli)
    );
    // The fake claude never runs hooks: report the session start as Claude's hook would.
    let listed = cli.ok(&["ls", "--tag", "tx-system"]);
    let id = listed.split('\t').next().unwrap();
    let started = cli.txi(
        &["hook", "SessionStart"],
        &[("TX_SESSION_ID", id)],
        r#"{"source":"startup"}"#,
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    assert!(
        cli.wait_for(10, || screen(&cli).contains("assistant ● ready")),
        "{}",
        screen(&cli)
    );
    assert_eq!(cli.ok(&["ls", "--tag", "tx-system"]).lines().count(), 1);
}

#[test]
fn x_then_y_stops_the_selected_agent() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    cli.create("b");
    floor(&cli);
    assert!(
        cli.wait_for(5, || selected(&cli).contains(" b ")),
        "{}",
        screen(&cli)
    );
    // Past the first open's grace period, where `x` is ignored.
    std::thread::sleep(std::time::Duration::from_millis(500));
    press(&cli, &["x"]);
    assert!(
        cli.wait_for(5, || screen(&cli).contains("stop b? y/n")),
        "{}",
        screen(&cli)
    );
    press(&cli, &["y"]);
    assert!(
        cli.wait_for(10, || common::state_of(&cli, "b") == "ended"),
        "{}",
        common::state_of(&cli, "b")
    );
    assert!(
        cli.wait_for(10, || home_shown(&cli)
            && screen(&cli)
                .lines()
                .any(|line| line.contains(" b ") && line.contains("ended"))),
        "{}",
        screen(&cli)
    );
}

#[test]
fn the_popup_variant_closes_on_q_and_attach_switches_the_client() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    cli.create("a");
    floor(&cli);
    let client = client_on(&cli, "home");
    let pane = home_pane(&cli);
    let choice = cli.dir.path().join("choice");
    let tui = format!(
        "{} {} _home-tui '{pane}' --popup --choice '{}'",
        env!("CARGO_BIN_EXE_tx"),
        cli.arguments(),
        choice.display()
    );
    // `display-popup` returns once the popup has closed.
    let mut popup = Command::new("tmux")
        .arg("-S")
        .arg(cli.sock())
        .args([
            "display-popup",
            "-c",
            &client,
            "-E",
            "-w",
            "80%",
            "-h",
            "80%",
        ])
        .args(["-b", "rounded", &tui])
        .env_remove("TMUX")
        .spawn()
        .unwrap();
    assert!(
        cli.wait_for(10, || screen(&cli).contains("q close")),
        "{}",
        screen(&cli)
    );
    press(&cli, &["q"]);
    assert!(
        cli.wait_for(5, || !screen(&cli).contains("q close") && home_shown(&cli)),
        "{}",
        screen(&cli)
    );
    assert!(popup.wait().unwrap().success());
    assert!(!choice.exists());

    // prefix+m's verb: Enter on `a` switches the terminal to it; the floor stays behind.
    let mut open = cli
        .command(&["_home-open", &pane, &client], &[])
        .spawn()
        .unwrap();
    assert!(
        cli.wait_for(10, || screen(&cli).contains("q close")
            && selected(&cli).contains(" a ")),
        "{}",
        screen(&cli)
    );
    press(&cli, &["Enter"]);
    assert!(open.wait().unwrap().success());
    assert!(
        cli.wait_for(5, || clients(&cli) == ["a"]),
        "{:?}",
        clients(&cli)
    );
    assert_eq!(home_command(&cli), "tx");
}

/// The first paint's budget is 100 ms on a quiet machine; 500 ms keeps the suite green under load.
#[test]
fn the_tui_paints_within_100ms() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    cli.create("a");
    floor(&cli);
    let pane = home_pane(&cli);
    assert!(
        cli.tmux(&["respawn-pane", "-k", "-t", &pane, "sleep 600"])
            .status
            .success()
    );
    assert!(cli.wait_for(5, || !home_shown(&cli)), "{}", screen(&cli));
    let tui = format!(
        "{} {} _home-tui '{pane}'",
        env!("CARGO_BIN_EXE_tx"),
        cli.arguments()
    );
    let started = std::time::Instant::now();
    assert!(
        cli.tmux(&["respawn-pane", "-k", "-t", &pane, &tui])
            .status
            .success()
    );
    while !home_shown(&cli) {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{}",
            screen(&cli)
        );
    }
    let painted = started.elapsed();
    eprintln!("first paint: {} ms", painted.as_millis());
    assert!(
        painted < std::time::Duration::from_millis(500),
        "{painted:?}"
    );
}
