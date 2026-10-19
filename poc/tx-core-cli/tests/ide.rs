//! The ide plugin through the binary: `ui install` and `ui attach`.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
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

fn exe() -> PathBuf {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_txc")).unwrap()
}

fn expected_conf(cli: &Cli) -> String {
    let state = cli.state();
    let state = state.display();
    format!(
        "# tx-core server config, written by `txc ui install`. Re-run to update.\n\
         set -s exit-empty on\n\
         set -g history-limit 50000\n\
         set -g detach-on-destroy off\n\
         set -g @tx-bin \"{}\"\n\
         set -g @tx-state \"{state}\"\n\
         source-file \"{state}/conf/tmux.conf\"\n\
         run-shell \"{state}/keys/ide.tmux\"\n\
         run-shell \"{state}/keys/labels.tmux\"\n\
         run-shell \"{state}/keys/kill.tmux\"\n\
         run-shell \"{state}/keys/picker.tmux\"\n\
         run-shell \"{state}/keys/edit.tmux\"\n\
         run-shell \"{state}/keys/spawn.tmux\"\n",
        exe().display(),
    )
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o111 != 0
}

fn installed(cli: &Cli) -> String {
    std::fs::read_to_string(cli.state().join("keys/.installed")).unwrap()
}

/// Starts the server from the installed conf and waits until every key fragment has run.
fn start_server(cli: &Cli) {
    assert!(
        cli.tmux_with_conf(&["new-session", "-d", "-s", "probe", "sleep", "60"])
            .status
            .success()
    );
    assert!(
        cli.wait_for(5, || {
            cli.tmux_out(&["list-keys", "-T", "prefix", "t"])
                .contains("pick")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "X"])
                    .contains("kill")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "e"])
                    .contains("edit")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "s"])
                    .contains("_relabel")
                && cli
                    .tmux_out(&["list-keys", "-T", "prefix", "n"])
                    .contains("tmux-new-agent")
                && cli
                    .tmux_out(&["show-option", "-gv", "pane-border-format"])
                    .contains("_pane-label")
        }),
        "fragments never loaded"
    );
}

fn plugins_yaml_disabling(id: &str) -> String {
    format!("plugins:\n  {id}: {{ enabled: false }}\n")
}

#[test]
fn ui_install_materialises_resources() {
    let cli = Cli::new();
    let out = cli.ok(&["ui", "install"]);
    assert_eq!(out, format!("{}\n", cli.conf().display()));
    let state = cli.state();
    for script in [
        "tmux-kill-tx-session",
        "tmux-edit-tx-session",
        "tmux-nav",
        "tmux-system-resources",
        "tmux-new-agent",
    ] {
        let path = state.join("bin").join(script);
        assert!(path.is_file(), "{path:?}");
        assert!(is_executable(&path), "{path:?}");
    }
    for id in ["ide", "picker", "edit", "kill", "labels", "spawn"] {
        let path = state.join(format!("keys/{id}.tmux"));
        assert!(path.is_file(), "{path:?}");
        assert!(is_executable(&path), "{path:?}");
    }
    assert!(state.join("conf/tmux.conf").is_file());
    for role in [
        "agents/COMMON.md",
        "agents/DEVELOPER.md",
        "agents/TX-ASSISTANT.md",
        "agents/skills/tx-sessions/SKILL.md",
    ] {
        let path = state.join(role);
        assert!(path.is_file(), "{path:?}");
        assert!(!is_executable(&path), "{path:?}");
    }
    assert_eq!(
        std::fs::read_to_string(cli.conf()).unwrap(),
        expected_conf(&cli)
    );
    let installed = installed(&cli);
    for (id, keys) in [
        ("labels", "keys=s"),
        ("kill", "keys=X"),
        ("picker", "keys=t"),
        ("edit", "keys=e"),
        ("spawn", "keys=n"),
    ] {
        assert!(
            installed
                .lines()
                .any(|line| line.starts_with(&format!("{id}\t{keys}"))),
            "{id}: {installed:?}"
        );
    }
    assert!(installed.lines().any(|line| line.starts_with("ide\t")));
}

