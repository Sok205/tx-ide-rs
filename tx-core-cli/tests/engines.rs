//! An engine that runs as its own program, end to end through the binaries: the stand-in engine
//! of `fixtures/engine-fake`, a Python script that speaks the engine protocol, installed as a
//! plugin of the sandbox. Every agent it launches is `sleep`; hooks are fired by hand with
//! `txi hook`. Each test skips itself without tmux or `python3`.

mod common;

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use common::Cli;
use serde_json::Value;

const ENGINE_FAKE: &str = "engine-fake";

impl Cli {
    /// The sandbox with the stand-in engine installed in its own plugin root.
    fn with_the_stand_in() -> Self {
        let cli = Cli::new();
        cli.fixture(ENGINE_FAKE);
        cli
    }

    /// A file of the stand-in's data directory, `<data>/plugins/engine-fake/<name>`.
    fn fake_data(&self, name: &str) -> PathBuf {
        self.state().join("plugins").join(ENGINE_FAKE).join(name)
    }

    /// Every request the stand-in was sent, in order.
    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(self.fake_data("calls"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn methods(&self) -> Vec<String> {
        self.calls()
            .iter()
            .map(|call| call["method"].as_str().unwrap().to_owned())
            .collect()
    }

    /// An agent of the stand-in engine named `name`, in a repository of its own: its id.
    fn create_fake(&self, name: &str) -> String {
        let cwd = self.repository(name);
        self.ok(&[
            "create",
            name,
            "--cwd",
            cwd.to_str().unwrap(),
            "--engine",
            "fake",
            "--prompt",
            "hi",
        ])
        .trim()
        .to_owned()
    }

    fn show(&self, name: &str) -> Value {
        serde_json::from_str(&self.ok(&["show", name])).unwrap()
    }

    fn state_of(&self, name: &str) -> String {
        self.show(name)["state"].as_str().unwrap().to_owned()
    }

    /// A hook fired by hand inside the session `id`, with no payload.
    fn hook(&self, event: &str, id: &str) -> Output {
        self.txi(&["hook", event], &[("TX_SESSION_ID", id)], "")
    }

    fn trace_file(&self) -> PathBuf {
        self.dir.path().join("trace")
    }

    /// The trace lines `run` leaves: every plugin program it started.
    fn traced(&self, run: impl FnOnce(&str) -> Output) -> Vec<String> {
        let trace = self.trace_file();
        std::fs::write(&trace, "").unwrap();
        let output = run(trace.to_str().unwrap());
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read_to_string(&trace)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// How many engine programs `tx <args>` starts.
    fn engines_started_by(&self, args: &[&str]) -> usize {
        self.traced(|trace| self.run(args, &[("TX_PLUGIN_TRACE", trace)], ""))
            .iter()
            .filter(|line| line.starts_with("engine\t"))
            .count()
    }

    /// How many engine programs `txi hook <event>` starts inside the session `id`.
    fn engines_started_by_hook(&self, event: &str, id: &str) -> usize {
        self.traced(|trace| {
            self.txi(
                &["hook", event],
                &[("TX_SESSION_ID", id), ("TX_PLUGIN_TRACE", trace)],
                "",
            )
        })
        .iter()
        .filter(|line| line.starts_with("engine\t"))
        .count()
    }

    fn live_sessions(&self) -> Vec<String> {
        self.tmux_out(&["list-sessions", "-F", "#{session_name}"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn worktrees(&self) -> usize {
        let Ok(keys) = std::fs::read_dir(self.state().join("worktrees")) else {
            return 0;
        };
        keys.flatten()
            .map(|key| std::fs::read_dir(key.path()).unwrap().count())
            .sum()
    }
}

/// Whether the process `pid` is running.
fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[test]
fn an_installed_engine_is_listed_next_to_the_compiled_in_ones() {
    let cli = Cli::with_the_stand_in();
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\nfake\n");
    let plugins: Value = serde_json::from_str(&cli.ok(&["plugins", "--json"])).unwrap();
    let fake = plugins
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == ENGINE_FAKE)
        .unwrap();
    assert_eq!(fake["status"], "active");
    assert_eq!(fake["engine"], "fake");
    assert_eq!(fake["detail"], Value::Null);
    // Listing them starts no engine.
    assert_eq!(cli.engines_started_by(&["engines"]), 0);
    assert!(!cli.fake_data("pid").exists());
    // Switched off, its kind is gone.
    cli.write_yaml("plugins:\n  engine-fake: { enabled: false }\n");
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\n");
}

/// Acceptance 1 and the calls behind it: create, hook, send, interrupt, fork, stop, respawn.
#[test]
fn an_agent_lives_its_whole_life_on_the_stand_in_engine() {
    require_tmux!();
    require_python3!();
    let cli = Cli::with_the_stand_in();
    let id = cli.create_fake("a");
    let record = cli.show("a");
    assert_eq!(record["state"], "starting");
    assert_eq!(record["engine"], "fake");
    // What `configure` answered is the engine's data: the options of the launch.
    assert_eq!(record["ext"]["fake"], serde_json::json!({"effort": "3"}));
    assert_eq!(cli.methods(), ["configure", "start"]);
    let calls = cli.calls();
    assert_eq!(
        calls[0]["params"]["options"],
        serde_json::json!({"effort": "3"})
    );
    assert_eq!(calls[0]["params"]["session"]["id"], id.as_str());
    assert_eq!(calls[1]["params"]["prompt"], "hi");
    let hook_command = calls[1]["params"]["request"]["hook_command"]
        .as_array()
        .unwrap();
    assert!(
        hook_command[0].as_str().unwrap().ends_with("/txi") && hook_command[1] == "hook",
        "an engine without a hook entry of its own is given core's: {hook_command:?}"
    );
    assert!(
        cli.tmux_out(&["list-panes", "-t", "=a:", "-F", "#{pane_current_command}"])
            .contains("sleep"),
        "the plan's program runs in the agent's session"
    );

    // Hooks fired by hand move it, each through the engine's program.
    assert!(cli.hook("Stop", &id).status.success());
    let record = cli.show("a");
    assert_eq!(record["state"], "idle");
    assert_eq!(record["idle_reason"], "finished");
    assert!(cli.hook("UserPromptSubmit", &id).status.success());
    assert_eq!(cli.state_of("a"), "working");
    assert!(cli.hook("PostToolUse", &id).status.success());
    assert_eq!(cli.state_of("a"), "working", "a hook it does not know");
    assert_eq!(
        cli.methods()[2..],
        [
            "on_hook", "archive", "on_hook", "archive", "on_hook", "archive"
        ]
    );

    // A message is the default envelope, an interrupt the manifest's fixed answer: the
    // program is asked for neither.
    cli.ok(&["send", "a", "--", "hello"]);
    assert!(
        cli.wait_for(5, || cli
            .tmux_out(&["capture-pane", "-p", "-t", "=a:"])
            .contains("<from-user>hello</from-user>")),
        "the envelope was typed into the pane"
    );
    assert_eq!(cli.ok(&["interrupt", "a"]), format!("{id}\tinterrupted\n"));
    assert_eq!(cli.state_of("a"), "idle");
    assert_eq!(cli.methods().len(), 8);

    // A fork is the engine's `fork`, given the source.
    let fork = cli
        .ok(&["fork", "a", "--prompt", "branch"])
        .trim()
        .to_owned();
    let forked = cli.show("a-fork");
    assert_eq!(forked["id"], fork.as_str());
    assert_eq!(forked["parent_id"], id.as_str());
    assert_eq!(forked["engine"], "fake");
    assert_eq!(cli.methods()[8..], ["configure", "fork"]);
    let call = &cli.calls()[9];
    assert_eq!(call["params"]["source"]["id"], id.as_str());
    assert_eq!(call["params"]["request"]["session"]["id"], fork.as_str());
    assert_eq!(call["params"]["prompt"], "branch");

    // A stop archives once the process is gone; a respawn resumes.
    assert_eq!(cli.ok(&["stop", "a"]), format!("{id}\tended\n"));
    let archive = cli.calls().pop().unwrap();
    assert_eq!(archive["method"], "archive");
    assert_eq!(archive["params"]["session"]["state"], "ended");
    assert_eq!(cli.ok(&["respawn", "a"]), format!("{id}\ta\n"));
    assert_eq!(cli.state_of("a"), "starting");
    assert_eq!(cli.methods()[10..], ["archive", "resume"]);
    assert_eq!(
        cli.calls().pop().unwrap()["params"]["request"]["session"]["id"],
        id.as_str()
    );
    // The conversation is read from the record: the engine is not asked, and has none to show.
    cli.ok(&["chat", "ls", "a"]);
    assert_eq!(cli.methods().len(), 12);
}

/// Acceptance 2: which commands start the engine's program, counted in the trace.
#[test]
fn the_engines_program_is_started_only_by_the_commands_that_need_it() {
    require_tmux!();
    require_python3!();
    let cli = Cli::with_the_stand_in();
    let cwd = cli.repository("w");
    let cwd = cwd.to_str().unwrap();
    let program = cli
        .plugins_dir()
        .join(ENGINE_FAKE)
        .join("bin")
        .join(ENGINE_FAKE);
    let lines = cli.traced(|trace| {
        cli.run(
            &[
                "create", "a", "--cwd", cwd, "--engine", "fake", "--prompt", "hi",
            ],
            &[("TX_PLUGIN_TRACE", trace)],
            "",
        )
    });
    assert_eq!(
        lines,
        [format!("engine\t{ENGINE_FAKE}\t{}", program.display())],
        "one program for every call of the create"
    );
    let id = cli.show("a")["id"].as_str().unwrap().to_owned();
    for nothing in [
        &["ls"][..],
        &["show", "a"],
        &["where", "a"],
        &["tag", "a", "t"],
        &["rename", "a", "a1"],
        &["send", "a1", "--", "x"],
        &["interrupt", "a1"],
        &["chat", "ls", "a1"],
        &["engines"],
        &["plugins"],
    ] {
        assert_eq!(cli.engines_started_by(nothing), 0, "{nothing:?}");
    }
    for event in ["Stop", "UserPromptSubmit", "PostToolUse"] {
        assert_eq!(cli.engines_started_by_hook(event, &id), 1, "{event}");
    }
    assert_eq!(
        cli.engines_started_by(&["fork", "a1", "--prompt", "branch"]),
        1
    );
    assert_eq!(cli.engines_started_by(&["stop", "a1"]), 1);
    assert_eq!(cli.engines_started_by(&["respawn", "a1"]), 1);
    // A hook of an agent that has ended is stale: nothing is started for it.
    cli.ok(&["stop", "a1"]);
    assert_eq!(cli.engines_started_by_hook("Stop", &id), 0);
    // Nor for a hook that is not tx's.
    assert_eq!(cli.engines_started_by_hook("Stop", "not an id"), 0);
}

/// Acceptance 3: a kind no engine provides fails at launch and leaves nothing; what cannot be a
/// kind is misuse.
#[test]
fn a_kind_no_active_engine_provides_fails_at_launch_and_leaves_nothing() {
    require_tmux!();
    let cli = Cli::with_the_stand_in();
    let cwd = cli.repository("w");
    let create = |engine: &'static str| {
        cli.run(
            &[
                "create",
                "b",
                "--cwd",
                cwd.to_str().unwrap(),
                "--engine",
                engine,
                "--prompt",
                "hi",
            ],
            &[],
            "",
        )
    };
    let refused = create("nope");
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr),
        "tx: engine nope is not enabled (enabled: claude, codex, fake)\n"
    );
    let misuse = create("Bad Kind");
    assert_eq!(misuse.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&misuse.stderr);
    assert!(
        stderr.starts_with("tx: --engine: engine kind \"Bad Kind\" must match [a-z0-9-]{1,31}\n")
            && stderr.contains("usage: tx"),
        "{stderr}"
    );
    assert_eq!(cli.ok(&["ls"]), "");
    assert_eq!(cli.ok(&["ls", "--ended"]), "");
    assert_eq!(cli.worktrees(), 0);
    assert!(!cli.fake_data("pid").exists(), "no engine was started");
}

/// Acceptance 4: with its plugin switched off, an engine's records stay and cannot be launched.
#[test]
fn a_disabled_engine_plugin_keeps_its_records_and_launches_none() {
    require_tmux!();
    require_python3!();
    let cli = Cli::with_the_stand_in();
    let ended = cli.create_fake("a");
    cli.ok(&["stop", "a"]);
    let running = cli.create_fake("c");
    let calls = cli.calls().len();
    cli.write_yaml("plugins:\n  engine-fake: { enabled: false }\n");
    assert!(cli.ok(&["plugins"]).contains("engine-fake\tdisabled"));
    let listed = cli.ok(&["ls", "--ended"]);
    assert!(listed.contains(&ended), "{listed}");
    assert_eq!(cli.show("a")["engine"], "fake");
    assert_eq!(
        cli.fails(&["respawn", "a"], 1),
        "tx: engine fake is not enabled (enabled: claude, codex)\n"
    );
    assert_eq!(
        cli.fails(&["fork", "c", "--prompt", "branch"], 1),
        "tx: engine fake is not enabled (enabled: claude, codex)\n"
    );
    // A hook of its running agent finds no engine: that is not a failure, and nothing moves.
    let hook = cli.hook("Stop", &running);
    assert!(hook.status.success());
    assert_eq!(String::from_utf8_lossy(&hook.stderr), "");
    assert_eq!(cli.state_of("c"), "starting");
    // The agent can still be stopped; nobody is there to archive it.
    cli.ok(&["stop", "c"]);
    assert_eq!(cli.calls().len(), calls, "the program was never asked");
    // Switched on again, the record respawns.
    cli.write_yaml("");
    cli.ok(&["respawn", "a"]);
    assert_eq!(cli.state_of("a"), "starting");
}

/// Acceptance 5: a program that does not speak the protocol fails the create by name, and
/// nothing of the agent is left.
#[test]
fn an_engine_that_does_not_speak_the_protocol_fails_the_create_and_leaves_nothing() {
    require_tmux!();
    require_python3!();
    let cli = Cli::with_the_stand_in();
    let cwd = cli.repository("w");
    let create = || {
        cli.run(
            &[
                "create",
                "c",
                "--cwd",
                cwd.to_str().unwrap(),
                "--engine",
                "fake",
                "--prompt",
                "hi",
            ],
            &[],
            "",
        )
    };
    let script = cli
        .plugins_dir()
        .join(ENGINE_FAKE)
        .join("bin")
        .join(ENGINE_FAKE);
    let good = std::fs::read_to_string(&script).unwrap();
    let (shebang, rest) = good.split_once('\n').unwrap();
    for (broken, expected) in [
        (
            format!("{shebang}\nprint(\"oops\", flush=True)\n{rest}"),
            "tx: engine fake: not a response to configure: oops\n",
        ),
        (
            format!("{shebang}\nraise SystemExit(0)\n"),
            "tx: engine fake: the engine exited without answering configure\n",
        ),
        // Any value is engine data, so `configure` passes; a plan is not a number.
        (
            format!(
                "{shebang}\nimport sys\nfor line in sys.stdin:\n    print('{{\"ok\": 7}}', flush=True)\n"
            ),
            "tx: engine fake: bad answer to start: ",
        ),
    ] {
        std::fs::write(&script, broken).unwrap();
        let refused = create();
        assert_eq!(refused.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(stderr.starts_with(expected), "{stderr}");
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
        assert_eq!(String::from_utf8_lossy(&refused.stdout), "");
        assert_eq!(cli.ok(&["ls"]), "", "no record");
        assert_eq!(cli.ok(&["ls", "--ended"]), "", "no record");
        assert_eq!(cli.worktrees(), 0, "no worktree");
        assert!(cli.live_sessions().is_empty(), "no tmux session");
    }
    // What the engine itself refuses is its own word.
    std::fs::write(
        &script,
        format!(
            "{shebang}\nfor line in __import__('sys').stdin:\n    print('{{\"error\": {{\"kind\": \"refused\", \"message\": \"fake options: no such model\"}}}}', flush=True)\n"
        ),
    )
    .unwrap();
    let refused = create();
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&refused.stderr),
        "tx: fake options: no such model\n"
    );
    assert_eq!(cli.worktrees(), 0);
    // Put right again, it launches.
    std::fs::write(&script, good).unwrap();
    assert!(create().status.success());
    assert_eq!(cli.state_of("c"), "starting");
}

/// Contract F: on the hook path the engine's program is running before the session lock is
/// taken. The test holds the agent's lock; `txi hook` must then wait, with the program already
/// started and not yet asked.
#[test]
fn the_engines_program_is_running_before_the_session_lock_is_taken() {
    require_tmux!();
    require_python3!();
    let cli = Cli::with_the_stand_in();
    let id = cli.create_fake("a");
    let created_by = std::fs::read_to_string(cli.fake_data("pid")).unwrap();
    assert!(
        cli.wait_for(5, || !alive(created_by.trim())),
        "the program of the create ended with it"
    );
    std::fs::remove_file(cli.fake_data("pid")).unwrap();
    let calls = cli.calls().len();

    // `<state>/locks/<id>.lock`, as core opens it: created by whoever locks it first.
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(cli.state().join("locks").join(format!("{id}.lock")))
        .unwrap();
    lock.lock().unwrap();

    let trace = cli.trace_file();
    std::fs::write(&trace, "").unwrap();
    let mut hook = cli
        .txi_command(
            &["hook", "Stop"],
            &[
                ("TX_SESSION_ID", id.as_str()),
                ("TX_PLUGIN_TRACE", trace.to_str().unwrap()),
            ],
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        cli.wait_for(10, || cli.fake_data("pid").exists()
            && !std::fs::read_to_string(cli.fake_data("pid"))
                .unwrap()
                .is_empty()),
        "the engine's program started while the lock was held"
    );
    let pid = std::fs::read_to_string(cli.fake_data("pid")).unwrap();
    assert!(alive(pid.trim()), "and it is running");
    assert_eq!(
        std::fs::read_to_string(&trace).unwrap().lines().count(),
        1,
        "started once"
    );
    // The hook itself waits for the lock: nothing was asked, nothing moved.
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(hook.try_wait().unwrap().is_none(), "txi is waiting");
    assert_eq!(cli.calls().len(), calls);

    lock.unlock().unwrap();
    let status = hook.wait().unwrap();
    assert!(status.success());
    assert_eq!(cli.state_of("a"), "idle");
    assert_eq!(cli.methods()[calls..], ["on_hook", "archive"]);
    assert_eq!(
        std::fs::read_to_string(&trace).unwrap().lines().count(),
        1,
        "the program started before the lock answered both calls"
    );
}

/// Acceptance 7: two hundred hooks, eight at a time, each with an engine program of its own.
#[test]
fn a_burst_of_hooks_through_the_engines_program_never_fails_one() {
    require_tmux!();
    require_python3!();
    let cli = Cli::with_the_stand_in();
    let id = cli.create_fake("a");
    let failures: Vec<String> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| {
                scope.spawn(|| {
                    (0..25)
                        .filter_map(|_| {
                            let output = cli.txi(
                                &["hook", "UserPromptSubmit"],
                                &[("TX_SESSION_ID", id.as_str())],
                                "{}",
                            );
                            (!output.status.success() || !output.stderr.is_empty()).then(|| {
                                format!(
                                    "{:?}: {}",
                                    output.status.code(),
                                    String::from_utf8_lossy(&output.stderr)
                                )
                            })
                        })
                        .collect::<Vec<String>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().unwrap())
            .collect()
    });
    assert!(failures.is_empty(), "{failures:?}");
    assert_eq!(cli.state_of("a"), "working");
    let hooks = cli
        .methods()
        .iter()
        .filter(|method| *method == "on_hook")
        .count();
    assert_eq!(hooks, 200);
}

/// Contract D: of two installed plugins naming one engine kind, or one `ext` key, the first in
/// plugin order has it; the other stays active without its engine, and says why.
#[test]
fn of_two_plugins_with_one_kind_or_one_ext_key_the_first_keeps_it() {
    let cli = Cli::with_the_stand_in();
    let engine = |id: &str, section: &str| {
        cli.plugin(
            id,
            &format!(
                "api: 1\nid: {id}\ndescription: Another engine.\nengine:\n  bin: {id}\n{section}"
            ),
            &[],
        );
    };
    engine("engine-key", "  kind: other\n  ext_key: fake\n");
    engine("engine-twin", "  kind: fake\n  ext_key: twin\n");
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\nfake\n");
    let plugins: Value = serde_json::from_str(&cli.ok(&["plugins", "--json"])).unwrap();
    let row = |id: &str| {
        plugins
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("{id}"))
            .clone()
    };
    assert_eq!(row(ENGINE_FAKE)["detail"], Value::Null);
    for (id, detail) in [
        (
            "engine-key",
            "ext key \"fake\" is already \"engine-fake\"'s",
        ),
        (
            "engine-twin",
            "engine kind \"fake\" is already \"engine-fake\"'s",
        ),
    ] {
        assert_eq!(row(id)["status"], "active", "{id}");
        assert_eq!(row(id)["detail"], detail, "{id}");
    }
    let listed = cli.ok(&["plugins"]);
    assert!(
        listed.contains("engine-twin\tactive\tengine kind \"fake\" is already \"engine-fake\"'s\n"),
        "{listed}"
    );
    // An installed engine of a kind a compiled-in engine has is not settled silently: tx
    // refuses to run with both. Claude is compiled in where no bundle of it is installed.
    let compiled_in = Cli::compiled_in();
    compiled_in.plugin(
        "engine-clash",
        "api: 1\nid: engine-clash\ndescription: Another engine.\nengine:\n  bin: engine-clash\n  kind: claude\n",
        &[],
    );
    assert_eq!(
        compiled_in.fails(&["ls"], 1),
        "tx: plugin \"engine-clash\": an engine of kind claude is already registered\n"
    );
}

