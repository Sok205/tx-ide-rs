//! Where `tx` keeps its files: the five XDG directories under `tx-ide` (or `$TX_APPNAME`).
//! Every process runs in a sandbox: `HOME`, `TMPDIR` and
//! every directory live under a temp root, and no `XDG_*` or `TX_*` of the caller leaks in.
//! Agents run a fake `claude` (a shim that just stays alive).

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use common::{repository_at, shim_path_in};

struct Sandbox {
    root: tempfile::TempDir,
    /// PATH with the fake `claude` first.
    path: String,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("txx")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::create_dir_all(root.path().join("tmp")).unwrap();
        let path = shim_path_in(&root.path().join("shim"), &[("claude", "exec sleep 30")]);
        Self { root, path }
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn home(&self) -> PathBuf {
        self.path().join("home")
    }

    fn command(&self, environment: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tx"));
        command
            .env("HOME", self.home())
            .env("TMPDIR", self.path().join("tmp"))
            .env("USER", "u")
            .env("PATH", &self.path)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID")
            .env_remove("TX_TMUX_SOCKET")
            .env_remove("TX_APPNAME");
        for key in [
            "TX_CONFIG_DIR",
            "TX_DATA_DIR",
            "TX_STATE_DIR",
            "TX_CACHE_DIR",
            "TX_RUN_DIR",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "XDG_RUNTIME_DIR",
        ] {
            command.env_remove(key);
        }
        for (key, value) in environment {
            command.env(key, value);
        }
        command
    }

    fn run(&self, args: &[&str], environment: &[(&str, &str)]) -> Output {
        self.command(environment).args(args).output().unwrap()
    }

    fn ok(&self, args: &[&str], environment: &[(&str, &str)]) -> String {
        let output = self.run(args, environment);
        assert!(
            output.status.success(),
            "tx {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// A git repository with one commit at `<root>/work`: an agent's starting checkout.
    fn repository(&self) -> PathBuf {
        repository_at(&self.path().join("work"))
    }

    /// Stand-ins for the roles `ui install` writes, where it writes them for `application`, so
    /// a `create` without `--role` (the default `agent.roles`) resolves.
    fn install_default_roles(&self, application: &str) {
        let roles = self
            .home()
            .join(".local/share")
            .join(application)
            .join("roles");
        std::fs::create_dir_all(&roles).unwrap();
        for role in ["COMMON", "DEVELOPER"] {
            std::fs::write(roles.join(format!("{role}.md")), "stand-in role\n").unwrap();
        }
    }

    /// `create NAME --cwd <repository> --engine claude --prompt start`, with the fake `claude`.
    fn create(&self, name: &str, environment: &[(&str, &str)]) -> String {
        self.install_default_roles("tx-ide");
        let work = self.repository();
        self.ok(
            &[
                "create",
                name,
                "--cwd",
                work.to_str().unwrap(),
                "--engine",
                "claude",
                "--prompt",
                "start",
            ],
            environment,
        )
        .trim()
        .to_owned()
    }

    /// An agent whose `claude` writes its environment to `env.txt` in its worktree, then the
    /// parsed lines.
    fn session_environment(&self, environment: &[(&str, &str)]) -> Vec<(String, String)> {
        let application = environment
            .iter()
            .find(|(key, _)| *key == "TX_APPNAME")
            .map_or("tx-ide", |(_, value)| *value);
        self.install_default_roles(application);
        let work = self.repository();
        let path = shim_path_in(
            &self.path().join("probe-shim"),
            &[("claude", "env > env.txt; exec sleep 30")],
        );
        let mut environment = environment.to_vec();
        environment.push(("PATH", path.as_str()));
        self.ok(
            &[
                "create",
                "probe",
                "--cwd",
                work.to_str().unwrap(),
                "--engine",
                "claude",
                "--prompt",
                "start",
            ],
            &environment,
        );
        let shown: serde_json::Value =
            serde_json::from_str(&self.ok(&["show", "probe"], &environment)).unwrap();
        let worktree = PathBuf::from(shown["working_directory"].as_str().unwrap());
        let text = wait_for(&worktree.join("env.txt"));
        text.lines()
            .filter_map(|line| line.split_once('='))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect()
    }
}

fn wait_for(path: &Path) -> String {
    for _ in 0..50 {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("{} never appeared", path.display());
}

fn kill_server(socket: &Path) {
    let _ = Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .arg("kill-server")
        .output();
}

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}

#[test]
fn defaults_are_the_xdg_directories_under_tx_ide() {
    if !tmux_available() {
        return;
    }
    let sandbox = Sandbox::new();
    let id = sandbox.create("a", &[]);
    let home = sandbox.home();
    let socket = sandbox.path().join("tmp/tx-ide.u/tmux.sock");
    let exists = socket.exists();
    kill_server(&socket);
    assert!(
        home.join(format!(".local/share/tx-ide/agents/{id}/agent.json"))
            .is_file()
    );
    assert!(home.join(".local/share/tx-ide/log.jsonl").is_file());
    assert!(home.join(".local/share/tx-ide/tmux.conf").is_file());
    assert!(home.join(".local/state/tx-ide/locks/create.lock").is_file());
    assert!(home.join(".local/state/tx-ide/profiles").is_dir());
    assert!(exists, "{} missing", socket.display());
    assert!(!home.join(".tx-core").exists());
}

#[test]
fn a_session_carries_every_directory_and_the_application_name() {
    if !tmux_available() {
        return;
    }
    let sandbox = Sandbox::new();
    let environment = sandbox.session_environment(&[("TX_APPNAME", "tx-qa-13")]);
    let socket = sandbox.path().join("tmp/tx-qa-13.u/tmux.sock");
    kill_server(&socket);
    let get = |key: &str| {
        environment
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| panic!("{key} not exported"))
    };
    let home = sandbox.home();
    let tmp = sandbox.path().join("tmp");
    for (key, expected) in [
        ("TX_CONFIG_DIR", home.join(".config/tx-qa-13")),
        ("TX_DATA_DIR", home.join(".local/share/tx-qa-13")),
        ("TX_STATE_DIR", home.join(".local/state/tx-qa-13")),
        ("TX_CACHE_DIR", home.join(".cache/tx-qa-13")),
        ("TX_RUN_DIR", tmp.join("tx-qa-13.u")),
        ("TX_TMUX_SOCKET", socket.clone()),
    ] {
        assert_eq!(PathBuf::from(get(key)), expected, "{key}");
    }
    assert_eq!(get("TX_APPNAME"), "tx-qa-13");
    assert!(
        environment
            .iter()
            .filter(|(key, _)| key.starts_with("TX_"))
            .all(|(_, value)| !value.contains("tx-ide")),
        "{environment:?}"
    );
    assert!(!home.join(".local/share/tx-ide").exists());
}

#[test]
fn xdg_variables_move_each_directory() {
    let sandbox = Sandbox::new();
    let x = sandbox.path().join("x");
    let y = sandbox.path().join("y");
    let run = sandbox.path().join("r");
    sandbox.ok(
        &["engines"],
        &[
            ("XDG_DATA_HOME", x.to_str().unwrap()),
            ("XDG_STATE_HOME", y.to_str().unwrap()),
            ("XDG_RUNTIME_DIR", run.to_str().unwrap()),
        ],
    );
    assert!(x.join("tx-ide/agents").is_dir());
    assert!(y.join("tx-ide/locks").is_dir());
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(run.join("tx-ide")).unwrap().permissions(),
    );
    assert_eq!(mode & 0o777, 0o700);
    assert!(!sandbox.home().exists());
}

