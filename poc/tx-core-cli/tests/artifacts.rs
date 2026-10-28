//! The artifacts plugin through the binary: `txc artifact …`, `txc group`, and prefix+a.

mod common;

use std::path::PathBuf;
use std::process::{Command, Stdio};

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

/// `txc artifact create <file> --title <name>`; returns the new id.
fn create(cli: &Cli, actor: &str, name: &str, body: &str) -> String {
    let file = write_file(cli, name, body);
    let out = ok_as(cli, actor, &["artifact", "create", &file, "--title", name]);
    created_id(&out)
}

fn created_id(out: &str) -> String {
    out.split_whitespace()
        .nth(3)
        .filter(|_| out.starts_with("Created "))
        .unwrap_or_else(|| panic!("{out:?}"))
        .to_owned()
}

fn record(cli: &Cli, id: &str) -> serde_json::Value {
    let text = std::fs::read_to_string(data_dir(cli).join(format!("{id}.json"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
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
fn create_modify_show_and_ls() {
    let cli = Cli::new();
    let actor = "11111111-1111-4111-8111-111111111111";
    let id = create(&cli, actor, "plan.md", "a\nb\n");
    let dir = data_dir(&cli);
    assert_eq!(
        std::fs::read(dir.join(&id).join("plan.md")).unwrap(),
        b"a\nb\n"
    );
    let created = record(&cli, &id);
    assert_eq!(created["artifact_schema_version"], 4);
    assert_eq!(created["owner"], actor);
    assert_eq!(created["document"]["file"], "plan.md");

    let next = write_file(&cli, "next.md", "a\nc\n");
    let out = cli.ok(&["artifact", "modify", &id[..8], &next]);
    assert_eq!(out, format!("Modified document artifact {id}\n"));
    assert_eq!(
        std::fs::read(dir.join(&id).join("plan.md")).unwrap(),
        b"a\nc\n"
    );
    assert_ne!(record(&cli, &id)["modified_at"], created["modified_at"]);

    let show = cli.ok(&["artifact", "show", &id]);
    assert!(show.starts_with(&format!("plan.md  ({id})\n")), "{show}");
    assert!(show.contains("  document:   file=plan.md"), "{show}");
    assert!(show.contains("  group:      derived: "), "{show}");
    assert_eq!(
        cli.ok(&["artifact", "path", &id]),
        format!("{}\n", dir.join(&id).display())
    );

    let ls = cli.ok(&["artifact", "ls"]);
    assert!(ls.starts_with("ARTIFACTS\n  "), "{ls}");
    assert!(ls.contains(&id[..8]) && ls.contains("document"), "{ls}");
    assert!(
        cli.ok(&["artifact", "ls", "--session", actor])
            .contains(&id[..8])
    );
    assert!(
        cli.ok(&["artifact", "ls", "--kind", "diff"])
            .contains("(none)")
    );
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
fn archive_hides_from_ls_until_unarchived() {
    let cli = Cli::new();
    let id = create(&cli, "user", "plan.md", "x");
    let out = cli.ok(&["artifact", "archive", &id]);
    assert!(
        out.starts_with(&format!("Archived artifact {id} (plan.md)")),
        "{out}"
    );
    assert_eq!(cli.ok(&["artifact", "ls"]), "ARTIFACTS\n  (none)\n");
    assert!(cli.ok(&["artifact", "ls", "--archived"]).contains(&id[..8]));
    assert!(
        cli.ok(&["artifact", "show", &id])
            .contains("  archived:   ")
    );
    cli.ok(&["artifact", "unarchive", &id]);
    assert!(cli.ok(&["artifact", "ls"]).contains(&id[..8]));
    assert!(record(&cli, &id)["archived_at"].is_null());
}

#[test]
fn artifact_errors_exit_1_and_usage_exits_2() {
    let cli = Cli::new();
    let stderr = cli.fails(&["artifact", "show", "nope"], 1);
    assert_eq!(stderr, "txc artifact: artifact 'nope' not found\n");
    let stderr = cli.fails(&["artifact", "show", "../x"], 1);
    assert!(stderr.contains("not found"), "{stderr}");
    let stderr = cli.fails(&["artifact", "create", "/no/such/file", "--title", "t"], 2);
    assert!(stderr.contains("no such file: /no/such/file"), "{stderr}");
    let file = write_file(&cli, "p.md", "x");
    let stderr = cli.fails(&["artifact", "create", &file], 2);
    assert!(stderr.contains("--title is required"), "{stderr}");
    let stderr = cli.fails(&["artifact", "create", "--title", "t"], 2);
    assert!(
        stderr.contains("a document is created from a file"),
        "{stderr}"
    );
    let stderr = cli.fails(
        &[
            "artifact", "create", &file, "--kind", "diff", "--title", "t",
        ],
        2,
    );
    assert!(stderr.contains("a diff takes no file"), "{stderr}");
    cli.fails(
        &[
            "artifact", "create", &file, "--kind", "site", "--title", "t",
        ],
        2,
    );
    cli.fails(&["artifact", "frob"], 2);
}

#[test]
fn a_diff_pins_its_base_and_refuses_a_file() {
    let cli = Cli::new();
    let repo = cli.dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    let repo_arg = repo.to_str().unwrap();
    let out = cli.ok(&[
        "artifact", "create", "--kind", "diff", "--title", "review", "--cwd", repo_arg,
    ]);
    let id = created_id(&out);
    assert_eq!(record(&cli, &id)["diff"]["base"], base.as_str());
    let patch = std::fs::read_to_string(data_dir(&cli).join(&id).join("diff.patch")).unwrap();
    assert!(patch.contains("+two"), "{patch}");
    let show = cli.ok(&["artifact", "show", &id]);
    assert!(show.contains(&format!("base={base}")), "{show}");
    assert!(cli.ok(&["artifact", "ls"]).contains("diff"));

    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    cli.ok(&["artifact", "modify", &id]);
    let patch = std::fs::read_to_string(data_dir(&cli).join(&id).join("diff.patch")).unwrap();
    assert!(patch.contains("+three"), "{patch}");
    let file = write_file(&cli, "x.patch", "x");
    let stderr = cli.fails(&["artifact", "modify", &id, &file], 1);
    assert!(stderr.contains("is a diff"), "{stderr}");
}

#[test]
fn doctor_reports_a_missing_file_and_a_stray_directory() {
    let cli = Cli::new();
    let id = create(&cli, "user", "plan.md", "v0\n");
    let dir = data_dir(&cli);
    std::fs::remove_file(dir.join(&id).join("plan.md")).unwrap();
    std::fs::create_dir_all(dir.join("stray")).unwrap();
    let output = cli.run(&["artifact", "doctor"], &[], "");
    assert_eq!(output.status.code(), Some(1));
    let out = String::from_utf8_lossy(&output.stdout);
    assert!(
        out.contains(&format!("{id}: document file plan.md is missing")),
        "{out}"
    );
    assert!(
        out.contains("stray: content directory has no record"),
        "{out}"
    );
    assert!(out.ends_with("artifacts: 2 problem(s)\n"), "{out}");
    let stderr = cli.fails(&["artifact", "open", &id], 1);
    assert!(
        stderr.contains("document file plan.md is missing"),
        "{stderr}"
    );
}

#[test]
fn migrate_upgrades_a_v2_record_in_place() {
    let cli = Cli::new();
    let dir = data_dir(&cli);
    std::fs::create_dir_all(dir.join("old/revs")).unwrap();
    std::fs::write(dir.join("old/current.md"), "body").unwrap();
    std::fs::write(dir.join("old/revs/0.md"), "body").unwrap();
    let v2 = serde_json::json!({
        "artifact_schema_version": 2, "id": "old", "title": "Plan", "filename": "plan.md",
        "created_at": 1.0, "group": null,
        "history": [{"session_id": "user", "at": 2.0, "rev": 0, "changes": null}],
    });
    std::fs::write(dir.join("old.json"), v2.to_string()).unwrap();
    let output = cli.run(&["artifact", "ls"], &[], "");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("run `txc artifact migrate`"),
        "{output:?}"
    );
    assert_eq!(
        cli.ok(&["artifact", "migrate"]),
        "  migrated old.json\nartifacts: 1 migrated, 0 skipped\n"
    );
    assert_eq!(std::fs::read(dir.join("old/plan.md")).unwrap(), b"body");
    assert!(cli.ok(&["artifact", "ls"]).contains("Plan"));
    assert_eq!(cli.ok(&["artifact", "doctor"]), "artifacts: clean\n");
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

    // An artifact the worker owns takes its group.
    let artifact = create(&cli, &id, "p.md", "x");
    assert!(
        cli.ok(&["artifact", "show", &artifact])
            .contains("group:      derived: effort")
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
    let path = cli.state().join("sessions").join(format!("{id}.json"));
    let mut json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    json["ext"] = serde_json::json!({"artifacts": {"bound": 5}});
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
    let dir = data_dir(&cli).join(&id);
    let file = dir.join("plan.md");
    // The core canonicalizes the cwd (/tmp is /private/tmp on macOS).
    assert!(
        out.starts_with(&format!(
            "Opened document artifact {id} in view 'plan-md' (cwd "
        )) && out.ends_with(&format!("plugins/artifacts/{id})\n")),
        "{out}"
    );
    let view: serde_json::Value = serde_json::from_str(&cli.ok(&["show", "plan-md"])).unwrap();
    assert_eq!(view["ext"]["artifacts"]["bound"], id.as_str());
    assert_eq!(view["parent"], worker.as_str());
    assert_eq!(view["tags"], serde_json::json!(["feat-x"]));
    assert_eq!(view["launch"]["program"], editor.as_str());
    assert_eq!(view["launch"]["args"][0], file.to_str().unwrap());
    // A view takes its artifact's group.
    cli.ok(&["group", "worker", "effort"]);
    assert_eq!(
        cli.ok(&["group", "plan-md"]),
        "own:      —\nresolved: effort\n"
    );
    // A second open while the first runs gets a disambiguated name.
    let out = cli.ok(&["artifact", "open", &id, "--tag", "x,y"]);
    assert!(
        out.contains(&format!("in view 'plan-md-{}'", &id[..8])),
        "{out}"
    );
    let log = std::fs::read_to_string(data_dir(&cli).join(".log.jsonl")).unwrap();
    assert_eq!(log.matches("\"artifact-open\"").count(), 2, "{log}");

    // Stopping a bound view runs the close step its open left behind.
    let temp = data_dir(&cli).join(".scratch/worktrees/artifact-fake");
    std::fs::create_dir_all(&temp).unwrap();
    let cleanups = serde_json::json!({ view["id"].as_str().unwrap(): {"remove_worktree": temp} });
    std::fs::write(dir.join(".cleanup.json"), cleanups.to_string()).unwrap();
    cli.ok(&["kill", "plan-md"]);
    assert!(!temp.exists());
    assert!(!dir.join(".cleanup.json").exists());
}

/// A plugins.yaml whose editor is `script`.
fn editor_running(cli: &Cli, script: &str) {
    let path = cli.shim_path(&[("fake-nvim", script)]);
    let dir = path.split(':').next().unwrap();
    cli.write_yaml(&format!(
        "plugins:\n  artifacts:\n    settings: {{ editor: \"{dir}/fake-nvim\" }}\n"
    ));
}

/// A diff artifact on a feature worktree that is then removed, so an open needs a temp worktree
/// made from the sibling `main` checkout. Returns (artifact id, the temp worktrees' root).
fn gone_diff(cli: &Cli) -> (String, std::path::PathBuf) {
    let main = cli.dir.path().join("repos/main");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    git(&main, &["add", "a.txt"]);
    git(&main, &["commit", "-q", "-m", "base"]);
    let feature = cli.dir.path().join("repos/main--feature");
    let tree = feature.to_str().unwrap();
    git(&main, &["worktree", "add", "-q", "--detach", tree, "HEAD"]);
    std::fs::write(feature.join("a.txt"), "one\ntwo\n").unwrap();
    let out = cli.ok(&[
        "artifact", "create", "--kind", "diff", "--title", "review", "--cwd", tree, "--base",
        "main",
    ]);
    git(&main, &["worktree", "remove", "--force", tree]);
    (created_id(&out), data_dir(cli).join(".scratch/worktrees"))
}

fn temp_worktrees(root: &std::path::Path) -> usize {
    std::fs::read_dir(root).map_or(0, |entries| entries.count())
}

#[test]
fn a_gone_diff_opens_in_a_temp_worktree_that_close_removes() {
    require_tmux!();
    let cli = Cli::new();
    editor_running(&cli, "exec sleep 60");
    let (id, root) = gone_diff(&cli);
    let out = cli.ok(&["artifact", "open", &id]);
    assert!(out.contains("in view 'review-diff'"), "{out}");
    assert_eq!(temp_worktrees(&root), 1);
    assert_eq!(cli.ok(&["artifact", "doctor"]), "artifacts: clean\n");
    assert_eq!(
        cli.ok(&["artifact", "close", &id]),
        format!("Closed artifact {id}: 1 close step(s) run\n")
    );
    assert_eq!(temp_worktrees(&root), 0);
}

/// An editor that exits at once leaks no temp worktree, whether it ends before the close step is
/// recorded (the open runs it) or just after (the Exited sink runs it).
#[test]
fn an_editor_that_exits_at_once_leaks_no_temp_worktree() {
    require_tmux!();
    let cli = Cli::new();
    editor_running(&cli, "exit 0");
    let (id, root) = gone_diff(&cli);
    cli.ok(&["artifact", "open", &id]);
    // Either the open saw the view already ended, or the next refresh marks it Exited and the
    // end sink runs the step.
    assert!(
        cli.wait_for(5, || {
            cli.ok(&["refresh"]);
            temp_worktrees(&root) == 0
        }),
        "temp worktree left behind"
    );
    assert_eq!(cli.ok(&["artifact", "doctor"]), "artifacts: clean\n");
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
    let cwd = cli.dir.path().join("a");
    std::fs::create_dir_all(&cwd).unwrap();
    let output = cli.run(
        &[
            "create",
            "a",
            "--cwd",
            cwd.to_str().unwrap(),
            "--engine",
            "claude",
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

/// Gate: prefix+a in an agent pane lists what it owns; Enter attaches the chosen artifact's
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
    let view: serde_json::Value = serde_json::from_str(&cli.ok(&["show", "plan-md"])).unwrap();
    assert_eq!(view["ext"]["artifacts"]["bound"], artifact.as_str());
    assert_eq!(view["parent"], a.as_str());
    // The agent keeps running, detached.
    assert_eq!(common::lifecycle_of(&cli, "a"), "running");

    // prefix+a in the view goes back to the controlling session, in the same pane.
    outer_keys(&cli, &["C-a", "a"]);
    assert!(
        cli.wait_for(10, || client_sessions(&cli) == ["a", "home"]),
        "{:?}",
        client_sessions(&cli)
    );
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
