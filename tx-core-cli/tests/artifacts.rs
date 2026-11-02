//! The artifacts plugin through the binary: `txc artifact …`, `txc group`, and prefix+a.

mod common;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use common::{Cli, lists_verb};

fn data_dir(cli: &Cli) -> PathBuf {
    cli.state().join("plugins/artifacts")
}

fn write_file(cli: &Cli, name: &str, body: &str) -> String {
    let path = cli.dir.path().join(name);
    std::fs::write(&path, body).unwrap();
    path.to_str().unwrap().to_owned()
}

fn run_as(cli: &Cli, actor: &str, args: &[&str]) -> std::process::Output {
    cli.run(args, &[("TX_SESSION_ID", actor)], "")
}

fn ok_as(cli: &Cli, actor: &str, args: &[&str]) -> String {
    let output = run_as(cli, actor, args);
    assert!(
        output.status.success(),
        "txc {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// `txc artifact create <file>`; returns the new id.
fn create(cli: &Cli, actor: &str, name: &str, body: &str) -> String {
    let file = write_file(cli, name, body);
    let out = ok_as(cli, actor, &["artifact", "create", &file]);
    let id = out
        .strip_prefix("Created artifact ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("{out:?}"));
    id.to_owned()
}

fn record(cli: &Cli, id: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(data_dir(cli).join(format!("{id}.json"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn revs(cli: &Cli, id: &str) -> Vec<u64> {
    record(cli, id)["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|touch| touch["rev"].as_u64().unwrap())
        .collect()
}

#[test]
fn help_lists_the_verbs() {
    let cli = Cli::new();
    let help = cli.ok(&["help"]);
    for verb in [
        "artifact",
        "group",
        "_artifact-menu",
        "_artifact-pick",
        "_artifact-go",
    ] {
        assert!(lists_verb(&help, verb), "{verb}: {help}");
    }
    let usage = cli.ok(&["artifact", "help"]);
    assert!(
        usage.starts_with("usage: txc artifact <subcommand>"),
        "{usage}"
    );
}

#[test]
fn disabled_plugin_hides_the_verbs() {
    let cli = Cli::new();
    cli.write_yaml("plugins:\n  artifacts: { enabled: false }\n");
    let stderr = cli.fails(&["artifact", "ls"], 2);
    assert!(stderr.contains("unknown verb \"artifact\""), "{stderr}");
}

#[test]
fn create_modify_show_diff_and_ls() {
    let cli = Cli::new();
    let actor = "11111111-1111-4111-8111-111111111111";
    let id = create(&cli, actor, "plan.md", "a\nb\n");
    let dir = data_dir(&cli);
    assert_eq!(
        std::fs::read(dir.join(&id).join("current.md")).unwrap(),
        b"a\nb\n"
    );
    assert_eq!(
        std::fs::read(dir.join(&id).join("revs/0.md")).unwrap(),
        b"a\nb\n"
    );
    assert_eq!(record(&cli, &id)["history"][0]["session_id"], actor);

    let same = write_file(&cli, "same.md", "a\nb\n");
    let out = ok_as(&cli, actor, &["artifact", "modify", &id[..8], &same]);
    assert!(
        out.starts_with("No change: the supplied file is identical to rev 0"),
        "{out}"
    );

    let next = write_file(&cli, "next.md", "a\nc\n");
    let out = cli.ok(&["artifact", "modify", &id, &next, "--changes", "swap b"]);
    assert_eq!(out, format!("Modified artifact {id} → rev 1\n"));
    assert_eq!(record(&cli, &id)["history"][1]["session_id"], "user");
    assert_eq!(record(&cli, &id)["history"][1]["changes"], "swap b");

    assert_eq!(
        cli.ok(&["artifact", "diff", &id]),
        "--- rev0\n+++ rev1\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n"
    );
    assert_eq!(
        cli.ok(&["artifact", "diff", &id, "1", "1"]),
        "(no differences)\n"
    );

    std::fs::write(dir.join(&id).join("current.md"), "a\nc\nd\n").unwrap();
    let show = cli.ok(&["artifact", "show", &id]);
    assert!(show.starts_with(&format!("plan.md  ({id})\n")), "{show}");
    assert!(show.contains("  working:    dirty"), "{show}");
    assert!(show.contains("  group:      derived: "), "{show}");
    let out = cli.ok(&["artifact", "modify", &id]);
    assert_eq!(out, format!("Modified artifact {id} → rev 2\n"));
    assert!(
        cli.ok(&["artifact", "show", &id])
            .contains("working:    clean")
    );

    let ls = cli.ok(&["artifact", "ls"]);
    assert!(ls.starts_with("ARTIFACTS\n  "), "{ls}");
    assert!(ls.contains(&id[..8]) && ls.contains("3r"), "{ls}");
    let mine = cli.ok(&["artifact", "ls", "--session", actor]);
    assert!(mine.contains(&id[..8]), "{mine}");
    let none = cli.ok(&[
        "artifact",
        "ls",
        "--session",
        "22222222-2222-4222-8222-222222222222",
    ]);
    assert_eq!(none, "ARTIFACTS\n  (none)\n");
    assert_eq!(cli.ok(&["artifact", "doctor"]), "artifacts: clean\n");
}

#[test]
fn artifact_errors_exit_1_and_usage_exits_2() {
    let cli = Cli::new();
    let stderr = cli.fails(&["artifact", "show", "nope"], 1);
    assert_eq!(stderr, "txc artifact: artifact 'nope' not found\n");
    let stderr = cli.fails(&["artifact", "show", "../x"], 1);
    assert!(stderr.contains("not found"), "{stderr}");
    let stderr = cli.fails(&["artifact", "create", "/no/such/file"], 2);
    assert!(stderr.contains("no such file: /no/such/file"), "{stderr}");
    let id = create(&cli, "user", "p.md", "x");
    let stderr = cli.fails(&["artifact", "diff", &id], 1);
    assert!(stderr.contains("has only one revision"), "{stderr}");
    let stderr = cli.fails(&["artifact", "diff", &id, "0"], 2);
    assert!(stderr.contains("both revs or neither"), "{stderr}");
    cli.fails(&["artifact", "frob"], 2);
}

#[test]
fn artifact_group_sets_reads_and_clears_the_override() {
    let cli = Cli::new();
    let id = create(&cli, "user", "p.md", "x");
    assert_eq!(
        cli.ok(&["artifact", "group", &id]),
        "own:      —\nresolved: ungrouped\n"
    );
    cli.ok(&["artifact", "group", &id, "effort"]);
    assert_eq!(
        cli.ok(&["artifact", "group", &id]),
        "own:      effort\nresolved: effort\n"
    );
    cli.fails(&["artifact", "group", &id, "x", "--clear"], 2);
    cli.ok(&["artifact", "group", &id, "--clear"]);
    assert_eq!(record(&cli, &id)["group"], serde_json::Value::Null);
    assert_eq!(cli.ok(&["artifact", "doctor"]), "artifacts: clean\n");
}

/// Gate: two `modify` calls racing produce contiguous revs, none lost.
#[test]
fn two_concurrent_modifies_produce_two_contiguous_revs() {
    let cli = Cli::new();
    let id = create(&cli, "user", "plan.md", "base\n");
    for round in 0..5 {
        let files: Vec<String> = (0..2)
            .map(|n| {
                let body = format!("round {round} writer {n}\n");
                write_file(&cli, &format!("w{n}.md"), &body)
            })
            .collect();
        let before = revs(&cli, &id).len() as u64;
        let children: Vec<Child> = files
            .iter()
            .map(|file| {
                cli.command(&["artifact", "modify", &id, file], &[])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for child in children {
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        assert_eq!(
            revs(&cli, &id),
            (0..before + 2).collect::<Vec<u64>>(),
            "both writes landed, contiguously"
        );
    }
    let dir = data_dir(&cli).join(&id).join("revs");
    for rev in revs(&cli, &id) {
        assert!(dir.join(format!("{rev}.md")).is_file(), "rev {rev}");
    }
    assert_eq!(cli.ok(&["artifact", "doctor"]), "artifacts: clean\n");
}

/// Gate: `doctor --repair` removes only the orphan rev file.
#[test]
fn doctor_repair_removes_only_the_orphan_rev() {
    let cli = Cli::new();
    let id = create(&cli, "user", "plan.md", "v0\n");
    let next = write_file(&cli, "v1.md", "v1\n");
    cli.ok(&["artifact", "modify", &id, &next]);
    let revs_dir = data_dir(&cli).join(&id).join("revs");
    std::fs::write(revs_dir.join("2.md"), "crash debris").unwrap();
    std::fs::write(revs_dir.join("notes.txt"), "not a rev").unwrap();
    std::fs::write(revs_dir.join(".3.abc.tmp"), "staging").unwrap();

    let output = cli.run(&["artifact", "doctor"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        out.contains(&format!("{id}: orphan rev file 2 not in history")),
        "{out}"
    );
    assert!(out.ends_with("artifacts: 1 problem(s)\n"), "{out}");

    // The orphan blocks the slot the next modify needs.
    let blocked = write_file(&cli, "v2.md", "v2\n");
    let stderr = cli.fails(&["artifact", "modify", &id, &blocked], 1);
    assert!(stderr.contains("rev 2 is already claimed"), "{stderr}");

    let out = cli.ok(&["artifact", "doctor", "--repair"]);
    assert_eq!(
        out,
        format!("  {id}: removed orphan rev file 2\nartifacts: clean\n")
    );
    let mut left: Vec<String> = std::fs::read_dir(&revs_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, [".3.abc.tmp", "0.md", "1.md", "notes.txt"]);
    assert_eq!(std::fs::read(revs_dir.join("1.md")).unwrap(), b"v1\n");
    cli.ok(&["artifact", "modify", &id, &blocked]);
    assert_eq!(revs(&cli, &id), [0, 1, 2]);
}

#[test]
fn doctor_reports_dirty_missing_and_out_of_band() {
    let cli = Cli::new();
    let id = create(&cli, "user", "plan.md", "v0\n");
    let dir = data_dir(&cli);
    std::fs::write(dir.join(&id).join("current.md"), "edited").unwrap();
    std::fs::create_dir_all(dir.join("stray")).unwrap();
    std::fs::remove_file(dir.join(".log.jsonl")).unwrap();
    let output = cli.run(&["artifact", "doctor"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        out.contains("working copy differs from last rev 0"),
        "{out}"
    );
    assert!(
        out.contains("rev 0 touch has no matching log line"),
        "{out}"
    );
    assert!(
        out.contains("stray: content directory has no record"),
        "{out}"
    );
    assert!(out.ends_with("artifacts: 3 problem(s)\n"), "{out}");
}

#[test]
fn group_sets_reads_and_clears_a_sessions_override() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("worker");
    cli.ok(&["tag", "worker", "feat-x"]);
    assert_eq!(
        cli.ok(&["group", "worker"]),
        "own:      —\nresolved: feat-x\n"
    );
    assert_eq!(
        cli.ok(&["group", "worker", "effort"]),
        "Grouped 'worker' (group=effort)\n"
    );
    let shown: serde_json::Value = serde_json::from_str(&cli.ok(&["show", &id])).unwrap();
    assert_eq!(shown["ext"]["artifacts"]["group"], "effort");
    assert_eq!(
        cli.ok(&["group", "worker"]),
        "own:      effort\nresolved: effort\n"
    );

    // An artifact the worker created takes its group.
    let artifact = create(&cli, &id, "p.md", "x");
    assert_eq!(
        cli.ok(&["artifact", "group", &artifact]),
        "own:      —\nresolved: effort\n"
    );

    cli.ok(&["group", "worker", "--clear"]);
    let shown: serde_json::Value = serde_json::from_str(&cli.ok(&["show", &id])).unwrap();
    assert!(shown["ext"].get("artifacts").is_none(), "{shown}");
    cli.fails(&["group", "worker", ""], 2);
}

/// Review focus 1: a malformed `ext["artifacts"]` is reported by this plugin's verb and never
/// makes `ls` or `show` fail.
#[test]
fn a_malformed_ext_is_reported_and_never_breaks_ls_or_show() {
    require_tmux!();
    let cli = Cli::new();
    let id = cli.create("worker");
    let path = common::record_path(&cli, &id);
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["ext"] = serde_json::json!({"artifacts": {"group": 5}});
    std::fs::write(&path, json.to_string()).unwrap();

    assert!(cli.ok(&["ls"]).contains("worker"));
    cli.ok(&["show", "worker"]);
    let output = cli.run(&["group", "worker"], &[], "");
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("session worker has a malformed ext.artifacts"),
        "{stderr}"
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "own:      —\nresolved: worker\n"
    );
    cli.ok(&["artifact", "ls"]);
    let artifact = create(&cli, &id, "p.md", "x");
    cli.ok(&["artifact", "show", &artifact]);
}

fn view_option(cli: &Cli, view: &str, option: &str) -> String {
    cli.tmux_out(&["show-options", "-vt", &format!("={view}:"), option])
}

/// A plugins.yaml naming a stand-in editor that just stays alive.
fn fake_editor(cli: &Cli) -> String {
    let path = cli.shim_path(&[("fake-nvim", "exec sleep 60")]);
    let dir = path.split(':').next().unwrap();
    let editor = format!("{dir}/fake-nvim");
    cli.write_yaml(&format!(
        "plugins:\n  artifacts:\n    settings: {{ editor: \"{editor}\" }}\n"
    ));
    editor
}

#[test]
fn artifact_open_starts_a_bound_view() {
    require_tmux!();
    let cli = Cli::new();
    let editor = fake_editor(&cli);
    let worker = cli.create("worker");
    cli.ok(&["tag", "worker", "feat-x"]);
    let id = create(&cli, &worker, "plan.md", "x");
    let out = ok_as(&cli, &worker, &["artifact", "open", &id]);
    let current = data_dir(&cli).join(&id).join("current.md");
    assert_eq!(
        out,
        format!(
            "Opened artifact {id} in nvim view 'plan-md' ({})\n",
            current.display()
        )
    );
    // A tx-client view: no core record, its fields in tmux options.
    assert!(!cli.ok(&["ls"]).contains("plan-md"));
    let sessions = std::fs::read_dir(cli.state().join("agents"))
        .unwrap()
        .count();
    assert_eq!(sessions, 1, "only the worker's record");
    assert_eq!(view_option(&cli, "plan-md", "@tx-view"), "1");
    assert_eq!(view_option(&cli, "plan-md", "@tx-view-kind"), "editor");
    assert_eq!(view_option(&cli, "plan-md", "@tx-view-bound"), id);
    assert_eq!(view_option(&cli, "plan-md", "@tx-view-parent"), worker);
    assert_eq!(view_option(&cli, "plan-md", "@tx-view-tags"), "feat-x");
    assert_eq!(
        cli.tmux_out(&[
            "display-message",
            "-p",
            "-t",
            "=plan-md:",
            "#{pane_start_command}"
        ]),
        format!("{editor} {}", current.display())
    );
    assert!(cli.ok(&["ui", "view", "ls"]).contains("plan-md"));
    // A view takes its artifact's group; it has no override of its own.
    cli.ok(&["group", "worker", "effort"]);
    assert_eq!(
        cli.ok(&["group", "plan-md"]),
        "own:      —\nresolved: effort\n"
    );
    let output = cli.run(&["group", "plan-md", "other"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("'plan-md' is a view"),
        "{output:?}"
    );
    cli.fails(&["group", "nosuch"], 1);
    // A second open shows the view already open; no second view, no second open logged.
    let out = cli.ok(&["artifact", "open", &id, "--tag", "x,y"]);
    assert_eq!(
        out,
        format!("Artifact {id} is already open in nvim view 'plan-md'\n")
    );
    let views = cli.ok(&["ui", "view", "ls"]);
    assert_eq!(views.lines().count(), 1, "{views}");
    let log = std::fs::read_to_string(data_dir(&cli).join(".log.jsonl")).unwrap();
    assert_eq!(log.matches("\"artifact-open\"").count(), 1, "{log}");
    // Another artifact with the same title finds the name taken in tmux: the id suffix.
    let other = create(&cli, &worker, "plan.md", "y");
    let out = cli.ok(&["artifact", "open", &other]);
    assert!(
        out.contains(&format!("nvim view 'plan-md-{}'", &other[..8])),
        "{out}"
    );
    assert_eq!(cli.ok(&["ui", "view", "ls"]).lines().count(), 2);
}

/// Cold start: with no tmux server running, `ui view ls` lists nothing and `artifact open`
/// starts the server (with core's conf) through the view's `new-session`.
#[test]
fn artifact_open_starts_the_server_when_none_runs() {
    require_tmux!();
    let cli = Cli::new();
    fake_editor(&cli);
    let id = create(&cli, "user", "plan.md", "x");
    assert!(
        !cli.tmux(&["list-sessions"]).status.success(),
        "no server yet"
    );
    assert_eq!(cli.ok(&["ui", "view", "ls"]), "");
    let out = cli.ok(&["artifact", "open", &id]);
    assert!(out.contains("nvim view 'plan-md'"), "{out}");
    assert_eq!(view_option(&cli, "plan-md", "@tx-view-kind"), "editor");
    assert_eq!(view_option(&cli, "plan-md", "@tx-view-bound"), id);
    assert!(cli.ok(&["ui", "view", "ls"]).contains("plan-md"));
}

// ----- prefix+a ---------------------------------------------------------------------------

/// The operator's setup for prefix+a: `ui install`ed server, an agent `a`, an outer terminal on
/// `home`, whose pane nest-attaches `a`. Returns (a's id, artifact id).
fn operator(cli: &Cli) -> (String, String) {
    fake_editor(cli);
    // The menu would pop up on this first attach and take the keys.
    let yaml = std::fs::read_to_string(cli.state().join("plugins.yaml")).unwrap();
    cli.write_yaml(&format!("{yaml}  menu: {{ enabled: false }}\n"));
    cli.ok(&["ui", "install"]);
    let path = cli.fake_claude_path();
    let cwd = cli.repository("a");
    let output = cli.run(
        &[
            "create",
            "a",
            "--cwd",
            cwd.to_str().unwrap(),
            "--engine",
            "claude",
            "--prompt",
            "start",
        ],
        &[("PATH", &path)],
        "",
    );
    assert!(output.status.success(), "{output:?}");
    let a = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(
        cli.wait_for(5, || cli
            .tmux_out(&["list-keys", "-T", "prefix", "a"])
            .contains("tmux-artifact-menu")),
        "prefix+a never bound"
    );
    let artifact = create(cli, &a, "plan.md", "the plan\n");
    let sock = cli.sock().display().to_string();
    let nest = format!("bash -c 'set -m; TMUX= tmux -S {sock} attach -t =a; exec sh'");
    assert!(
        cli.tmux(&[
            "new-session",
            "-d",
            "-s",
            "home",
            "-x",
            "120",
            "-y",
            "40",
            &nest
        ])
        .status
        .success()
    );
    cli.in_outer_pane(&format!("TMUX= tmux -S {sock} attach -t =home"));
    assert!(
        cli.wait_for(5, || client_sessions(cli) == ["a", "home"]),
        "{:?}",
        client_sessions(cli)
    );
    (a, artifact)
}

fn client_sessions(cli: &Cli) -> Vec<String> {
    let mut sessions: Vec<String> = cli
        .tmux_out(&["list-clients", "-F", "#{client_session}"])
        .lines()
        .map(str::to_owned)
        .collect();
    sessions.sort();
    sessions
}

/// The nested client showing `a` inside home's pane.
fn nested_client(cli: &Cli) -> String {
    cli.tmux_out(&["list-clients", "-t", "=a", "-F", "#{client_name}"])
}

fn outer_keys(cli: &Cli, keys: &[&str]) {
    let mut args = vec!["send-keys", "-t", "=outer:"];
    args.extend_from_slice(keys);
    let output = Command::new("tmux")
        .arg("-S")
        .arg(cli.outer_sock())
        .args(["-f", "/dev/null"])
        .args(&args)
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

fn running_pick(cli: &Cli) -> bool {
    let output = Command::new("ps")
        .args(["-axo", "command"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .any(|line| line.contains("_artifact-pick") && line.contains(cli.state().to_str().unwrap()))
}

/// Gate: prefix+a in an agent pane lists what it touched; Enter attaches the chosen artifact's
/// view in that pane.
#[test]
fn prefix_a_in_an_agent_pane_attaches_the_chosen_artifact() {
    require_tmux!();
    let cli = Cli::new();
    let (a, artifact) = operator(&cli);
    outer_keys(&cli, &["C-a", "a"]);
    assert!(
        cli.wait_for(10, || running_pick(&cli)),
        "the picker never opened"
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    outer_keys(&cli, &["Enter"]);
    assert!(
        cli.wait_for(10, || client_sessions(&cli) == ["home", "plan-md"]),
        "{:?}",
        client_sessions(&cli)
    );
    assert_eq!(view_option(&cli, "plan-md", "@tx-view-kind"), "editor");
    assert_eq!(view_option(&cli, "plan-md", "@tx-view-bound"), artifact);
    assert_eq!(view_option(&cli, "plan-md", "@tx-view-parent"), a);
    // The agent keeps running, detached.
    assert_ne!(common::state_of(&cli, "a"), "ended");

    // prefix+a in the view goes back to the controlling session, in the same pane.
    outer_keys(&cli, &["C-a", "a"]);
    assert!(
        cli.wait_for(10, || client_sessions(&cli) == ["a", "home"]),
        "{:?}",
        client_sessions(&cli)
    );

    // prefix+a on the same artifact again shows the view already open; no second one.
    outer_keys(&cli, &["C-a", "a"]);
    assert!(
        cli.wait_for(10, || running_pick(&cli)),
        "the picker never opened"
    );
    std::thread::sleep(std::time::Duration::from_millis(500));
    outer_keys(&cli, &["Enter"]);
    assert!(
        cli.wait_for(10, || client_sessions(&cli) == ["home", "plan-md"]),
        "{:?}",
        client_sessions(&cli)
    );
    let views = cli.ok(&["ui", "view", "ls"]);
    assert_eq!(views.matches("plan-md").count(), 1, "{views}");
}

/// Gate: a pick in focus mode moves the terminal to the pane already showing the view.
#[test]
fn artifact_go_focus_moves_to_the_pane_showing_the_view() {
    require_tmux!();
    let cli = Cli::new();
    let (a, artifact) = operator(&cli);
    ok_as(&cli, &a, &["artifact", "open", &artifact]);
    let sock = cli.sock().display().to_string();
    let nest = format!("bash -c 'set -m; TMUX= tmux -S {sock} attach -t =plan-md; exec sh'");
    assert!(
        cli.tmux(&["new-window", "-d", "-t", "=home:", &nest])
            .status
            .success()
    );
    assert!(
        cli.wait_for(5, || client_sessions(&cli) == ["a", "home", "plan-md"]),
        "{:?}",
        client_sessions(&cli)
    );
    let active = || cli.tmux_out(&["display-message", "-p", "-t", "=home:", "#{window_index}"]);
    let before = active();
    let client = nested_client(&cli);
    cli.ok(&["_artifact-go", "a", &client, "focus", "artifact", &artifact]);
    assert!(
        cli.wait_for(5, || active() != before),
        "window stayed {before}"
    );

    // Focus on a session no pane shows is reported, exit 1.
    cli.ok(&["_artifact-go", "a", &client, "focus", "session", &a]);
    let lone = cli.create("lone");
    let output = cli.run(
        &["_artifact-go", "a", &client, "focus", "session", &lone],
        &[],
        "",
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("lone: not attached to any pane"),
        "{output:?}"
    );
    // A view row: shown by name, reported once it is gone.
    cli.ok(&["_artifact-go", "a", &client, "focus", "view", "plan-md"]);
    let output = cli.run(
        &["_artifact-go", "a", &client, "focus", "view", "gone"],
        &[],
        "",
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("gone has ended"),
        "{output:?}"
    );
}

/// Gate: a pick in popup mode nest-attaches the view in a popup over the outer terminal; the
/// panes stay as they are.
#[test]
fn artifact_go_popup_nest_attaches_the_view() {
    require_tmux!();
    let cli = Cli::new();
    let (_a, artifact) = operator(&cli);
    let client = nested_client(&cli);
    let mut child = cli
        .command(
            &["_artifact-go", "a", &client, "popup", "artifact", &artifact],
            &[],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let on_view = || cli.tmux_out(&["list-clients", "-t", "=plan-md", "-F", "#{client_name}"]);
    assert!(
        cli.wait_for(10, || !on_view().is_empty()),
        "no popup client"
    );
    assert_eq!(client_sessions(&cli), ["a", "home", "plan-md"]);
    assert_eq!(
        cli.tmux_out(&["show-options", "-vt", "=plan-md:", "status"]),
        "on"
    );
    cli.tmux(&["detach-client", "-t", &on_view()]);
    let output = child.wait().unwrap();
    assert!(output.success());
    assert_eq!(client_sessions(&cli), ["a", "home"]);
    let _ = child.kill();
}

#[test]
fn the_menu_reports_a_session_it_does_not_know() {
    require_tmux!();
    let cli = Cli::new();
    cli.create("a");
    let output = cli.run(&["_artifact-menu", "nosuch", "/dev/null"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("prefix+a: not a tx session"),
        "{output:?}"
    );
    cli.fails(&["_artifact-go", "a", "c", "jump", "artifact", "x"], 2);
}
