//! Claude and Codex as plugin programs, end to end through the binaries: both engines are
//! bundles (every engine call of core goes through the proxy to `tx-engine-<kind>`), with a CLI
//! on `PATH` that records its arguments. An agent of each is started, forked, stopped and
//! resumed, and every hook it wired runs the plugin's own program. Each test that launches an
//! agent skips itself without tmux.
//!
//! What an engine adds to the definitions comes from the engine too: `tx harness ls`,
//! `tx harness ls tools`, `tx harness check` and the warning of `tx create --agent` print what the
//! engines report.

mod common;

use std::io::Write;
use std::path::PathBuf;
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

/// The life of an agent of `engine` as a plugin program: created from the agent definition
/// `developer` with a model, an effort and engine arguments; its wired `SessionStart` fired,
/// which reports the conversation; forked; stopped and respawned, each launch reaching the CLI.
/// Every hook the engine wired is the program's own entry, `tx-engine-<kind> --tx-hook`.
fn life(engine: &Engine) {
    let cli = &Cli::new();
    let program = common::shipped().join(format!("engine-{0}/bin/tx-engine-{0}", engine.kind));
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
    let started = cli.launched(&id);

    // Every hook the engine wired runs the program's own entry, and no other.
    let words = format!("'{}' '--tx-hook'", program.display());
    let wired = (engine.wired)(&started);
    assert_eq!(wired, format!("{words} 'SessionStart' || exit 1"));
    let count = |needle: &str| -> usize {
        started
            .iter()
            .map(|argument| argument.matches(needle).count())
            .sum()
    };
    assert!(count(&words) > 1, "{started:?}");
    assert_eq!(count(&words), count("|| exit 1"), "{started:?}");

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
    cli.launched(&fork);

    cli.tx(engine, &["stop", "a"]);
    std::fs::remove_file(cli.launched_file(&id)).unwrap();
    cli.tx(engine, &["respawn", "a"]);
    cli.launched(&id);
}

#[test]
fn claude_as_a_plugin_program_lives_through_its_own_hook_entry() {
    require_tmux!();
    life(&CLAUDE);
}

#[test]
fn codex_as_a_plugin_program_lives_through_its_own_hook_entry() {
    require_tmux!();
    life(&CODEX);
}

// What an engine adds to the definitions (`tx harness ls`, `tx harness ls tools`, `tx harness check`,
// the warning of `tx create --agent`) comes from the engine. A `claude` that answers `--version`
// and the probe, and a `codex` whose `app-server` lists one tool: nothing real runs.

/// Answers `claude --version`, and the probe's headless run with its `system/init` event.
const PROBED_CLAUDE: &str = r#"case "$1" in
  --version) echo '2.1.999 (Claude Code)' ;;
  -p) echo '{"type":"system","subtype":"init","tools":["Bash","Read","Task","Write"]}'; exec sleep 60 ;;
  *) exec sleep 600 ;;
esac"#;

/// `codex app-server` that answers `initialize` and lists one tool for the server asked about.
const LISTING_CODEX: &str = r#"[ "$1" = app-server ] || exec sleep 600
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*) echo '{"id":0,"result":{}}' ;;
    *'"method":"mcpServerStatus/list"'*)
      id=$(printf '%s' "$line" | sed 's/.*"id":\([0-9]*\).*/\1/')
      echo "{\"id\":$id,\"result\":{\"data\":[{\"name\":\"docs\",\"tools\":{\"find\":{\"description\":\"Finds  a page.\"}}}]}}" ;;
  esac
done"#;

/// `codex app-server` as the QA machine's stand-in has it: refused.
const REFUSING_CODEX: &str = r#"[ "$1" = app-server ] && exit 1
exec sleep 600"#;