/// Contract D: an engine whose manifest names a hook entry of its own is sent that entry as the
/// hook command of every launch, in place of `txi hook`.
#[test]
fn an_engine_with_a_hook_entry_of_its_own_is_sent_it_in_the_launch_request() {
    require_tmux!();
    require_python3!();
    let cli = Cli::new();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(ENGINE_FAKE)
        .join("bin")
        .join(ENGINE_FAKE);
    let directory = cli.plugin(
        "engine-own",
        "api: 1\nid: engine-own\ndescription: An engine with its own hook entry.\nengine:\n  kind: own\n  \
         bin: engine-own\n  hook: { bin: hook-entry, args: [--tx-hook] }\n",
        &[],
    );
    let program = directory.join("bin/engine-own");
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    std::fs::copy(&fixture, &program).unwrap();
    let cwd = cli.repository("w");
    cli.ok(&[
        "create",
        "a",
        "--cwd",
        cwd.to_str().unwrap(),
        "--engine",
        "own",
        "--prompt",
        "hi",
    ]);
    let calls = std::fs::read_to_string(cli.state().join("plugins/engine-own/calls")).unwrap();
    let start: Value = serde_json::from_str(calls.lines().nth(1).unwrap()).unwrap();
    assert_eq!(start["method"], "start");
    assert_eq!(
        start["params"]["request"]["hook_command"],
        serde_json::json!([
            directory.join("bin/hook-entry").to_string_lossy(),
            "--tx-hook"
        ])
    );
}