#[test]
fn ui_install_deletes_files_no_plugin_provides() {
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let stale = [
        cli.state().join("bin/old-script"),
        cli.state().join("keys/old.tmux"),
        cli.state().join("conf/old.conf"),
        cli.state().join("agents/OLD.md"),
    ];
    for path in &stale {
        std::fs::write(path, "x").unwrap();
    }
    cli.ok(&["ui", "install"]);
    for path in &stale {
        assert!(!path.exists(), "{path:?}");
    }
    assert!(cli.state().join("keys/.installed").is_file());
}

#[test]
fn prune_removes_a_symlink_but_never_follows_it() {
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let victim = cli.dir.path().join("victim-dir");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("precious"), "x").unwrap();
    let link = cli.state().join("bin/link");
    std::os::unix::fs::symlink(&victim, &link).unwrap();
    cli.ok(&["ui", "install"]);
    assert!(victim.join("precious").is_file());
    assert!(link.symlink_metadata().is_err(), "link is gone");
}

#[test]
fn a_symlinked_kind_directory_is_refused() {
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let victim = cli.dir.path().join("victim-dir");
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("precious"), "x").unwrap();
    let keys = cli.state().join("keys");
    std::fs::remove_dir_all(&keys).unwrap();
    std::os::unix::fs::symlink(&victim, &keys).unwrap();
    let stderr = cli.fails(&["ui", "install"], 1);
    assert_eq!(
        stderr,
        format!(
            "txc ui install: {} is a symlink; refusing to prune\n",
            keys.display()
        )
    );
    assert!(victim.join("precious").is_file());
    assert_eq!(std::fs::read_dir(&victim).unwrap().count(), 1);
}

#[test]
fn ui_install_takes_no_arguments() {
    let cli = Cli::new();
    for args in [
        &["ui", "install", "--poc", "/tmp"][..],
        &["ui", "install", "x"][..],
    ] {
        assert!(
            cli.fails(args, 2)
                .starts_with("usage: txc ui install | attach")
        );
    }
    let conf = std::fs::read_to_string(cli.conf()).unwrap_or_default();
    assert!(!conf.contains("@tx-bin"), "{conf:?}");
}

#[test]
fn ui_install_fragment_loads() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    start_server(&cli);
    let bound = |table: &str, key: &str| cli.tmux_out(&["list-keys", "-T", table, key]);
    assert!(bound("prefix", "t").contains("pick"));
    assert!(bound("prefix", "X").contains("tmux-kill-tx-session"));
    assert!(bound("prefix", "e").contains("tmux-edit-tx-session"));
    assert!(bound("prefix", "s").contains("_relabel"));
    assert!(bound("root", "C-h").contains("tmux-nav"));
    assert!(
        cli.tmux_out(&["show-option", "-gv", "pane-border-format"])
            .contains("_pane-label")
    );
    assert!(
        cli.tmux_out(&["show-option", "-gv", "status-right"])
            .contains("tmux-system-resources")
    );
    assert_eq!(
        cli.tmux_out(&["show-option", "-gv", "@tx-bin"]),
        exe().to_str().unwrap()
    );
    assert_eq!(
        cli.tmux_out(&["show-option", "-gv", "@tx-state"]),
        cli.state().to_str().unwrap()
    );
    assert!(bound("prefix", "n").contains("tmux-new-agent"));
    cli.tmux(&["kill-server"]);
}

#[test]
fn fragments_do_nothing_outside_an_installed_server() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let other = cli.outer_sock();
    let on_other = |args: &[&str]| {
        Command::new("tmux")
            .arg("-S")
            .arg(&other)
            .args(["-f", "/dev/null"])
            .args(args)
            .env_remove("TMUX")
            .output()
            .unwrap()
    };
    assert!(
        on_other(&["new-session", "-d", "-s", "other", "sleep", "60"])
            .status
            .success()
    );
    for id in ["ide", "labels", "kill", "picker", "edit", "spawn"] {
        let output = Command::new(cli.state().join(format!("keys/{id}.tmux")))
            .env("TMUX", format!("{},1,0", other.display()))
            .output()
            .unwrap();
        assert!(output.status.success(), "{id}: {output:?}");
    }
    let bound = on_other(&["list-keys", "-T", "prefix", "X"]);
    let root = on_other(&["list-keys", "-T", "root", "C-h"]);
    on_other(&["kill-server"]);
    assert!(bound.stdout.is_empty(), "{bound:?}");
    assert!(root.stdout.is_empty(), "{root:?}");
}

