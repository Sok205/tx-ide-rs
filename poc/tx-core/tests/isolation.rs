//! Spec verification 8 and 10: the private server never touches the user's tmux, never loads
//! `~/.tmux.conf`; a socket path that would overflow `sun_path` fails before any file exists.

mod common;

use std::process::Command;

use common::{Harness, ShellStub, agent};
use tx_core::{CoreConfig, CoreError, Directories, TxCore};

/// A label on the *default* socket directory that stands in for the user's own server: we must
/// not create sessions on it, and we must not even start it.
const PROBE_LABEL: &str = "tx-core-isolation-probe";

fn probe_server_sessions() -> Option<Vec<String>> {
    let output = Command::new("tmux")
        .args(["-L", PROBE_LABEL, "list-sessions", "-F", "#{session_name}"])
        .output()
        .ok()?;
    output.status.success().then(|| {
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    })
}

#[test]
fn private_server_never_touches_the_default_one_or_the_home_conf() {
    require_tmux!();
    let before = probe_server_sessions();
    let home = tempfile::Builder::new()
        .prefix("txc-home")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::write(
        home.path().join(".tmux.conf"),
        "set -g @probe yes\nset -g status off\n",
    )
    .unwrap();
    let h = Harness::new();
    // Run core in a child process with HOME pointing at the conf, so the test process's own
    // environment stays untouched (and so the check covers the real tmux start path).
    let exe = std::env::current_exe().unwrap();
    let status = Command::new(&exe)
        .args(["--exact", "child_create", "--nocapture", "--test-threads=1"])
        .env("HOME", home.path())
        .env("TMUX_TMPDIR", home.path())
        .env_remove("TMUX")
        .env(
            "TX_CORE_ISOLATION_CHILD",
            format!("{}:{}", h.home().display(), h.repository("w").display()),
        )
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(h.live_names(), ["iso"]);
    let output = h.tmux(&["show-options", "-gv", "@probe"]);
    assert!(
        !output.status.success(),
        "~/.tmux.conf leaked into the private server: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(h.tmux_ok(&["show-options", "-gv", "status"]).trim(), "on");
    assert_eq!(
        probe_server_sessions(),
        before,
        "the default server changed"
    );
    assert!(
        !home.path().join("tmux-".to_owned() + &users_uid()).exists(),
        "a default-socket server was started under the child's TMUX_TMPDIR"
    );
}

fn users_uid() -> String {
    let output = Command::new("id").arg("-u").output().unwrap();
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[test]
fn child_create() {
    let Ok(spec) = std::env::var("TX_CORE_ISOLATION_CHILD") else {
        return;
    };
    let (home, cwd) = spec.split_once(':').unwrap();
    let mut core = TxCore::open(CoreConfig::new(Directories::under(home))).unwrap();
    core.register_engine(Box::new(ShellStub)).unwrap();
    core.create_session(agent("iso", std::path::Path::new(cwd)))
        .unwrap();
}

#[test]
fn overlong_socket_path_fails_before_any_file_exists() {
    let root = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("d".repeat(110));
    let directories = Directories::under(&home);
    let error = TxCore::open(CoreConfig::new(directories.clone())).unwrap_err();
    let CoreError::SocketPathTooLong(path) = error else {
        panic!("{error:?}")
    };
    assert_eq!(path, directories.run.join("tmux.sock"));
    assert!(!home.exists());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    let mut config = CoreConfig::new(directories.clone());
    config.tmux_socket = Some(root.path().join("s.sock"));
    TxCore::open(config).unwrap();
    assert!(directories.data.join("agents").is_dir());
}
