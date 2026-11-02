//! End-to-end over the built `txc` binary and a private tmux server.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{Cli, cwd_of};

fn wait_for(path: &Path, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.is_empty()
        {
            return text;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn usage_errors_exit_2_without_touching_state() {
    let cli = Cli::new();
    let stderr = cli.fails(&[], 2);
    assert!(
        stderr.contains("missing verb") && stderr.contains("usage: txc"),
        "{stderr}"
    );
    assert!(cli.fails(&["create", "x"], 2).contains("--cwd"));
    assert!(
        cli.fails(&["create", "x", "--cwd", "/"], 2)
            .contains("create needs --engine ENGINE")
    );
    // An engine outside the closed set is a misuse that names the set.
    let stderr = cli.fails(
        &[
            "create", "x", "--cwd", "/", "--engine", "kodex", "--prompt", "go",
        ],
        2,
    );
    assert!(
        stderr.contains("--engine: no engine named \"kodex\" (known: claude, codex)"),
        "{stderr}"
    );
    assert!(cli.fails(&["bogus"], 2).contains("bogus"));
    assert!(!cli.state().exists());
    assert!(cli.ok(&["help"]).starts_with("usage: txc"));
}

#[test]
fn engines_are_registered() {
    let cli = Cli::new();
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\n");
    assert!(cli.state().join("tmux.conf").is_file());
}

#[test]
fn core_errors_exit_1() {
    let cli = Cli::new();
    let cwd = cli.repository("w");
    let stderr = cli.fails(&["show", "nope"], 1);
    assert_eq!(stderr, "txc: no session \"nope\"\n");
    // The engine's plugin is disabled: its kind has no adapter, so the create is refused.
    cli.write_yaml("plugins:\n  engine-codex: { enabled: false }\n");
    let stderr = cli.fails(
        &[
            "create",
            "a",
            "--cwd",
            cwd.to_str().unwrap(),
            "--engine",
            "codex",
            "--prompt",
            "start",
        ],
        1,
    );
    assert_eq!(stderr, "txc: engine codex is disabled\n");
}

/// A disabled engine plugin leaves its kind unregistered: `engines` does not list it, a create
/// and a respawn of that kind are refused with the same message, and an existing Codex record
/// still lists and shows. Re-enabled, the respawn gets past the engine check.
#[test]
fn a_disabled_engine_plugin_leaves_its_kind_unregistered_but_its_records_listed() {
    let cli = Cli::new();
    let id = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f2";
    common::plant_record(&cli, id, "old-codex", "codex");
    cli.write_yaml("plugins:\n  engine-codex: { enabled: false }\n");
    assert_eq!(cli.ok(&["engines"]), "claude\n");
    assert!(cli.ok(&["plugins"]).contains("engine-codex\tdisabled"));
    let listed = cli.ok(&["ls"]);
    assert!(listed.contains("old-codex"), "{listed}");
    assert!(cli.ok(&["show", "old-codex"]).contains("\"codex\""));
    assert_eq!(
        cli.fails(&["respawn", "old-codex"], 1),
        "txc: engine codex is disabled\n"
    );
    // Re-enabled, the engine check passes and the Codex adapter itself answers: this record
    // never got a conversation to resume.
    cli.write_yaml("plugins:\n  engine-codex: { enabled: true }\n");
    assert_eq!(cli.ok(&["engines"]), "claude\ncodex\n");
    let stderr = cli.fails(&["respawn", "old-codex"], 1);
    assert!(stderr.contains("codex cannot resume old-codex"), "{stderr}");
}

/// A record naming an engine this build does not know (hand-edited, or another build's) is
/// skipped: the other records list, stderr names it with the known engines, nothing is deleted.
#[test]
fn records_with_an_unknown_engine_are_skipped_and_named_on_stderr() {
    let cli = Cli::new();
    let id = "0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f1";
    let path = common::plant_record(&cli, id, "odd", "kodex");
    let output = cli.run(&["ls"], &[], "");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(
        stderr.contains(&format!(
            "skipped record {id}: names engine \"kodex\", which this build does not know (known: claude, codex)"
        )),
        "{stderr}"
    );
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    assert!(
        cli.fails(&["show", "odd"], 1)
            .ends_with("no session \"odd\"\n")
    );
    assert!(path.is_file(), "nothing is deleted");
}

/// The version-1 records a migration test starts from, as the builds before this one wrote
/// them: one row of the mapping table each.
mod version_one {
    use serde_json::{Value, json};

    pub const CLAUDE: &str = "a0000000-0000-4000-8000-00000000000a";
    pub const CODEX: &str = "b0000000-0000-4000-8000-00000000000b";
    pub const FORK: &str = "c0000000-0000-4000-8000-00000000000c";
    pub const VIEW: &str = "d0000000-0000-4000-8000-00000000000d";
    pub const SILENT: &str = "e0000000-0000-4000-8000-00000000000e";
    pub const ANCIENT: &str = "f0000000-0000-4000-8000-00000000000f";

    fn base(id: &str, name: &str, lifecycle: &str) -> Value {
        json!({
            "version": 1, "id": id, "name": name, "parent": null, "tags": ["t"], "cwd": "/w",
            "created_at": 1.0, "ended_at": null, "last_modified_at": 5.0, "lifecycle": lifecycle,
        })
    }

    /// A running Claude agent of PR 12a vintage: options in `launch`, a running `state`, the
    /// chat plugin's capture of its conversation, a retired `hooks` entry and a plugin's own.
    pub fn claude() -> Value {
        let mut record = base(CLAUDE, "lead", "running");
        record["state"] = json!("working");
        record["launch"] = json!({"kind": "agent", "engine": "claude", "profile": ["COMMON", "DEVELOPER"], "model": "sonnet", "effort": "max"});
        record["ext"] = json!({
            "chat": {"engine": "claude", "chats": [
                {"id": "old-1", "role": "primary", "how": "spawn", "since": 1.0, "ended_at": 2.0, "transcript": "/t/old-1.jsonl"},
                {"id": "c1", "role": "primary", "how": "spawn", "since": 2.0, "transcript": "/t/c1.jsonl"},
            ]},
            "hooks": {"state": "idle"},
            "artifacts": {"group": "g"},
        });
        record
    }

    /// A stopped Codex agent from before `state` existed, with an effort Codex does not accept.
    pub fn codex() -> Value {
        let mut record = base(CODEX, "helper", "stopped");
        record["ended_at"] = json!(4.0);
        record["launch"] = json!({"kind": "agent", "engine": "codex", "profile": [], "model": "gpt-6.1-sol", "effort": "extreme", "resume": "r1"});
        record
    }

    /// A fork from before core fork: a process record running the engine, the agent launch kept
    /// by the chat plugin.
    pub fn fork() -> Value {
        let mut record = base(FORK, "lead-fork", "exited");
        record["parent"] = json!(CLAUDE);
        record["launch"] = json!({"kind": "process", "program": "claude", "args": ["--resume", "c1", "--fork-session"]});
        record["ext"] = json!({"chat": {
            "launch": {"engine": "claude", "profile": ["COMMON"], "model": "opus"},
            "chats": [{"id": "c2", "role": "fork", "how": "fork", "since": 3.0, "transcript": "/t/c2.jsonl"}],
        }});
        record
    }

    /// An nvim view: a process record that is not an agent.
    pub fn view() -> Value {
        let mut record = base(VIEW, "plan-md", "running");
        record["launch"] = json!({"kind": "process", "program": "nvim", "args": ["plan.md"]});
        record
    }

    /// A running agent that never reported a conversation.
    pub fn silent() -> Value {
        let mut record = base(SILENT, "quiet", "running");
        record["launch"] = json!({"kind": "agent", "engine": "codex", "profile": []});
        record
    }

    /// A record from before `last_modified_at` existed.
    pub fn ancient() -> Value {
        let mut record = base(ANCIENT, "elder", "exited");
        record.as_object_mut().unwrap().remove("last_modified_at");
        record["launch"] =
            json!({"kind": "agent", "engine": "claude", "profile": [], "resume": "c3"});
        record
    }
}

/// Acceptance 1 and 2 of Storage PR 14: version-1 records refuse every verb with the pointer to
/// `txc migrate`; `txc migrate` upgrades them row by row of the mapping table, moves the
/// non-agents to `legacy/`, backs the originals up, and reports; a second run has nothing to
/// do; a key this build does not know survives a rename.
#[test]
fn version_one_records_refuse_until_migrated_then_match_the_mapping() {
    let cli = Cli::new();
    let sessions = cli.state().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let records = [
        version_one::claude(),
        version_one::codex(),
        version_one::fork(),
        version_one::view(),
        version_one::silent(),
        version_one::ancient(),
    ];
    for record in &records {
        let id = record["id"].as_str().unwrap();
        std::fs::write(sessions.join(format!("{id}.json")), record.to_string()).unwrap();
    }
    std::fs::write(sessions.join("notes.json"), "{not json").unwrap();
    let stderr = cli.fails(&["ls"], 1);
    assert_eq!(
        stderr,
        format!(
            "txc: 7 version-1 record(s) in {}: run `txc migrate`\n",
            sessions.display()
        )
    );
    assert!(
        cli.fails(&["show", "lead"], 1)
            .contains("run `txc migrate`")
    );

    let report = cli.ok(&["migrate"]);
    for line in [
        format!("migrated\t{}\tlead", version_one::CLAUDE),
        format!("migrated\t{}\tlead-fork", version_one::FORK),
        format!("migrated\t{}\telder", version_one::ANCIENT),
        format!("migrated-no-conversation\t{}\tquiet", version_one::SILENT),
        format!(
            "legacy\t{}.json\ta process record (nvim), not an agent",
            version_one::VIEW
        ),
        "legacy\tnotes.json\tnot a JSON record".to_owned(),
    ] {
        assert!(report.contains(&line), "{line:?} not in:\n{report}");
    }
    // A resume id without a rollout path: Codex can `resume` it, but names no conversation
    // (`LlmEngine::conversation` wants the path too, PR 8b), so the report says so.
    assert!(
        report.contains(&format!(
            "migrated-no-conversation\t{}\thelper",
            version_one::CODEX
        )),
        "{report}"
    );
    assert!(!report.contains("nothing to do"), "{report}");
    assert!(!sessions.exists(), "sessions/ is removed once empty");
    for record in records
        .iter()
        .filter(|record| record["id"] != version_one::VIEW)
    {
        let id = record["id"].as_str().unwrap();
        let backup = cli.state().join(format!("migration-backup/{id}.v1.json"));
        assert!(backup.is_file(), "{id}");
        let kept: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
        assert_eq!(&kept, record, "the backup is the original, byte for byte");
    }
    assert!(
        cli.state()
            .join("legacy")
            .join(format!("{}.json", version_one::VIEW))
            .is_file()
    );
    assert!(cli.state().join("legacy/notes.json").is_file());

    let listed = cli.ok(&["ls"]);
    for name in ["lead", "helper", "lead-fork", "quiet", "elder"] {
        assert!(listed.contains(name), "{name} missing from:\n{listed}");
    }
    assert!(!listed.contains("plan-md"), "{listed}");
    let show = |name: &str| -> serde_json::Value {
        serde_json::from_str(&cli.ok(&["show", name])).unwrap()
    };

    let lead = show("lead");
    assert_eq!(lead["version"], 2);
    assert_eq!(lead["engine"], "claude");
    assert_eq!(lead["working_directory"], "/w");
    assert_eq!(lead["roles"], serde_json::json!(["COMMON", "DEVELOPER"]));
    assert_eq!(lead["state"], "working");
    assert_eq!(lead["last_modified_at"], 5.0);
    assert_eq!(lead["archived_at"], serde_json::Value::Null);
    assert_eq!(lead["tags"], serde_json::json!(["t"]));
    assert_eq!(lead["parent_id"], serde_json::Value::Null);
    assert_eq!(
        lead["ext"]["claude"],
        serde_json::json!({"model": "sonnet", "effort": "max", "conversation_id": "c1", "transcript_path": "/t/c1.jsonl"})
    );
    assert_eq!(lead["ext"]["artifacts"], serde_json::json!({"group": "g"}));
    assert!(lead["ext"].get("chat").is_none() && lead["ext"].get("hooks").is_none());
    for gone in ["lifecycle", "launch", "ended_at", "cwd", "parent"] {
        assert!(lead.get(gone).is_none(), "{gone}: {lead}");
    }

    let helper = show("helper");
    assert_eq!(helper["engine"], "codex");
    assert_eq!(helper["state"], "ended");
    assert_eq!(
        helper["ext"]["codex"],
        serde_json::json!({"model": "gpt-6.1-sol", "conversation_id": "r1"}),
        "an effort Codex refuses is dropped; the resume id is the conversation"
    );

    let fork = show("lead-fork");
    assert_eq!(fork["engine"], "claude");
    assert_eq!(fork["roles"], serde_json::json!(["COMMON"]));
    assert_eq!(fork["parent_id"], version_one::CLAUDE);
    assert_eq!(fork["state"], "ended");
    assert_eq!(
        fork["ext"]["claude"],
        serde_json::json!({"model": "opus", "conversation_id": "c2", "transcript_path": "/t/c2.jsonl"})
    );

    let quiet = show("quiet");
    assert_eq!(
        quiet["state"], "starting",
        "running without a state starts over"
    );
    assert!(quiet.get("ext").is_none(), "{quiet}");

    let elder = show("elder");
    assert_eq!(elder["state"], "ended");
    let mtime = std::fs::metadata(
        cli.state()
            .join(format!("migration-backup/{}.v1.json", version_one::ANCIENT)),
    )
    .unwrap()
    .modified()
    .unwrap()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap()
    .as_secs_f64();
    let stamped = elder["last_modified_at"].as_f64().unwrap();
    assert!(
        (stamped - mtime).abs() < 5.0,
        "{stamped} vs the file's {mtime}"
    );
    assert_eq!(elder["ext"]["claude"]["conversation_id"], "c3");
    assert!(
        elder["ext"]["claude"]["transcript_path"]
            .as_str()
            .unwrap()
            .ends_with("/c3.jsonl"),
        "{elder}"
    );

    assert_eq!(cli.ok(&["migrate"]), "nothing to do\n");

    // Acceptance 5: an unknown top-level key survives a rewrite.
    let path = common::record_path(&cli, version_one::CODEX);
    let mut record: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    record["future_field"] = serde_json::json!(1);
    std::fs::write(&path, record.to_string()).unwrap();
    cli.ok(&["rename", "helper", "helper-2"]);
    let renamed = show("helper-2");
    assert_eq!(renamed["future_field"], 1);
    assert_eq!(renamed["version"], 2);
}

#[test]
fn agent_session_round_trip() {
    require_tmux!();
    let cli = Cli::new();
    let repository = cli.repository("w");
    // The agent writes its environment's greeting, then captures what is typed at it.
    let path = cli.shim_path(&[("claude", "echo $GREETING > env.txt; exec cat > input.txt")]);
    let output = cli.run(
        &[
            "create",
            "ag-1",
            "--cwd",
            repository.to_str().unwrap(),
            "--tag",
            "t",
            "--env",
            "GREETING=hi",
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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert_eq!(id.len(), 36);
    let cwd = cwd_of(&cli, "ag-1");
    assert_ne!(cwd, repository, "an agent runs in its own worktree");
    assert_eq!(
        wait_for(&cwd.join("env.txt"), Duration::from_secs(5)),
        "hi\n"
    );

    let ls = cli.ok(&["ls"]);
    let fields: Vec<&str> = ls.trim().split('\t').collect();
    assert_eq!(fields[0], id);
    assert_eq!(fields[1], "ag-1");
    assert_eq!(fields[2], "starting");
    assert_eq!(fields[3], cwd.to_str().unwrap());
    assert_eq!(cli.ok(&["ls", "--tag", "zzz"]), "");
    assert_eq!(cli.ok(&["ls", "--running"]).lines().count(), 1);
    assert_eq!(cli.ok(&["ls", "--ended"]), "");

    let shown: serde_json::Value = serde_json::from_str(&cli.ok(&["show", "ag-1"])).unwrap();
    assert_eq!(shown["id"], id);
    assert_eq!(shown["tags"], serde_json::json!(["t"]));
    assert_eq!(cli.ok(&["show", &id]), cli.ok(&["show", "ag-1"]));

    assert_eq!(
        cli.ok(&["rename", "ag-1", "ag-2"]).trim(),
        format!("{id}\tag-2")
    );
    assert_eq!(
        cli.ok(&["tag", "ag-2", "a", "b"]).trim(),
        format!("{id}\ta,b")
    );
    assert_eq!(
        cli.ok(&["tag", "ag-2"]).trim_end_matches('\n'),
        format!("{id}\t")
    );

    let where_ = cli.ok(&["where", "ag-2"]);
    let fields: Vec<&str> = where_.trim().split('\t').collect();
    assert_eq!(fields[0], cli.sock().to_str().unwrap());
    assert!(
        fields[1].starts_with('$') && fields[2].starts_with('%'),
        "{where_}"
    );

    cli.ok(&["send", "ag-2", "hello", "there;"]);
    cli.ok(&["send", "ag-2", "--", "--flag"]);
    // Inside tx's server, `send` carries the caller's id; elsewhere it does not.
    let fake_tmux = format!("{},0,0", cli.sock().display());
    let output = cli.run(
        &["send", "ag-2", "from me"],
        &[("TMUX", &fake_tmux), ("TX_SESSION_ID", &id)],
        "",
    );
    assert!(output.status.success());
    let output = cli.run(
        &["send", "ag-2", "not me"],
        &[("TMUX", "/tmp/other,0,0"), ("TX_SESSION_ID", &id)],
        "",
    );
    assert!(output.status.success());
    // The operator names where they typed with `--via`; from inside a view the view names it.
    cli.ok(&["send", "--via", "plan.md", "ag-2", "from a buffer"]);
    cli.ok(&[
        "ui", "view", "open", "notes", "--kind", "shell", "--", "sleep", "600",
    ]);
    let view_pane = cli.tmux_out(&["display-message", "-p", "-t", "=notes:", "#{pane_id}"]);
    let output = cli.run(
        &["send", "--via", "ignored", "ag-2", "from notes"],
        &[("TMUX", &fake_tmux), ("TMUX_PANE", &view_pane)],
        "",
    );
    assert!(output.status.success());
    // An id core does not know is no agent: the view still names the sender.
    let output = cli.run(
        &["send", "ag-2", "unknown id"],
        &[
            ("TMUX", &fake_tmux),
            ("TMUX_PANE", &view_pane),
            ("TX_SESSION_ID", "unknown-id"),
        ],
        "",
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let typed = loop {
        let text = wait_for(&cwd.join("input.txt"), Duration::from_secs(5));
        if text.lines().count() >= 7 {
            break text;
        }
    };
    assert_eq!(
        typed,
        "<from-user>hello there;</from-user>\n<from-user>--flag</from-user>\n<from-agent session=\"ag-2\">from me</from-agent>\n<from-user>not me</from-user>\n<from-user session=\"plan.md\">from a buffer</from-user>\n<from-user session=\"notes\">from notes</from-user>\n<from-user session=\"notes\">unknown id</from-user>\n"
    );

    assert_eq!(cli.ok(&["refresh"]), "");
    assert_eq!(cli.ok(&["refresh", "ag-2"]), "");
    assert_eq!(cli.ok(&["stop", "ag-2"]).trim(), format!("{id}\tended"));
    assert!(cli.fails(&["where", "ag-2"], 1).contains("not running"));
    assert!(!cli.tmux(&["has-session", "-t", "=ag-2"]).status.success());
    assert_eq!(
        cli.ok(&["archive", "ag-2"]).trim(),
        format!("{id}\tarchived")
    );
    assert_eq!(cli.ok(&["ls"]), "");
    assert!(
        cli.ok(&["ls", "--archived"])
            .starts_with(&format!("{id}\tag-2\tended\t"))
    );
    assert!(cli.ok(&["show", "ag-2"]).contains("\"archived_at\""));
    assert_eq!(
        cli.ok(&["archive", "ag-2"]).trim(),
        format!("{id}\tarchived"),
        "archiving again changes nothing"
    );
}

#[test]
fn ls_json_lists_running_agents_with_exactly_five_keys() {
    require_tmux!();
    let cli = Cli::new();
    assert_eq!(cli.ok(&["ls", "--json"]).trim(), "[]");
    let live = cli.create("live");
    let stopped = cli.create("stopped");
    cli.ok(&["tag", "live", "scope"]);
    cli.ok(&["stop", "stopped"]);

    let listing: serde_json::Value = serde_json::from_str(&cli.ok(&["ls", "--json"])).unwrap();
    let entries = listing.as_array().unwrap();
    assert_eq!(entries.len(), 1, "a stopped agent is not live: {listing}");
    let entry = entries[0].as_object().unwrap();
    let keys: Vec<&str> = entry.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["id", "name", "state", "working_directory", "tags"],
        "exactly the five keys"
    );
    assert_eq!(entry["id"], live.as_str());
    assert_eq!(entry["name"], "live");
    assert!(entry["state"].is_string());
    assert_eq!(entry["tags"], serde_json::json!(["scope"]));
    assert_eq!(
        entry["working_directory"],
        cwd_of(&cli, "live").to_str().unwrap()
    );

    let ended: serde_json::Value =
        serde_json::from_str(&cli.ok(&["ls", "--json", "--ended"])).unwrap();
    assert_eq!(ended[0]["id"], stopped.as_str());
}

#[test]
fn an_agent_reaches_its_own_core_through_the_session_environment() {
    require_tmux!();
    let cli = Cli::new();
    let repository = cli.repository("w");
    let path = cli.shim_path(&[("claude", "env > env.txt; exec cat > input.txt")]);
    // `txc` with relative directory flags, run from the temp dir.
    let relative = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_txc"))
            .current_dir(cli.dir.path())
            .args([
                "--config-dir",
                "config",
                "--data-dir",
                "data",
                "--state-dir",
                "state",
                "--cache-dir",
                "cache",
                "--run-dir",
                "run",
            ])
            .args(args)
            .env("PATH", &path)
            .env_remove("TMUX")
            .env_remove("TX_SESSION_ID")
            .env_remove("TX_APPNAME")
            .output()
            .unwrap()
    };
    // Relative directory flags still yield absolute paths inside the session environment.
    let rel = relative(&[
        "create",
        "me",
        "--cwd",
        repository.to_str().unwrap(),
        "--engine",
        "claude",
        "--prompt",
        "start",
    ]);
    assert!(
        rel.status.success(),
        "{}",
        String::from_utf8_lossy(&rel.stderr)
    );
    let id = String::from_utf8_lossy(&rel.stdout).trim().to_owned();
    let shown = relative(&["show", "me"]);
    assert!(shown.status.success());
    let shown: serde_json::Value = serde_json::from_slice(&shown.stdout).unwrap();
    let cwd = Path::new(shown["working_directory"].as_str().unwrap());
    let env = wait_for(&cwd.join("env.txt"), Duration::from_secs(5));
    let get = |key: &str| {
        env.lines()
            .find_map(|l| l.strip_prefix(&format!("{key}=")))
            .map(str::to_owned)
    };
    let socket = get("TX_TMUX_SOCKET").unwrap();
    for (key, name) in [
        ("TX_CONFIG_DIR", "config"),
        ("TX_DATA_DIR", "data"),
        ("TX_STATE_DIR", "state"),
        ("TX_CACHE_DIR", "cache"),
        ("TX_RUN_DIR", "run"),
    ] {
        let value = get(key).unwrap_or_else(|| panic!("{key} not exported"));
        assert!(Path::new(&value).is_absolute(), "{key}={value}");
        // config and cache are not created by opening the core; compare through the parent.
        let value = Path::new(&value);
        assert_eq!(value.file_name().unwrap(), name, "{key}");
        assert_eq!(
            value.parent().unwrap().canonicalize().unwrap(),
            cli.dir.path().canonicalize().unwrap(),
            "{key}"
        );
    }
    assert_eq!(get("TX_APPNAME").as_deref(), Some("tx-ide"));
    assert_eq!(get("TX_CORE_HOME"), None);
    assert!(Path::new(&socket).is_absolute());
    assert_eq!(
        Path::new(&socket).canonicalize().unwrap(),
        cli.dir.path().join("run/tmux.sock").canonicalize().unwrap()
    );
    // From inside the session: no flags, only the exported environment.
    let mut inner = Command::new(env!("CARGO_BIN_EXE_txc"));
    inner
        .args(["send", "me", "hello self"])
        .env("TX_TMUX_SOCKET", &socket)
        .env("TMUX", format!("{socket},0,0"))
        .env("TX_SESSION_ID", &id)
        .env_remove("HOME")
        .current_dir("/");
    for key in [
        "TX_CONFIG_DIR",
        "TX_DATA_DIR",
        "TX_STATE_DIR",
        "TX_CACHE_DIR",
        "TX_RUN_DIR",
    ] {
        inner.env(key, get(key).unwrap());
    }
    let inner = inner.output().unwrap();
    let typed = wait_for(&cwd.join("input.txt"), Duration::from_secs(5));
    let _ = Command::new("tmux")
        .arg("-S")
        .arg(&socket)
        .arg("kill-server")
        .output();
    assert!(
        inner.status.success(),
        "{}",
        String::from_utf8_lossy(&inner.stderr)
    );
    assert_eq!(
        typed,
        "<from-agent session=\"me\">hello self</from-agent>\n"
    );
}

#[test]
fn refresh_reports_vanished_sessions() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("v");
    assert!(cli.tmux(&["kill-session", "-t", "=v"]).status.success());
    assert_eq!(cli.ok(&["refresh"]).trim(), format!("{id}\tv\tended"));
    assert_eq!(cli.ok(&["refresh"]), "");
}
