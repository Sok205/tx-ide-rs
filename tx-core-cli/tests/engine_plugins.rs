//! Claude and Codex as plugin programs, end to end through the binaries: the same commands in a
//! sandbox where both engines are bundles (every engine call of core goes through the proxy to
//! `tx-engine-<kind>`) and in one where they are compiled into `tx`, with a CLI on `PATH` that
//! records its arguments. What each launch plan turned into must be the same: the arguments the
//! launched program received and the engine data in the record, for a start, a fork and a
//! resume. What differs between two sandboxes by construction is brought to placeholders first:
//! the sandbox's directory, the agents' ids and worktrees, and the words of the hook command,
//! which with a bundle are the plugin's own program. Each test skips itself without tmux.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::Cli;
use serde_json::{Value, json};

/// The conversation the wired `SessionStart` reports, as the engine's CLI would.
const CONVERSATION: &str = "11111111-2222-3333-4444-555555555555";

/// Settings of the Claude plugin: they reach a launch only through the plugin's `register`,
/// whichever process runs it.
const CONFIG: &str =
    "plugins:\n  engine-claude:\n    settings:\n      claude_settings: { theme: dark }\n";

/// An engine under test.
struct Engine {
    /// Its kind, which is also the name of its CLI on `PATH` and its `ext` key.
    kind: &'static str,
    model: &'static str,
    /// The command it wired for `SessionStart`, out of the arguments it gave its CLI.
    wired: fn(&[String]) -> String,
}

const CLAUDE: Engine = Engine {
    kind: "claude",
    model: "sonnet",
    wired: wired_by_claude,
};

const CODEX: Engine = Engine {
    kind: "codex",
    model: "gpt-6.1-sol",
    wired: wired_by_codex,
};