#[test]
fn disabling_kill_removes_its_key_on_reload() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    start_server(&cli);
    cli.write_yaml(&plugins_yaml_disabling("kill"));
    let out = cli.ok(&["ui", "install"]);
    assert_eq!(out, format!("{}\nreloaded\n", cli.conf().display()));
    let kill = cli.tmux_out(&["list-keys", "-T", "prefix", "X"]);
    assert!(!kill.contains("tmux-kill-tx-session"), "{kill:?}");
    assert!(!cli.state().join("keys/kill.tmux").exists());
    assert!(!cli.state().join("bin/tmux-kill-tx-session").exists());
    assert!(
        !installed(&cli)
            .lines()
            .any(|line| line.starts_with("kill\t"))
    );
    assert!(
        !std::fs::read_to_string(cli.conf())
            .unwrap()
            .contains("kill")
    );
    // The others survive the reload.
    assert!(cli.wait_for(5, || {
        cli.tmux_out(&["list-keys", "-T", "prefix", "t"])
            .contains("pick")
    }));
    assert!(
        cli.tmux_out(&["list-keys", "-T", "prefix", "e"])
            .contains("tmux-edit-tx-session")
    );
}

#[test]
fn disabling_labels_unsets_border_format_on_reload() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    start_server(&cli);
    assert!(
        cli.tmux_out(&["show-hooks", "-g", "after-new-window"])
            .contains("@tx-view")
    );
    cli.write_yaml(&plugins_yaml_disabling("labels"));
    let out = cli.ok(&["ui", "install"]);
    assert!(out.contains("reloaded"), "{out:?}");
    assert!(!out.contains("legacy"), "{out:?}");
    let format = cli.tmux_out(&["show-option", "-gv", "pane-border-format"]);
    assert!(!format.contains("_pane-label"), "{format:?}");
    // An unset hook is listed by name only, with no command behind it.
    let hook = cli.tmux_out(&["show-hooks", "-g", "after-new-window"]);
    assert!(!hook.contains("@tx-view"), "{hook:?}");
    let relabel = cli.tmux_out(&["list-keys", "-T", "prefix", "s"]);
    assert!(!relabel.contains("_relabel"), "{relabel:?}");
    // The border look stays: it belongs to ide.
    assert_eq!(
        cli.tmux_out(&["show-option", "-gv", "pane-border-lines"]),
        "heavy"
    );
}

#[test]
fn legacy_hint_when_no_installed_file() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    start_server(&cli);
    let fresh = cli.ok(&["ui", "install"]);
    assert!(!fresh.contains("legacy"), "{fresh:?}");
    std::fs::remove_file(cli.state().join("keys/.installed")).unwrap();
    assert!(
        cli.tmux(&["set-option", "-g", "@tx-bin", exe().to_str().unwrap()])
            .status
            .success()
    );
    let out = cli.ok(&["ui", "install"]);
    assert_eq!(
        out,
        format!(
            "{}\nreloaded\nlegacy bindings may remain; restart the server to drop them\n",
            cli.conf().display()
        )
    );
    assert!(cli.state().join("keys/.installed").is_file());
}

#[test]
fn no_legacy_hint_when_the_server_never_loaded_an_install_conf() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    start_server(&cli);
    std::fs::remove_file(cli.state().join("keys/.installed")).unwrap();
    assert!(
        cli.tmux(&["set-option", "-gu", "@tx-bin"]).status.success(),
        "unset @tx-bin"
    );
    let out = cli.ok(&["ui", "install"]);
    assert_eq!(out, format!("{}\nreloaded\n", cli.conf().display()));
    assert!(cli.state().join("keys/.installed").is_file());
}

#[test]
fn reload_unbinds_a_key_a_fragment_stopped_declaring() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    start_server(&cli);
    assert!(
        cli.tmux(&["bind-key", "F12", "display-message", "bogus"])
            .status
            .success()
    );
    assert!(
        cli.tmux_out(&["list-keys", "-T", "prefix", "F12"])
            .contains("bogus")
    );
    let path = cli.state().join("keys/.installed");
    let text = installed(&cli);
    assert!(text.contains("kill\tkeys=X\t"), "{text:?}");
    std::fs::write(&path, text.replace("kill\tkeys=X\t", "kill\tkeys=X F12\t")).unwrap();
    let out = cli.ok(&["ui", "install"]);
    assert!(out.contains("reloaded"), "{out:?}");
    let f12 = cli.tmux_out(&["list-keys", "-T", "prefix", "F12"]);
    assert!(f12.is_empty(), "{f12:?}");
    assert!(
        installed(&cli).contains("kill\tkeys=X\t"),
        "{:?}",
        installed(&cli)
    );
    assert!(cli.wait_for(5, || {
        cli.tmux_out(&["list-keys", "-T", "prefix", "X"])
            .contains("tmux-kill-tx-session")
    }));
}

