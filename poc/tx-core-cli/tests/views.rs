//! The views plugin through the binary: `ui install` and `ui attach`.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output};

use common::Cli;

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