/// An agent definition of the operator's that lists a bundled skill and a tool Claude does not
/// know, the bundled skill's reference, and one MCP server.
fn definitions(cli: &Cli) {
    let root = cli.state().join("harness");
    std::fs::create_dir_all(root.join("agents")).unwrap();
    std::fs::write(
        root.join("agents/mine.md"),
        "---\nname: mine\ndescription: test\ntools: Read, Nope\nskills: [pdf]\n---\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("skills/pdf")).unwrap();
    std::fs::write(
        root.join("skills/pdf/SKILL.md"),
        "---\nname: pdf\ndescription: PDFs.\nmetadata:\n  provided-by: claude-bundled\n---\n",
    )
    .unwrap();
    std::fs::write(
        root.join("mcp.json"),
        r#"{"mcpServers": {"docs": {"type": "stdio", "command": "docs-mcp"}}}"#,
    )
    .unwrap();
}

/// What `tx <args>` printed, with `codex` behaving as `codex`, the sandbox's directory as
/// `<sandbox>`: exit code, standard output, standard error.
fn reported(cli: &Cli, args: &[&str], codex: &str, env: &[(&str, &str)]) -> (i32, String, String) {
    let path = cli.shim_path(&[("claude", PROBED_CLAUDE), ("codex", codex)]);
    let mut env = env.to_vec();
    env.push(("PATH", path.as_str()));
    let output = cli.run(args, &env, "");
    let sandbox = cli.dir.path().to_str().unwrap();
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).replace(sandbox, "<sandbox>");
    (
        output.status.code().unwrap(),
        text(&output.stdout),
        text(&output.stderr),
    )
}

