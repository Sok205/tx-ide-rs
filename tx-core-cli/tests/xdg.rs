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

    /// `tx` in the sandbox with no flag at all: every directory and both plugin roots are the
    /// defaults `environment` leads to.
    fn bare(&self, environment: &[(&str, &str)]) -> Command {
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
            "TX_BIN",
            "TX_PLUGIN_PATH",
            "TX_PROFILE_PATH",
            "TX_SKILL_PATH",
            "TX_CONFIG_FILE",
            "TX_SESSION_ENV",
            "TX_PLUGIN_TRACE",
        ] {
            command.env_remove(key);
        }
        for (key, value) in environment {
            command.env(key, value);
        }
        command
    }

    /// The config directory `environment` leads to: a sandbox's own plugins live under it.
    fn config_directory(&self, environment: &[(&str, &str)]) -> PathBuf {
        let get = |key: &str| {
            environment
                .iter()
                .find(|(name, value)| *name == key && !value.is_empty())
                .map(|(_, value)| *value)
        };
        let application = get("TX_APPNAME").unwrap_or("tx-ide");
        match (get("TX_CONFIG_DIR"), get("XDG_CONFIG_HOME")) {
            (Some(directory), _) => PathBuf::from(directory),
            (None, Some(base)) => Path::new(base).join(application),
            (None, None) => self.home().join(".config").join(application),
        }
    }

    /// The plugin roots the suites run with: the shared root of the shipped bundles, then the
    /// sandbox's own `<config>/plugins`.
    fn plugin_dirs(&self, environment: &[(&str, &str)]) -> Vec<PathBuf> {
        vec![
            common::shipped().to_path_buf(),
            self.config_directory(environment).join("plugins"),
        ]
    }

    fn command(&self, environment: &[(&str, &str)]) -> Command {
        let mut command = self.bare(environment);
        command.args(common::plugin_flags(&self.plugin_dirs(environment)));
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

    /// Stand-ins for the roles `tx update` writes, where it writes them for `application`, so
    /// a `create` without `--role` (the default definition's `roles`) resolves.
    fn install_default_roles(&self, application: &str) {
        let roles = self
            .home()
            .join(".local/share")
            .join(application)
            .join("roles");
        std::fs::create_dir_all(&roles).unwrap();
        std::fs::write(roles.join("COMMON.md"), "stand-in role\n").unwrap();
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
            &[(
                "claude",
                "env > env.txt.tmp && mv env.txt.tmp env.txt; exec sleep 30",
            )],
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
    common::stand_in_definition(&sandbox.home().join(".config/tx-ide"));
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
    assert!(home.join(".cache/tx-ide/plugins-index.json").is_file());
    assert!(exists, "{} missing", socket.display());
    assert!(!home.join(".tx-core").exists());
}

#[test]
fn a_session_carries_every_directory_and_the_application_name() {
    if !tmux_available() {
        return;
    }
    let sandbox = Sandbox::new();
    common::stand_in_definition(&sandbox.home().join(".config/tx-qa-13"));
    let application = [("TX_APPNAME", "tx-qa-13")];
    let environment = sandbox.session_environment(&application);
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
    // The `tx` that launched it, for the command lines its plugins build.
    assert_eq!(
        PathBuf::from(get("TX_BIN")),
        Path::new(env!("CARGO_BIN_EXE_tx"))
    );
    // The plugin roots it was given, so that a `tx` run inside finds the same plugins.
    let roots: Vec<String> = sandbox
        .plugin_dirs(&application)
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect();
    assert_eq!(get("TX_PLUGIN_PATH"), roots.join(":"));
    assert!(roots[1].ends_with("/.config/tx-qa-13/plugins"), "{roots:?}");
    // Nothing of the default application leaks in. `TX_BIN` and the shipped root are paths of
    // this build, which may hold that name by chance.
    assert!(
        environment
            .iter()
            .filter(|(key, _)| {
                key.starts_with("TX_") && key != "TX_BIN" && key != "TX_PLUGIN_PATH"
            })
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
    let k = sandbox.path().join("k");
    let run = sandbox.path().join("r");
    sandbox.ok(
        &["engines"],
        &[
            ("XDG_DATA_HOME", x.to_str().unwrap()),
            ("XDG_STATE_HOME", y.to_str().unwrap()),
            ("XDG_CACHE_HOME", k.to_str().unwrap()),
            ("XDG_RUNTIME_DIR", run.to_str().unwrap()),
        ],
    );
    assert!(x.join("tx-ide/agents").is_dir());
    assert!(y.join("tx-ide/locks").is_dir());
    assert!(k.join("tx-ide/plugins-index.json").is_file());
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

/// A plugin `id` with no verb, as the directory `<root>/<id>`.
fn plugin(root: &Path, id: &str) {
    common::write_plugin(
        root,
        id,
        &format!("api: 1\nid: {id}\ndescription: The {id} plugin.\n"),
        &[],
    );
}

/// The ids and origins `tx plugins --json` lists for the installed plugins, in its order.
fn installed(output: &str) -> Vec<(String, String)> {
    let rows: serde_json::Value = serde_json::from_str(output).unwrap();
    rows.as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["id"].as_str().unwrap().to_owned(),
                row["origin"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

/// With no flag and no variable, plugins are the directories of `<data>/shipped`, then of
/// `<config>/plugins`; `TX_PLUGIN_PATH` replaces both, and `--plugin-dir` replaces that.
#[test]
fn the_plugin_roots_default_to_the_shipped_directory_then_the_users_own() {
    let sandbox = Sandbox::new();
    let home = sandbox.home();
    plugin(&home.join(".local/share/tx-ide/shipped"), "zebra");
    plugin(&home.join(".config/tx-ide/plugins"), "alpha");
    let elsewhere = sandbox.path().join("elsewhere");
    plugin(&elsewhere, "third");
    let flagged = sandbox.path().join("flagged");
    plugin(&flagged, "fourth");
    let list = |command: &mut Command| {
        let output = command.args(["plugins", "--json"]).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        installed(&String::from_utf8_lossy(&output.stdout))
    };
    let pair = |id: &str, origin: &str| (id.to_owned(), origin.to_owned());
    assert_eq!(
        list(&mut sandbox.bare(&[])),
        [pair("zebra", "shipped"), pair("alpha", "user")]
    );
    let variable = [("TX_PLUGIN_PATH", elsewhere.to_str().unwrap())];
    assert_eq!(
        list(&mut sandbox.bare(&variable)),
        [pair("third", "shipped")]
    );
    assert_eq!(
        list(
            sandbox
                .bare(&variable)
                .arg("--plugin-dir")
                .arg(&flagged)
                .arg("--plugin-dir")
                .arg(&elsewhere)
        ),
        [pair("fourth", "shipped"), pair("third", "user")]
    );
}
