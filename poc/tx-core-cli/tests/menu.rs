//! The menu plugin through the binary: prefix+m, the first-attach hook, hidden items.

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

fn menu_shown(cli: &Cli) -> bool {
    screen(cli).contains("New tx session")
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

/// `ui install`, then a server started from its conf with every fragment loaded.
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
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    assert!(
        cli.wait_for(5, || cli
            .tmux_out(&["list-keys", "-T", "prefix", "m"])
            .contains("_menu-open")),
        "prefix+m never bound"
    );
}

const NO_ASSISTANT: &str = "plugins:\n  assistant: { enabled: false }\n";

#[test]
fn ui_install_binds_prefix_m_and_the_first_attach_hook() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    let bound = cli.tmux_out(&["list-keys", "-T", "prefix", "m"]);
    assert!(bound.contains("run-shell -b"), "{bound}");
    assert!(bound.contains("#{pane_id}"), "{bound}");
    assert!(bound.contains("#{client_name}"), "{bound}");
    let hook = cli.tmux_out(&["show-hooks", "-g", "client-attached"]);
    assert!(hook.contains("_menu-first"), "{hook}");
    assert!(hook.contains("@tx-menu-shown"), "{hook}");
    assert!(
        std::fs::read_to_string(cli.state().join("keys/.installed"))
            .unwrap()
            .lines()
            .any(|line| line == "menu\tkeys=m\tunset=\tunhook=client-attached")
    );

    cli.write_yaml("plugins:\n  assistant: { enabled: false }\n  menu: { enabled: false }\n");
    cli.ok(&["ui", "install"]);
    assert!(
        !cli.tmux_out(&["list-keys", "-T", "prefix", "m"])
            .contains("_menu-open")
    );
    let hook = cli.tmux_out(&["show-hooks", "-g", "client-attached"]);
    assert!(!hook.contains("_menu-first"), "{hook}");
}

/// The operator's first attach: home's pane nests an agent (that nested client attaches first
/// and must not count), the terminal attaches home and gets the menu; a reattach does not.
#[test]
fn the_menu_pops_up_on_the_first_attach_only() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(&cli, NO_ASSISTANT);
    cli.create("a");
    let sock = cli.sock().display().to_string();
    let nest = format!("bash -c 'set -m; TMUX= tmux -S {sock} attach -t =a; exec sh'");
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
            &nest
        ])
        .status
        .success()
    );
    assert!(
        cli.wait_for(5, || clients(&cli) == ["a"]),
        "{:?}",
        clients(&cli)
    );
    assert_eq!(
        cli.tmux_out(&["show-option", "-gqv", "@tx-menu-shown"]),
        "",
        "the nested client is not the first attach"
    );

    cli.in_outer_pane(&format!("TMUX= tmux -S {sock} attach -t =home"));
    assert!(
        cli.wait_for(10, || menu_shown(&cli)),
        "no menu on the first attach:\n{}",
        screen(&cli)
    );
    assert_eq!(cli.tmux_out(&["show-option", "-gv", "@tx-menu-shown"]), "1");
    let shown = screen(&cli);
    assert!(shown.contains("████████╗██╗  ██╗"), "{shown}");
    assert!(
        shown.contains("Fork this session"),
        "home's pane shows the tx session a:\n{shown}"
    );
    press(&cli, &["q"]);
    assert!(
        cli.wait_for(5, || !menu_shown(&cli)),
        "q did not close it:\n{}",
        screen(&cli)
    );

    assert!(
        cli.tmux(&["detach-client", "-s", "home"]).status.success(),
        "detach"
    );
    assert!(
        cli.wait_for(5, || clients(&cli) == ["a"]),
        "{:?}",
        clients(&cli)
    );
    press(
        &cli,
        &[&format!("TMUX= tmux -S {sock} attach -t =home"), "Enter"],
    );
    assert!(
        cli.wait_for(5, || clients(&cli) == ["a", "home"]),
        "{:?}",
        clients(&cli)
    );
    assert!(
        !cli.wait_for(3, || menu_shown(&cli)),
        "a reattach showed the menu again:\n{}",
        screen(&cli)
    );
}

#[test]
fn prefix_m_opens_the_menu_without_disabled_plugins_items() {
    require_tmux!();
    let cli = Cli::new();
    installed_server(
        &cli,
        "plugins:\n  assistant: { enabled: false }\n  timeline: { enabled: false }\n",
    );
    // Not this test's first attach.
    assert!(
        cli.tmux(&["set-option", "-g", "@tx-menu-shown", "1"])
            .status
            .success()
    );
    let sock = cli.sock().display().to_string();
    cli.in_outer_pane(&format!("TMUX= tmux -S {sock} attach -t =probe"));
    assert!(
        cli.wait_for(5, || clients(&cli) == ["probe"]),
        "{:?}",
        clients(&cli)
    );
    assert!(!cli.wait_for(1, || menu_shown(&cli)), "{}", screen(&cli));

    press(&cli, &["C-a", "m"]);
    assert!(
        cli.wait_for(10, || menu_shown(&cli)),
        "prefix+m showed no menu:\n{}",
        screen(&cli)
    );
    let shown = screen(&cli);
    assert!(shown.contains("Attach to running"), "{shown}");
    for hidden in ["Timeline", "Assistant", "Fork this session"] {
        assert!(!shown.contains(hidden), "{hidden}:\n{shown}");
    }
    // A hidden item's key does nothing; the plugins list is read-only and Esc leaves it.
    press(&cli, &["t"]);
    press(&cli, &["p"]);
    assert!(
        cli.wait_for(5, || screen(&cli).contains("disabled")),
        "{}",
        screen(&cli)
    );
    press(&cli, &["Escape"]);
    assert!(cli.wait_for(5, || menu_shown(&cli)), "{}", screen(&cli));
    press(&cli, &["Escape"]);
    assert!(
        cli.wait_for(5, || !menu_shown(&cli)),
        "Esc did not close it:\n{}",
        screen(&cli)
    );
}

#[test]
fn menu_outside_tx_ide_says_where_to_run_it() {
    let cli = Cli::new();
    let stderr = cli.fails(&["menu"], 1);
    assert!(stderr.contains("prefix+m"), "{stderr}");
    cli.fails(&["menu", "x"], 2);
    cli.fails(&["_menu-open", "%1"], 2);
    cli.fails(&["_menu-first"], 2);
    cli.fails(&["_menu-tui", "%1", "c"], 2);
}