#[test]
fn no_reload_and_no_hint_without_a_server() {
    let cli = Cli::new();
    assert_eq!(
        cli.ok(&["ui", "install"]),
        format!("{}\n", cli.conf().display())
    );
}

#[test]
fn resource_verb_prints_bytes_and_fails_on_unknown() {
    let cli = Cli::new();
    let output = cli.run(&["_resource", "conf/tmux.conf"], &[], "");
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        output.stdout,
        include_bytes!("../../plugins/ide/conf/tmux.conf")
    );
    let output = cli.run(&["_resource", "bin/tmux-nav"], &[], "");
    assert!(output.stdout.starts_with(b"#!/usr/bin/env bash"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("@tx-ide-nav-keys"));
    let output = cli.run(&["_resource", "nope/x"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "txc _resource: no such resource: nope/x\n"
    );
    cli.fails(&["_resource"], 2);
    cli.write_yaml(&plugins_yaml_disabling("kill"));
    let stderr = cli.fails(&["_resource", "keys/kill.tmux"], 1);
    assert!(stderr.contains("no such resource"), "{stderr:?}");
}

#[test]
fn ui_install_refuses_unsafe_paths() {
    let cli = Cli::new();
    let state = cli.dir.path().join("with space");
    let output = Command::new(env!("CARGO_BIN_EXE_txc"))
        .arg("--state-dir")
        .arg(&state)
        .args(["ui", "install"])
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        format!(
            "txc ui install: path contains unsafe characters: {}\n",
            state.display()
        )
    );
    let conf = std::fs::read_to_string(state.join("tmux.conf")).unwrap_or_default();
    assert!(!conf.contains("@tx-bin"), "{conf:?}");
    assert!(!state.join("bin").exists());
    assert!(!state.join("keys").exists());
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
    cli.write_yaml(&plugins_yaml_disabling("assistant"));
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
    cli.write_yaml(&plugins_yaml_disabling("assistant"));
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
    cli.write_yaml(&plugins_yaml_disabling("assistant"));
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
fn ui_attach_starts_the_assistant_and_nests_home_into_it() {
    require_tmux!();
    let cli = Cli::new();
    cli.ok(&["ui", "install"]);
    let path = cli.fake_claude_path();
    cli.in_outer_pane(&format!("PATH='{path}' {}", attach_command(&cli)));
    let clients = || {
        let mut clients: Vec<String> = cli
            .tmux_out(&["list-clients", "-F", "#{client_session}"])
            .lines()
            .map(str::to_owned)
            .collect();
        clients.sort();
        clients
    };
    assert!(
        cli.wait_for(10, || clients() == ["home", "tx-assistant"]),
        "clients: {:?}",
        clients()
    );
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["show", "tx-assistant"])).unwrap();
    assert_eq!(json["lifecycle"], "running");
    assert_eq!(json["tags"], serde_json::json!(["tx-system"]));
    assert_eq!(
        json["cwd"],
        cli.state().canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(
        json["launch"]["profile"],
        serde_json::json!(["COMMON", "TX-ASSISTANT"])
    );
    assert_eq!(
        cli.tmux_out(&["show-options", "-vt", "=home:", "@tx-view"]),
        "1"
    );
}

#[test]
fn ui_attach_without_the_roles_installed_opens_a_plain_home() {
    require_tmux!();
    let cli = Cli::new();
    let path = cli.fake_claude_path();
    let output = cli.run(
        &["ui", "attach"],
        &[("PATH", &path), ("TMUX", "/tmp/not-the-core.sock,1,0")],
        "",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("COMMON"), "{stderr:?}");
    assert!(cli.tmux(&["has-session", "-t", "=home"]).status.success());
    assert_eq!(
        cli.run(&["show", "tx-assistant"], &[], "").status.code(),
        Some(1)
    );
}
