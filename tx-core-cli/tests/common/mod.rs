//! Shared harness for the CLI suites: one temp dir under /tmp holding all five directories (short socket path), helpers
//! that run `tx` against it, and a kill-server on drop so no private server outlives its test.
//! The shipped plugins run as their own programs here, as in an install: every sandbox names one
//! plugin root linked from this build, [`shipped`], and then its own `<config>/plugins`.
#![allow(dead_code)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

use tx_core::plugin::Manifest;

/// The plugin root every sandbox shares: `<target>/<profile>/tx-shipped/<id>/` for each
/// `plugins/<id>/plugin.yaml` of the workspace, holding links to the manifest, to every resource
/// and to every program a `bin` names (the script `plugins/<id>/bin/<name>` when there is one,
/// else the program `<target>/<profile>/<name>`). Links, so a sandbox pays nothing to have them.
///
/// Cargo builds another package's program only for that package's own tests, so the first call
/// in a test process builds every program of the workspace, in the profile and target directory
/// the test itself was built in. Both steps hold a lock: other test processes link the same
/// directory.
pub fn shipped() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(link_shipped)
}

fn link_shipped() -> PathBuf {
    let binaries = Path::new(env!("CARGO_BIN_EXE_tx")).parent().unwrap();
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let lock = std::fs::File::create(binaries.join("tx-shipped.lock")).unwrap();
    lock.lock().unwrap();

    let mut build = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    build
        .args(["build", "--workspace", "--bins", "--target-dir"])
        .arg(binaries.parent().unwrap())
        .current_dir(workspace);
    match binaries.file_name().unwrap().to_str().unwrap() {
        "debug" => {}
        "release" => {
            build.arg("--release");
        }
        profile => {
            build.args(["--profile", profile]);
        }
    }
    let output = build.output().unwrap();
    assert!(
        output.status.success(),
        "cargo build --workspace --bins: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let root = binaries.join("tx-shipped");
    std::fs::create_dir_all(&root).unwrap();
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(workspace.join("plugins")).unwrap() {
        let source = entry.unwrap().path();
        let file = source.join("plugin.yaml");
        if !file.is_file() {
            continue;
        }
        let manifest = Manifest::read(&file).unwrap_or_else(|error| panic!("{error}"));
        let bundle = root.join(&manifest.id);
        link(&file, &bundle.join("plugin.yaml"));
        for resource in &manifest.resources {
            link(&source.join(resource), &bundle.join(resource));
        }
        for bin in manifest.bins() {
            let script = source.join("bin").join(bin);
            let program = if script.is_file() {
                script
            } else {
                binaries.join(bin)
            };
            link(&program, &bundle.join("bin").join(bin));
        }
        ids.push(manifest.id);
    }
    // A bundle whose manifest is gone (another branch was built here before).
    for entry in std::fs::read_dir(&root).unwrap() {
        let entry = entry.unwrap();
        if !ids.contains(&entry.file_name().to_string_lossy().into_owned()) {
            std::fs::remove_dir_all(entry.path()).unwrap();
        }
    }
    root
}

/// Makes `link` a symbolic link to `target`, in one step and only when it is not one already:
/// a test process running next to this one may be reading it.
fn link(target: &Path, link: &Path) {
    if std::fs::read_link(link).is_ok_and(|current| current == target) {
        return;
    }
    let directory = link.parent().unwrap();
    std::fs::create_dir_all(directory).unwrap();
    let temporary = directory.join(format!(".link-{}", std::process::id()));
    let _ = std::fs::remove_file(&temporary);
    std::os::unix::fs::symlink(target, &temporary).unwrap();
    std::fs::rename(&temporary, link).unwrap();
}

/// `--plugin-dir DIR` for each of `dirs`.
pub fn plugin_flags(dirs: &[PathBuf]) -> Vec<std::ffi::OsString> {
    dirs.iter()
        .flat_map(|dir| ["--plugin-dir".into(), dir.clone().into_os_string()])
        .collect()
}

/// Writes a plugin of a test's own into the plugin root `root`: `<root>/<id>/plugin.yaml` with
/// `manifest`, and each of `programs` (a name and the body of a `#!/bin/sh` script) under its
/// `bin/`. Its directory.
pub fn write_plugin(root: &Path, id: &str, manifest: &str, programs: &[(&str, &str)]) -> PathBuf {
    let directory = root.join(id);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("plugin.yaml"), manifest).unwrap();
    if !programs.is_empty() {
        shim_path_in(&directory.join("bin"), programs);
    }
    directory
}

