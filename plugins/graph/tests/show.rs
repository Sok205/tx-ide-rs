//! Enter's "show the agent" on a private tmux server: nested into the launching pane only where
//! the picker would nest.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use tx_client::tmux::Ui;
use tx_graph::run;

struct Server {
    socket: PathBuf,
    _directory: tempfile::TempDir,
}

impl Server {
    fn start() -> Self {
        // Socket paths are short on macOS: keep the directory under /tmp.
        let directory = tempfile::Builder::new()
            .prefix("txg")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = directory.path().join("s");
        let server = Self {
            socket,
            _directory: directory,
        };
        server.tmux(&["-f", "/dev/null", "new-session", "-d", "-s", "home", "sh"]);
        server.tmux(&["set-option", "-t", "=home:", "@tx-view", "1"]);
        server.tmux(&["set-option", "-t", "=home:", "@tx-view-kind", "home"]);
        server.tmux(&["new-session", "-d", "-s", "plain", "sh"]);
        server.tmux(&["new-session", "-d", "-s", "worker", "sleep", "600"]);
        server
    }

    fn tmux(&self, args: &[&str]) -> String {
        let output = Command::new("tmux")
            .arg("-S")
            .arg(&self.socket)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "tmux {args:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn ui(&self) -> Ui {
        Ui::new(self.socket.clone(), "/dev/null".into(), "/dev/null".into())
    }

    fn pane(&self, session: &str) -> String {
        self.tmux(&[
            "display-message",
            "-p",
            "-t",
            &format!("={session}:"),
            "#{pane_id}",
        ])
    }

    fn start_command(&self, pane: &str) -> String {
        self.tmux(&["display-message", "-p", "-t", pane, "#{pane_start_command}"])
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .arg("-S")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
    }
}

#[test]
fn an_agent_no_pane_shows_nests_into_a_home_shell_pane() {
    let server = Server::start();
    let ui = server.ui();
    let home = server.pane("home");
    // The shell must be the pane's current command before it counts as a hosting pane.
    let deadline = Instant::now() + Duration::from_secs(5);
    let shell = |command: &str| ["sh", "bash", "dash", "zsh"].contains(&command);
    while !shell(&server.tmux(&[
        "display-message",
        "-p",
        "-t",
        &home,
        "#{pane_current_command}",
    ])) {
        assert!(Instant::now() < deadline, "the home pane never ran a shell");
        std::thread::sleep(Duration::from_millis(50));
    }
    run::show_agent(&ui, "no-client", Some(&home), "worker").unwrap();
    let command = server.start_command(&home);
    assert!(
        command.contains("attach -t") && command.contains("=worker"),
        "{command}"
    );
}

#[test]
fn elsewhere_it_switches_the_client_and_leaves_the_pane_alone() {
    let server = Server::start();
    let ui = server.ui();
    let plain = server.pane("plain");
    let before = server.start_command(&plain);
    // No client is attached in the test, so the switch itself fails, saying why.
    let error = run::show_agent(&ui, "no-client", Some(&plain), "worker").unwrap_err();
    assert_eq!(error, "no terminal client to move");
    assert_eq!(server.start_command(&plain), before);
}