/// Claude's hooks are in its `--settings`.
fn wired_by_claude(arguments: &[String]) -> String {
    let at = arguments
        .iter()
        .position(|argument| argument == "--settings")
        .unwrap();
    let settings: Value = serde_json::from_str(&arguments[at + 1]).unwrap();
    settings["hooks"]["SessionStart"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Codex's hooks are config overrides,
/// `hooks.<Event>=[{matcher="",hooks=[{type="command",command="…"}]}]`, the command a JSON
/// string.
fn wired_by_codex(arguments: &[String]) -> String {
    let wired = arguments
        .iter()
        .find_map(|argument| argument.strip_prefix("hooks.SessionStart="))
        .unwrap();
    let command = wired
        .split_once("command=")
        .unwrap()
        .1
        .strip_suffix("}]}]")
        .unwrap();
    serde_json::from_str(command).unwrap()
}

/// One launch: which it was, the arguments the CLI received, and the engine data the record
/// held once it ran.
type Launch = (&'static str, Vec<String>, Value);

impl Cli {
    /// Codex's own directory, inside the sandbox.
    fn codex_home(&self) -> PathBuf {
        self.dir.path().join("codex-home")
    }

    /// Where the CLI of the session `id` leaves the arguments of its last launch.
    fn launched_file(&self, id: &str) -> PathBuf {
        self.dir.path().join("launched").join(id)
    }

    /// `tx <args>` with a CLI of `engine` on `PATH` that writes its arguments, NUL-separated, to
    /// [`Self::launched_file`] and stays alive; both engines' own directories are the
    /// sandbox's. What it printed.
    fn tx(&self, engine: &Engine, args: &[&str]) -> String {
        let launched = self.dir.path().join("launched");
        std::fs::create_dir_all(&launched).unwrap();
        let recorder = format!(
            "printf '%s\\0' \"$@\" > '{0}/new.'$TX_SESSION_ID\n\
             mv '{0}/new.'$TX_SESSION_ID '{0}/'$TX_SESSION_ID\nexec sleep 600",
            launched.display()
        );
        let path = self.shim_path(&[(engine.kind, &recorder)]);
        let output = self.run(
            args,
            &[
                ("PATH", path.as_str()),
                ("CODEX_HOME", self.codex_home().to_str().unwrap()),
            ],
            "",
        );
        assert!(
            output.status.success(),
            "tx {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// The arguments the CLI of the session `id` was launched with.
    fn launched(&self, id: &str) -> Vec<String> {
        let file = self.launched_file(id);
        assert!(
            self.wait_for(10, || file.exists()),
            "the CLI of {id} never started"
        );
        std::fs::read_to_string(&file)
            .unwrap()
            .trim_end_matches('\0')
            .split('\0')
            .map(str::to_owned)
            .collect()
    }

    fn show(&self, name: &str) -> Value {
        serde_json::from_str(&self.ok(&["show", name])).unwrap()
    }

    /// The names of this sandbox in what the agent `name` was launched with, each with its
    /// placeholder: its worktree, as Claude spells it inside a transcript's path and as it is,
    /// then its id.
    fn names_of(&self, name: &str) -> Vec<(String, String)> {
        let record = self.show(name);
        let worktree = Path::new(record["working_directory"].as_str().unwrap())
            .canonicalize()
            .unwrap();
        let worktree = worktree.to_str().unwrap();
        vec![
            (
                worktree.replace(['/', '.'], "-"),
                format!("<worktree-of-{name}>"),
            ),
            (worktree.to_owned(), format!("<worktree of {name}>")),
            (
                record["id"].as_str().unwrap().to_owned(),
                format!("<id of {name}>"),
            ),
        ]
    }
}

/// Runs a wired hook command the way the engine's CLI does: through a shell, the payload on
/// standard input, inside the session `id`. It gets what a session of this sandbox carries and
/// nothing else of tx's: the five directories, the sandbox's own socket, its plugin roots and
/// the session's id. Whoever runs the suite may sit in a tx session of their own, whose `TX_*`
/// variables name another core.
fn fire(cli: &Cli, command: &str, id: &str, payload: &Value) {
    let mut shell = Command::new("sh");
    shell.args(["-c", command]);
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("TX_") {
            shell.env_remove(name);
        }
    }
    let mut child = shell
        .env_remove("TMUX")
        .envs(common::directory_environment(&cli.state()))
        .env("TX_TMUX_SOCKET", cli.sock())
        .env("TX_PLUGIN_PATH", cli.plugin_path())
        .env("TX_SESSION_ID", id)
        .env("CLAUDE_CONFIG_DIR", cli.claude_config_dir())
        .env("CODEX_HOME", cli.codex_home())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "{command}");
}

/// The life of an agent of `engine` in `cli`: created from the agent definition `developer`
/// with a model, an effort and engine arguments; its wired `SessionStart` fired, which reports
/// the conversation; forked; stopped and respawned. Its three launches, with the names of the
/// sandbox as placeholders. `hook` is what this sandbox wires every hook to, a program and its
/// first argument; that the engine did so is asserted here, on the arguments as they were.
fn life(cli: &Cli, engine: &Engine, hook: [&str; 2]) -> Vec<Launch> {
    cli.write_yaml(CONFIG);
    let repository = cli.repository("repository");
    let data = |name: &str| cli.show(name)["ext"][engine.kind].clone();

    let id = cli.tx(
        engine,
        &[
            "create",
            "a",
            "--cwd",
            repository.to_str().unwrap(),
            "--engine",
            engine.kind,
            "--agent",
            "developer",
            "--model",
            engine.model,
            "--effort",
            "high",
            "--engine-args",
            "--verbose --add-dir '/some where'",
            "--prompt",
            "start",
        ],
    );
    let started = (cli.launched(&id), data("a"));

    // Every hook the engine wired runs this sandbox's hook command, and no other.
    let words = format!("'{}' '{}'", hook[0], hook[1]);
    let wired = (engine.wired)(&started.0);
    assert_eq!(wired, format!("{words} 'SessionStart' || exit 1"));
    let count = |needle: &str| -> usize {
        started
            .0
            .iter()
            .map(|argument| argument.matches(needle).count())
            .sum()
    };
    assert!(count(&words) > 1, "{:?}", started.0);
    assert_eq!(count(&words), count("|| exit 1"), "{:?}", started.0);

    // The wired command works: it moves the agent and records the conversation it reports.
    let transcript = cli.dir.path().join(format!("{CONVERSATION}.jsonl"));
    std::fs::write(&transcript, "{\"type\":\"session_meta\"}\n").unwrap();
    fire(
        cli,
        &wired,
        &id,
        &json!({"session_id": CONVERSATION, "transcript_path": transcript, "source": "startup"}),
    );
    assert_eq!(common::state_of(cli, "a"), "idle");
    assert_eq!(data("a")["conversation_id"], CONVERSATION);

    let fork = cli.tx(engine, &["fork", "a", "--name", "b", "--prompt", "branch"]);
    let forked = (cli.launched(&fork), data("b"));

    cli.tx(engine, &["stop", "a"]);
    std::fs::remove_file(cli.launched_file(&id)).unwrap();
    cli.tx(engine, &["respawn", "a"]);
    let resumed = (cli.launched(&id), data("a"));

    // The longer names first: a worktree is inside the sandbox.
    let sandbox = cli.dir.path().canonicalize().unwrap();
    let mut names = vec![(words, "<hook>".to_owned())];
    names.extend(cli.names_of("a"));
    names.extend(cli.names_of("b"));
    names.push((sandbox.to_str().unwrap().to_owned(), "<sandbox>".to_owned()));
    names.push((
        cli.dir.path().to_str().unwrap().to_owned(),
        "<sandbox>".to_owned(),
    ));
    let placeholders = |text: &str| {
        names
            .iter()
            .fold(text.to_owned(), |text, (name, placeholder)| {
                text.replace(name, placeholder)
            })
    };
    [("start", started), ("fork", forked), ("resume", resumed)]
        .into_iter()
        .map(|(launch, (arguments, data))| {
            (
                launch,
                arguments
                    .iter()
                    .map(|argument| placeholders(argument))
                    .collect(),
                serde_json::from_str(&placeholders(&data.to_string())).unwrap(),
            )
        })
        .collect()
}

/// The launches of `engine` as a plugin program are those of the adapter compiled into `tx`,
/// but for the hook command: the program's own entry in place of `txi hook`.
fn launches_the_same_as_compiled_in(engine: &Engine) {
    let program = common::shipped().join(format!("engine-{0}/bin/tx-engine-{0}", engine.kind));
    let as_a_program = life(
        &Cli::new(),
        engine,
        [program.to_str().unwrap(), "--tx-hook"],
    );
    let compiled_in = life(
        &Cli::compiled_in(),
        engine,
        [env!("CARGO_BIN_EXE_txi"), "hook"],
    );
    for (program, linked) in as_a_program.iter().zip(&compiled_in) {
        assert_eq!(program, linked);
    }
}

#[test]
fn claude_as_a_plugin_program_launches_what_the_adapter_compiled_in_launches() {
    require_tmux!();
    launches_the_same_as_compiled_in(&CLAUDE);
}

#[test]
fn codex_as_a_plugin_program_launches_what_the_adapter_compiled_in_launches() {
    require_tmux!();
    launches_the_same_as_compiled_in(&CODEX);
}