#[test]
fn an_invalid_application_name_is_refused_before_any_file() {
    let sandbox = Sandbox::new();
    for bad in ["../escape", "a b", "..", "x/y"] {
        let output = sandbox.run(&["engines"], &[("TX_APPNAME", bad)]);
        assert_eq!(output.status.code(), Some(1), "{bad:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("TX_APPNAME must match"), "{stderr}");
    }
    assert!(!sandbox.home().exists());
    // Empty means unset.
    sandbox.ok(&["engines"], &[("TX_APPNAME", "")]);
    assert!(sandbox.home().join(".local/share/tx-ide").is_dir());
}

/// With no flags, the config file is `$XDG_CONFIG_HOME/<app>/config.yaml`, else
/// `~/.config/<app>/config.yaml`.
#[test]
fn the_config_file_lives_in_the_config_directory() {
    let sandbox = Sandbox::new();
    let write = |directory: &Path| {
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(
            directory.join("config.yaml"),
            "plugins:\n  engine-codex: { enabled: false }\n",
        )
        .unwrap();
    };
    write(&sandbox.home().join(".config/tx-ide"));
    assert_eq!(sandbox.ok(&["engines"], &[]), "claude\n");
    let xdg = sandbox.path().join("xdg-config");
    assert_eq!(
        sandbox.ok(&["engines"], &[("XDG_CONFIG_HOME", xdg.to_str().unwrap())]),
        "claude\ncodex\n"
    );
    write(&xdg.join("tx-ide"));
    assert_eq!(
        sandbox.ok(&["engines"], &[("XDG_CONFIG_HOME", xdg.to_str().unwrap())]),
        "claude\n"
    );
}