#[test]
fn what_the_engines_add_is_what_their_programs_report() {
    let as_programs = Cli::new();
    definitions(&as_programs);
    let mine = "<sandbox>/state/harness/agents/mine.md";

    let (code, out, _) = reported(&as_programs, &["harness", "ls"], LISTING_CODEX, &[]);
    assert_eq!(code, 0);
    assert!(
        out.contains(
            "mine\ttest\n# claude adds to every agent's tools: Skill, ToolSearch\n\nskills:"
        ),
        "{out}"
    );

    let (code, out, _) = reported(
        &as_programs,
        &["harness", "ls", "tools"],
        LISTING_CODEX,
        &[],
    );
    assert_eq!(code, 0);
    assert!(
        out.starts_with("claude (Claude Code 2.1.999):\nAgent\tbuilt-in\nAskUserQuestion\tbuilt-in, interactive only\n"),
        "{out}"
    );
    assert!(
        out.contains(
            "Write\tbuilt-in\n# claude adds to every agent's tools: Skill, ToolSearch\n\n\
             codex (the Claude names it switches off when left out):\nBash\t--disable shell_tool\n"
        ),
        "{out}"
    );
    assert!(
        out.ends_with(
            "ScheduleWakeup\t--disable sleep_tool\n\nmcp docs:\nmcp__docs__find\tFinds a page.\n"
        ),
        "{out}"
    );

    let (code, out, _) = reported(
        &as_programs,
        &["harness", "check", "mine"],
        LISTING_CODEX,
        &[],
    );
    assert_eq!(code, 0);
    assert_eq!(
        out,
        format!(
            "# claude adds to every agent's tools: Skill, ToolSearch\n\
             {mine}:5: warning: bundled skill pdf brings in all bundled skills: pdf, and any \
             others this Claude version ships\nok: 1 agents, 0 skills, 0 MCP servers\n"
        )
    );

    let (code, out, _) = reported(
        &as_programs,
        &["harness", "check", "--engine", "claude", "mine"],
        LISTING_CODEX,
        &[],
    );
    assert_eq!(code, 1, "Nope fails the check");
    assert!(
        out.contains("\nclaude: always present: the tools EndConversation, advisor and DeferredToolPlaceholder\n"),
        "{out}"
    );
    assert!(
        out.ends_with(&format!(
            "{mine}:4: not a Claude Code 2.1.999 tool, which it would drop silently: Nope\n\
             {mine}: claude: gets the baseline tools Skill, ToolSearch\n"
        )),
        "{out}"
    );

    let (code, out, _) = reported(
        &as_programs,
        &["harness", "check", "--engine", "codex", "mine"],
        LISTING_CODEX,
        &[],
    );
    assert_eq!(code, 1, "codex has no bundled skills");
    assert!(out.starts_with("codex: always present: "), "{out}");
    assert!(!out.contains("# claude adds"), "{out}");
    assert!(
        out.contains(&format!(
            "{mine}:5: skill \"pdf\" is provided by claude-bundled: not on codex\n"
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!("{mine}: codex: no codex counterpart: Nope\n")),
        "{out}"
    );
}

/// The trace of one `tx` run of the bundle sandbox: the plugin programs it started.
fn started(cli: &Cli, args: &[&str]) -> Vec<String> {
    let trace = cli.dir.path().join("trace");
    std::fs::write(&trace, "").unwrap();
    reported(
        cli,
        args,
        LISTING_CODEX,
        &[("TX_PLUGIN_TRACE", trace.to_str().unwrap())],
    );
    std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(|line| line.split('\t').take(2).collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn listing_definitions_starts_no_engine_and_a_check_starts_the_one_it_asks() {
    let cli = Cli::new();
    definitions(&cli);
    assert_eq!(started(&cli, &["harness", "ls"]), Vec::<String>::new());
    assert_eq!(
        started(&cli, &["harness", "check", "mine"]),
        ["engine engine-claude"]
    );
    assert_eq!(
        started(&cli, &["harness", "check", "--engine", "claude", "mine"]),
        ["engine engine-claude"]
    );
    assert_eq!(
        started(&cli, &["harness", "check", "--engine", "codex", "mine"]),
        ["engine engine-codex"]
    );
}

#[test]
fn without_codex_no_engine_lists_mcp_tools_and_codex_cannot_be_checked() {
    let cli = Cli::new();
    definitions(&cli);
    cli.write_yaml("plugins:\n  engine-codex: { enabled: false }\n");
    let (code, out, _) = reported(&cli, &["harness", "ls", "tools"], LISTING_CODEX, &[]);
    assert_eq!(code, 0);
    assert!(
        out.ends_with(
            "Skill, ToolSearch\n\n# no MCP tools listed: no active engine can list them\n"
        ),
        "{out}"
    );
    assert_eq!(
        reported(
            &cli,
            &["harness", "check", "--engine", "codex"],
            LISTING_CODEX,
            &[]
        ),
        (
            1,
            String::new(),
            "tx: engine codex is not enabled (enabled: claude)\n".to_owned()
        )
    );
}

/// The QA machine's stand-in `codex` refuses `app-server`: both engines' sections are printed,
/// then the listing fails as a whole.
#[test]
fn an_mcp_listing_that_fails_as_a_whole_fails_after_every_engines_section() {
    let cli = Cli::new();
    definitions(&cli);
    let (code, out, err) = reported(&cli, &["harness", "ls", "tools"], REFUSING_CODEX, &[]);
    assert_eq!(code, 1);
    assert!(
        out.ends_with("ScheduleWakeup\t--disable sleep_tool\n"),
        "{out}"
    );
    assert_eq!(err, "tx: <sandbox>/state: codex app-server: no answer\n");
}

/// `tx create --agent` warns with the launch engine's warnings, and that engine is the one
/// program it starts.
#[test]
fn creating_an_agent_warns_as_its_engine_says_and_starts_that_engine_once() {
    require_tmux!();
    let cli = Cli::new();
    definitions(&cli);
    let repository = cli.repository("repository");
    let trace = cli.dir.path().join("trace");
    let (code, _, err) = reported(
        &cli,
        &[
            "create",
            "x",
            "--agent",
            "mine",
            "--engine",
            "claude",
            "--cwd",
            repository.to_str().unwrap(),
            "--prompt",
            "hi",
        ],
        LISTING_CODEX,
        &[("TX_PLUGIN_TRACE", trace.to_str().unwrap())],
    );
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        err,
        "tx: warning: bundled skill pdf brings in all bundled skills: pdf, and any others \
         this Claude version ships\n"
    );
    let engines = std::fs::read_to_string(&trace)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("engine\t"))
        .count();
    assert_eq!(engines, 1);
    cli.ok(&["stop", "x"]);
}