/// Whether `tx help` lists `verb` on a line of its own: after the indent, the line is the verb
/// or starts with the verb and a space. A bare substring would match flags and prose.
pub fn lists_verb(help: &str, verb: &str) -> bool {
    help.lines().any(|line| {
        let line = line.trim_start();
        line == verb || line.starts_with(&format!("{verb} "))
    })
}

pub fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok()
}

/// Whether `python3` runs: the stand-in engine of `fixtures/engine-fake` is a Python script.
pub fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Copies the directory `from` to `to`, files with their permissions.
fn copy_directory(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Every directory flag naming `dir`: the suites keep the five directories in one place, so
/// their paths stay `<dir>/sessions`, `<dir>/tmux.sock`, … (the XDG suite separates them).
/// A stand-in for the shipped default agent definition, `developer`, under `config_dir`.
pub fn stand_in_definition(config_dir: &std::path::Path) {
    let agents = config_dir.join("harness/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("developer.md"),
        "---\nname: developer\ndescription: stand-in\ntools: Bash, Read, Edit, Write\nroles: [COMMON]\n---\n",
    )
    .unwrap();
}

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

/// The variables core exports into every session, all naming `dir`.
pub fn directory_environment(dir: &std::path::Path) -> Vec<(&'static str, std::path::PathBuf)> {
    [
        "TX_CONFIG_DIR",
        "TX_DATA_DIR",
        "TX_STATE_DIR",
        "TX_CACHE_DIR",
        "TX_RUN_DIR",
    ]
    .into_iter()
    .map(|key| (key, dir.to_path_buf()))
    .collect()
}

pub struct Cli {
    pub dir: tempfile::TempDir,
    /// The plugin roots every `tx` of this sandbox is given, in order.
    pub plugin_dirs: Vec<PathBuf>,
}

/// Stand-ins for the roles `tx update` writes: COMMON, which the stand-in default definition
/// runs on, and REVIEWER, a role of the operator's own for `--role`.
pub const STAND_IN_ROLES: [&str; 2] = ["COMMON", "REVIEWER"];

/// The update steps of this tx counted as run in the sandbox whose directories are all `state`,
/// so no suite sees the "update step pending" warning by accident. The stamp does not count
/// against "fresh". Every constructor of a sandbox calls this.
pub fn stamp_update_steps(state: &std::path::Path) {
    std::fs::create_dir_all(state).unwrap();
    std::fs::write(
        state.join("update-version"),
        format!("{}\n", tx_core::updates::latest()),
    )
    .unwrap();
}

impl Cli {
    /// A sandbox whose plugin roots are the shared [`shipped`] one and then its own
    /// `<config>/plugins`: every shipped plugin runs as its own program.
    pub fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("tx")
            .tempdir_in("/tmp")
            .unwrap();
        // A real install has the default roles (`tx update` writes them); a test that never
        // runs it finds stand-ins here, outside the state dir, below any role it installs.
        let roles = dir.path().join("default-roles");
        std::fs::create_dir_all(&roles).unwrap();
        for role in STAND_IN_ROLES {
            std::fs::write(roles.join(format!("{role}.md")), "stand-in role\n").unwrap();
        }
        // Likewise the default agent definition (`agent.definition: developer`), which a real
        // install ships under `<data>/harness`; `tx update` replaces this stand-in.
        stand_in_definition(&dir.path().join("state"));
        stamp_update_steps(&dir.path().join("state"));
        let plugin_dirs = vec![
            shipped().to_path_buf(),
            dir.path().join("state").join("plugins"),
        ];
        Self { dir, plugin_dirs }
    }
    /// The sandbox's own plugin root, `<config>/plugins`: where a test writes a plugin.
    pub fn plugins_dir(&self) -> PathBuf {
        self.state().join("plugins")
    }
    /// A plugin of this sandbox's own: see [`write_plugin`].
    pub fn plugin(&self, id: &str, manifest: &str, programs: &[(&str, &str)]) -> PathBuf {
        write_plugin(&self.plugins_dir(), id, manifest, programs)
    }
    /// Installs the plugin `tests/fixtures/<id>` into this sandbox's own plugin root: a copy,
    /// so a test may edit it. Its directory.
    pub fn fixture(&self, id: &str) -> PathBuf {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(id);
        let directory = self.plugins_dir().join(id);
        copy_directory(&source, &directory);
        directory
    }
    /// `--profile-dir` for the stand-in default roles, then the state dir's own roles, which win.
    pub fn role_flags(&self) -> Vec<std::ffi::OsString> {
        vec![
            "--profile-dir".into(),
            self.dir.path().join("default-roles").into_os_string(),
            "--profile-dir".into(),
            self.state().join("roles").into_os_string(),
        ]
    }
    pub fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }
    /// The directory flags and the plugin roots, space-separated, as a shell command line
    /// carries them: what `CoreConfig::cli_arguments` gives a core of this sandbox.
    pub fn arguments(&self) -> String {
        directory_flags(&self.state())
            .iter()
            .chain(&plugin_flags(&self.plugin_dirs))
            .map(|word| word.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }
    pub fn write_yaml(&self, text: &str) {
        std::fs::create_dir_all(self.state()).unwrap();
        std::fs::write(self.state().join("config.yaml"), text).unwrap();
    }
    pub fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_tx"));
        command
            .args(directory_flags(&self.state()))
            .args(plugin_flags(&self.plugin_dirs))
            .args(self.role_flags())
            .args(args)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID")
            .env("CLAUDE_CONFIG_DIR", self.claude_config_dir());
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }
    pub fn run(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
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
    /// The plugin roots as one value, the way a session carries them (`TX_PLUGIN_PATH`).
    pub fn plugin_path(&self) -> String {
        self.plugin_dirs
            .iter()
            .map(|dir| dir.to_string_lossy())
            .collect::<Vec<_>>()
            .join(":")
    }
    /// `txi` (the internal binary the wired hooks run) against this state dir, the way a hook
    /// inside a session runs it: the exported `TX_*_DIR` and `TX_PLUGIN_PATH`, no flags.
    pub fn txi(&self, args: &[&str], env: &[(&str, &str)], stdin: &str) -> Output {
        let mut command = self.txi_command(args, env);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
    /// The `txi` of [`Self::txi`] not yet started, for a test that watches it run.
    pub fn txi_command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_txi"));
        command
            .args(args)
            .envs(directory_environment(&self.state()))
            .env("TX_PLUGIN_PATH", self.plugin_path())
            .env_remove("TX_TMUX_SOCKET")
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID");
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }
    pub fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args, &[], "");
        assert!(
            output.status.success(),
            "tx {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }
    pub fn fails(&self, args: &[&str], code: i32) -> String {
        let output = self.run(args, &[], "");
        assert_eq!(
            output.status.code(),
            Some(code),
            "tx {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stderr).into_owned()
    }
    pub fn sock(&self) -> PathBuf {
        self.state().join("tmux.sock")
    }
    /// A second tmux server standing in for "the user's own tmux".
    pub fn outer_sock(&self) -> PathBuf {
        self.dir.path().join("outer.sock")
    }
    /// Run `command` in a pane of the outer server (its `$TMUX` names the outer socket).
    pub fn in_outer_pane(&self, command: &str) {
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
    pub fn wait_for(&self, seconds: u64, mut probe: impl FnMut() -> bool) -> bool {
        for _ in 0..seconds * 10 {
            if probe() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        probe()
    }
    pub fn tmux(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .arg("-S")
            .arg(self.sock())
            .args(args)
            .env_remove("TMUX")
            .output()
            .unwrap()
    }
    pub fn tmux_out(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.tmux(args).stdout)
            .trim()
            .to_owned()
    }
    /// A directory of stand-in executables under `<dir>/shim`, returned as a PATH value that
    /// puts it first.
    pub fn shim_path(&self, shims: &[(&str, &str)]) -> String {
        shim_path_in(&self.dir.path().join("shim"), shims)
    }
    /// A `claude` that just stays alive, so agent sessions can be created without the engine.
    pub fn fake_claude_path(&self) -> String {
        self.shim_path(&[("claude", "exec sleep 60")])
    }
    /// `<dir>/claude-config`, made: `command` sets it as `CLAUDE_CONFIG_DIR` unless a test
    /// overrides it, so trust seeding never reaches the operator's own `~/.claude.json`.
    pub fn claude_config_dir(&self) -> PathBuf {
        let dir = self.dir.path().join("claude-config");
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    /// A git repository with one commit under `<dir>/<name>`: an agent's starting checkout.
    pub fn repository(&self, name: &str) -> PathBuf {
        repository_at(&self.dir.path().join(name))
    }
    /// `<data>/worktrees`, canonical: where every agent's worktree lands.
    pub fn worktrees_dir(&self) -> PathBuf {
        let dir = self.state().join("worktrees");
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }
    /// A Claude agent named `name`, started from a fresh repository `<dir>/<name>` with the
    /// fake `claude` on PATH; its id.
    pub fn create(&self, name: &str) -> String {
        let cwd = self.repository(name);
        let path = self.fake_claude_path();
        let output = self.run(
            &[
                "create",
                name,
                "--cwd",
                cwd.to_str().unwrap(),
                "--engine",
                "claude",
                "--prompt",
                "start",
            ],
            &[("PATH", path.as_str())],
            "",
        );
        assert!(
            output.status.success(),
            "tx create {name}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }
}

/// Stand-in executables (`#!/bin/sh` scripts with the given bodies) written into `dir`, returned
/// as a PATH value that puts `dir` first.
pub fn shim_path_in(dir: &std::path::Path, shims: &[(&str, &str)]) -> String {
    std::fs::create_dir_all(dir).unwrap();
    for (name, body) in shims {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// A git repository with one commit at `repository` (created if missing, left alone if it is one
/// already): an agent's starting checkout, isolated from the host's git config.
pub fn repository_at(repository: &std::path::Path) -> PathBuf {
    if repository.join(".git").exists() {
        return repository.to_path_buf();
    }
    std::fs::create_dir_all(repository).unwrap();
    std::fs::write(repository.join("README.md"), "hello\n").unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "README.md"],
        &["commit", "-q", "-m", "init"],
    ] {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    repository.to_path_buf()
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

#[macro_export]
macro_rules! require_tmux {
    () => {
        if !$crate::common::tmux_available() {
            eprintln!("tmux not available; skipping");
            return;
        }
    };
}

#[macro_export]
macro_rules! require_python3 {
    () => {
        if !$crate::common::python3_available() {
            eprintln!("python3 not available; skipping");
            return;
        }
    };
}

/// The record's `state`: `starting`, `idle`, `working` or `ended`.
pub fn state_of(cli: &Cli, token: &str) -> String {
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["show", token])).unwrap();
    json["state"].as_str().unwrap().to_owned()
}

/// Where a session runs: its record's `working_directory`, the agent's worktree.
pub fn cwd_of(cli: &Cli, token: &str) -> PathBuf {
    let json: serde_json::Value = serde_json::from_str(&cli.ok(&["show", token])).unwrap();
    PathBuf::from(json["working_directory"].as_str().unwrap())
}

/// `<data>/agents/<id>/agent.json`: where the record of `id` lives.
pub fn record_path(cli: &Cli, id: &str) -> PathBuf {
    cli.state().join("agents").join(id).join("agent.json")
}

/// Writes a version-2 record of an ended agent by hand, as a build before this one might have
/// left it: `engine` is any string, so an unknown engine can be planted too.
pub fn plant_record(cli: &Cli, id: &str, name: &str, engine: &str) -> PathBuf {
    let path = record_path(cli, id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 2, "id": id, "name": name, "engine": engine, "working_directory": "/",
            "roles": [], "state": "ended", "created_at": 1.0, "last_modified_at": 2.0,
            "archived_at": null, "tags": [], "parent_id": null,
        })
        .to_string(),
    )
    .unwrap();
    path
}

pub fn pane_id_of(cli: &Cli, name: &str) -> String {
    cli.tmux_out(&[
        "display-message",
        "-p",
        "-t",
        &format!("={name}:"),
        "#{pane_id}",
    ])
}
