//! `install --dry-run` against a throwaway `$HOME`, from an empty environment: which plugins a
//! run would select, build and install, what it would do to the dotfiles and to `config.yaml`,
//! and which old hook entries it would remove (what matches, not how the file is formatted).
//! A dry run executes nothing (no cargo, no `tx`, no tmux server) and the script stops without
//! `cargo`, `tmux` or `jq` on `PATH`, so every test skips itself when one is missing.

use std::path::{Path, PathBuf};
use std::process::Command;

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn script() -> PathBuf {
    repository().join("install")
}

/// Whether the tools the script requires are there; says so when they are not.
fn tools() -> bool {
    let found = ["jq", "cargo", "tmux"].iter().all(|tool| {
        Command::new("which")
            .arg(tool)
            .output()
            .is_ok_and(|output| output.status.success())
    });
    if !found {
        eprintln!("jq, cargo or tmux not available; skipping");
    }
    found
}

fn home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("tx-install")
        .tempdir_in("/tmp")
        .unwrap()
}

/// What one run of the script printed, with the ANSI colouring stripped, and its exit code.
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn plain(text: &[u8]) -> String {
    let text = String::from_utf8_lossy(text);
    let mut plain = String::new();
    let mut rest = text.as_ref();
    while let Some(start) = rest.find('\u{1b}') {
        plain.push_str(&rest[..start]);
        rest = &rest[start..];
        match rest.find('m') {
            Some(end) => rest = &rest[end + 1..],
            None => rest = "",
        }
    }
    plain.push_str(rest);
    plain
}

/// Runs `script` with `flags`. The environment is empty but for `PATH` and the throwaway home:
/// the script obeys tx's own variables before `$HOME`, and the session that runs the tests may
/// export the real ones.
fn install_at(script: &Path, home: &Path, flags: &[&str]) -> Run {
    let output = Command::new("bash")
        .arg(script)
        .args(flags)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", home)
        .env("CARGO_INSTALL_ROOT", home.join("cargo"))
        .output()
        .unwrap();
    Run {
        code: output.status.code(),
        stdout: plain(&output.stdout),
        stderr: plain(&output.stderr),
    }
}

/// The output of a dry run with `flags`, which must succeed.
fn dry_run(home: &Path, flags: &[&str]) -> String {
    let mut arguments = vec!["--dry-run"];
    arguments.extend(flags);
    let run = install_at(&script(), home, &arguments);
    assert_eq!(run.code, Some(0), "{}\n{}", run.stdout, run.stderr);
    run.stdout
}

/// The checkout's bundles as `(id, tier)`, in id order: every `plugins/<id>/plugin.yaml`, with
/// the tier its `tier:` line names (`extra` without one).
fn bundles() -> Vec<(String, String)> {
    let mut bundles: Vec<(String, String)> = std::fs::read_dir(repository().join("plugins"))
        .unwrap()
        .filter_map(|entry| {
            let entry = entry.unwrap();
            let manifest = std::fs::read_to_string(entry.path().join("plugin.yaml")).ok()?;
            let tier = manifest
                .lines()
                .find_map(|line| line.strip_prefix("tier:"))
                .map_or("extra", str::trim);
            Some((
                entry.file_name().to_string_lossy().into_owned(),
                tier.to_owned(),
            ))
        })
        .collect();
    bundles.sort();
    bundles
}

fn of_tiers(tiers: &[&str]) -> Vec<String> {
    bundles()
        .into_iter()
        .filter(|(_, tier)| tiers.contains(&tier.as_str()))
        .map(|(id, _)| id)
        .collect()
}

/// The plugins a dry run would build: the `-p tx-<id>` of its `cargo build` line after
/// `-p tx-core-cli`, sorted.
fn built(output: &str) -> Vec<String> {
    let line = output
        .lines()
        .find(|line| line.contains("would run: cargo build") && line.contains("-p tx-core-cli"))
        .unwrap_or_else(|| panic!("the build line in:\n{output}"));
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut ids: Vec<String> = words
        .windows(2)
        .filter(|pair| pair[0] == "-p" && pair[1] != "tx-core-cli")
        .map(|pair| pair[1].strip_prefix("tx-").unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

/// Whether a step line (`→ <subject> … <verdict>`) about `subject` says `verdict`.
fn reported(output: &str, subject: &str, verdict: &str) -> bool {
    output.lines().any(|line| {
        line.trim_start()
            .strip_prefix("→ ")
            .is_some_and(|rest| rest.starts_with(&format!("{subject} ")) && rest.contains(verdict))
    })
}

fn entries(directory: &Path) -> usize {
    std::fs::read_dir(directory).unwrap().count()
}

/// Records `selection` as the last install would have, under the home's data directory.
fn record(home: &Path, selection: &str) {
    let shipped = home.join(".local/share/tx-ide/shipped");
    std::fs::create_dir_all(&shipped).unwrap();
    std::fs::write(shipped.join(".selection"), selection).unwrap();
}

const FOREIGN: &str = "$HOME/.claude/hooks/require-worktree/check.sh";
const OURS_WORK: &str = "/Users/someone/.tx-ide/hooks/claude/work.sh";
const OURS_POST: &str = "/Users/someone/.tx-ide/hooks/claude/post.sh";
const OURS_STATUSLINE: &str = "bash /Users/someone/.tx-ide/statusline.sh";

#[test]
fn a_dry_run_names_full_and_every_bundle_and_changes_nothing() {
    if !tools() {
        return;
    }
    let home = home();
    let output = dry_run(home.path(), &[]);

    assert!(reported(&output, "preset", "full"), "{output}");
    let every: Vec<String> = bundles().into_iter().map(|(id, _)| id).collect();
    assert!(!every.is_empty());
    for (id, tier) in bundles() {
        assert!(reported(&output, &id, &tier), "{id} ({tier}) in:\n{output}");
    }
    assert_eq!(built(&output), every);
    assert!(
        output.contains("would run: cargo build --release")
            && output.contains("-p tx-setup")
            && output.contains("tx-setup --bundles"),
        "{output}"
    );
    let update = output
        .lines()
        .find(|line| line.contains("/tx update --bundles "))
        .unwrap_or_else(|| panic!("the update line in:\n{output}"));
    assert!(
        update.contains("would run: ") && update.contains("/plugins --binaries "),
        "{update}"
    );
    assert!(update.ends_with("/release"), "{update}");
    // tx is installed from the versions Cargo.lock pins, as the plugins' programs are built.
    assert!(
        output
            .lines()
            .any(|line| line.contains("would run: cargo install --path ")
                && line.ends_with("/tx-core-cli --force --locked")),
        "{output}"
    );
    assert!(
        output.contains("not linked (use --link-dotfiles)"),
        "{output}"
    );
    assert!(!output.contains("would link"), "{output}");
    assert_eq!(entries(home.path()), 0, "a dry run writes nothing");
}

#[test]
fn a_shipped_preset_is_named_from_the_tiers() {
    if !tools() {
        return;
    }
    let home = home();
    for (preset, tiers) in [
        ("headless", &["core"][..]),
        ("ide", &["core", "ide"]),
        ("full", &["core", "ide", "extra"]),
    ] {
        let output = dry_run(home.path(), &["--preset", preset]);
        assert!(reported(&output, "preset", preset), "{output}");
        assert_eq!(built(&output), of_tiers(tiers), "{preset}:\n{output}");
        assert!(
            output.contains("tx-setup --bundles ")
                && output.contains(&format!(" --preset {preset}")),
            "{output}"
        );
    }
    // `setup` is tier core: every preset takes the plugin that resolves them.
    assert!(of_tiers(&["core"]).contains(&"setup".to_owned()));
}

#[test]
fn a_list_of_plugins_is_named_as_given() {
    if !tools() {
        return;
    }
    let home = home();
    let output = dry_run(home.path(), &["--plugins", "setup,not-a-bundle"]);
    assert!(reported(&output, "plugins", "as given"), "{output}");
    assert_eq!(built(&output), ["not-a-bundle", "setup"]);
    assert!(reported(&output, "setup", "core"), "{output}");
    assert!(
        reported(&output, "not-a-bundle", "no bundle in")
            && reported(&output, "not-a-bundle", "/plugins: tx setup will refuse it"),
        "{output}"
    );
    assert!(output.contains(" --plugins setup,not-a-bundle"), "{output}");
}

/// Step 6 reads the `needs` of the selected plugins' manifests, in both ways of writing a list,
/// and names each program that `PATH` does not have with the plugin that needs it.
#[test]
fn the_programs_the_selected_plugins_need_are_looked_up() {
    if !tools() {
        return;
    }
    let home = home();
    // A checkout of its own: the script, and three bundles.
    let checkout = home.path().join("checkout");
    for (id, needs) in [
        ("flow", "needs: [sh, tx-no-such-program-a]   # a comment\n"),
        (
            "block",
            "needs:\n  - tx-no-such-program-b\n  - \"sh\"\nrequires: [flow]\n",
        ),
        ("other", "needs: [tx-no-such-program-c]\n"),
    ] {
        let bundle = checkout.join("plugins").join(id);
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(
            bundle.join("plugin.yaml"),
            format!("api: 1\nid: {id}\ndescription: The {id} plugin.\ntier: core\n{needs}"),
        )
        .unwrap();
    }
    let copy = checkout.join("install");
    std::fs::copy(script(), &copy).unwrap();

    let run = install_at(
        &copy,
        home.path(),
        &["--dry-run", "--plugins", "flow,block"],
    );
    assert_eq!(run.code, Some(0), "{}\n{}", run.stdout, run.stderr);
    let output = run.stdout;
    assert!(
        reported(&output, "tx-no-such-program-a", "flow needs it"),
        "{output}"
    );
    assert!(
        reported(&output, "tx-no-such-program-b", "block needs it"),
        "{output}"
    );
    // What is on PATH is not named, and neither is what an unselected plugin needs.
    assert_eq!(output.matches("needs it").count(), 2, "{output}");

    let run = install_at(&copy, home.path(), &["--dry-run", "--plugins", "other"]);
    assert!(
        reported(&run.stdout, "tx-no-such-program-c", "other needs it"),
        "{}",
        run.stdout
    );
}

/// `tx doctor` is the chat plugin's verb: the step runs only when chat is selected.
#[test]
fn the_doctor_runs_only_when_chat_is_selected() {
    if !tools() {
        return;
    }
    let home = home();
    let output = dry_run(home.path(), &["--plugins", "setup"]);
    assert!(output.contains("chat is not selected: skipped"), "{output}");
    assert!(!output.contains("doctor --fix-hooks\n  →"), "{output}");

    let output = dry_run(home.path(), &["--plugins", "setup,chat"]);
    assert!(
        output
            .lines()
            .any(|line| line.contains("would run: ") && line.ends_with("/tx doctor --fix-hooks")),
        "{output}"
    );
    assert!(!output.contains("chat is not selected"), "{output}");
}

#[test]
fn a_preset_of_your_own_is_left_to_tx_setup() {
    if !tools() {
        return;
    }
    let home = home();
    let output = dry_run(home.path(), &["--preset", "mine"]);
    assert!(
        output.contains("would resolve preset mine with tx setup"),
        "{output}"
    );
    assert_eq!(built(&output), ["<id>"], "no plugin is named: {output}");
    assert!(
        output.contains("doctor --fix-hooks, when chat is selected"),
        "{output}"
    );
}

#[test]
fn a_dry_upgrade_follows_the_recorded_selection() {
    if !tools() {
        return;
    }
    let every: Vec<String> = bundles().into_iter().map(|(id, _)| id).collect();

    // Nothing recorded: `full`.
    let home = home();
    let output = dry_run(home.path(), &["--upgrade"]);
    assert!(reported(&output, "preset", "full"), "{output}");
    assert_eq!(built(&output), every);
    assert!(
        output.contains("tx-setup --bundles ") && output.contains(" --upgrade"),
        "{output}"
    );

    // A shipped preset is resolved again, whatever list was recorded with it.
    record(home.path(), r#"{"preset": "headless", "plugins": []}"#);
    let output = dry_run(home.path(), &["--upgrade"]);
    assert!(reported(&output, "preset", "headless"), "{output}");
    assert_eq!(built(&output), of_tiers(&["core"]));

    // A list is kept, without what has no bundle any more.
    record(
        home.path(),
        r#"{"preset": null, "plugins": ["not-a-bundle", "setup"]}"#,
    );
    let output = dry_run(home.path(), &["--upgrade"]);
    assert_eq!(built(&output), ["setup"]);
    assert!(
        reported(&output, "not-a-bundle", "tx setup will drop it"),
        "{output}"
    );

    // A preset of the user's is tx setup's to resolve.
    record(home.path(), r#"{"preset": "mine", "plugins": ["setup"]}"#);
    let output = dry_run(home.path(), &["--upgrade"]);
    assert!(
        output.contains("would resolve preset mine with tx setup"),
        "{output}"
    );

    // An upgrade never links, and says how to.
    assert!(
        output.contains("not linked (use --link-dotfiles)"),
        "{output}"
    );
}

#[test]
fn flags_that_contradict_each_other_are_a_usage_error() {
    let home = home();
    for flags in [
        &["--nope"][..],
        &["--preset", "a", "--plugins", "b"],
        &["--preset", "a", "--upgrade"],
        &["--plugins", "b", "--upgrade"],
        &["--upgrade", "--link-dotfiles"],
        &["--dry-run", "--upgrade", "--link-dotfiles"],
        &["--preset"],
        &["--dry-run", "--plugins"],
    ] {
        let run = install_at(&script(), home.path(), flags);
        assert_eq!(run.code, Some(2), "{flags:?}: {}", run.stdout);
        assert!(run.stdout.is_empty(), "{flags:?}: {}", run.stdout);
        assert!(
            run.stderr.starts_with("install: ") && run.stderr.contains("usage: install "),
            "{flags:?}: {}",
            run.stderr
        );
    }
    assert_eq!(entries(home.path()), 0, "nothing is built or written");
}

#[test]
fn the_dotfiles_are_linked_only_when_asked() {
    if !tools() {
        return;
    }
    let home = home();
    let output = dry_run(home.path(), &["--link-dotfiles"]);
    let nvim = home.path().join(".config/nvim");
    let tmux = home.path().join(".tmux.conf");
    for (link, target) in [(&nvim, "/config/nvim"), (&tmux, "/config/tmux.conf")] {
        assert!(
            reported(&output, &link.display().to_string(), "would link to ")
                && output.lines().any(|line| line.ends_with(target)),
            "{}:\n{output}",
            link.display()
        );
    }
    assert_eq!(output.matches("would link").count(), 2, "{output}");
    assert!(!output.contains("not linked"), "{output}");
    assert!(!nvim.exists() && !tmux.exists(), "a dry run links nothing");
}

#[test]
fn a_compact_file_without_old_entries_is_not_written() {
    if !tools() {
        return;
    }
    let home = home();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    std::fs::create_dir_all(home.path().join(".codex")).unwrap();
    let settings = home.path().join(".claude/settings.json");
    let compact = format!(
        "{{\"theme\":\"dark\",\"hooks\":{{\"PreToolUse\":[{{\"matcher\":\"\",\"hooks\":[{{\"type\":\"command\",\"command\":\"{FOREIGN}\"}}]}}]}}}}"
    );
    std::fs::write(&settings, &compact).unwrap();
    std::fs::write(home.path().join(".codex/hooks.json"), "{\"hooks\":{}}").unwrap();

    let output = dry_run(home.path(), &[]);

    for file in [".claude/settings.json", ".codex/hooks.json"] {
        // The step's own line, not the header that names both files.
        let line = output
            .lines()
            .find(|line| line.trim_start().starts_with('→') && line.contains(file))
            .unwrap_or_else(|| panic!("a step line about {file} in:\n{output}"));
        assert!(
            line.contains("no old entries — not written"),
            "{file} untouched: {line}"
        );
    }
    assert!(!output.contains("would remove"), "{output}");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), compact);
    let backups = std::fs::read_dir(home.path().join(".claude"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".bak.")
        })
        .count();
    assert_eq!(backups, 0, "a dry run writes no backup either");
}

#[test]
fn a_compact_file_with_old_entries_lists_what_goes_and_keeps_the_rest() {
    if !tools() {
        return;
    }
    let home = home();
    std::fs::create_dir_all(home.path().join(".claude")).unwrap();
    let settings = home.path().join(".claude/settings.json");
    let compact = format!(
        "{{\"statusLine\":{{\"type\":\"command\",\"command\":\"{OURS_STATUSLINE}\"}},\
         \"hooks\":{{\"PreToolUse\":[{{\"matcher\":\"\",\"hooks\":[\
         {{\"type\":\"command\",\"command\":\"{FOREIGN}\"}},\
         {{\"type\":\"command\",\"command\":\"{OURS_WORK}\"}}]}}],\
         \"Stop\":[{{\"hooks\":[{{\"type\":\"command\",\"command\":\"{OURS_POST}\"}}]}}]}}}}"
    );
    std::fs::write(&settings, &compact).unwrap();

    let output = dry_run(home.path(), &[]);

    assert!(output.contains("would remove these entries"), "{output}");
    assert!(
        output.contains(&format!("- PreToolUse: {OURS_WORK}")),
        "{output}"
    );
    assert!(output.contains(&format!("- Stop: {OURS_POST}")), "{output}");
    assert!(
        !output.contains("- statusLine:"),
        "the statusLine stays: {output}"
    );
    assert!(
        !output.contains(&format!("- PreToolUse: {FOREIGN}")),
        "{output}"
    );
    // The diff it prints keeps the foreign entry and the statusLine.
    assert!(
        output.contains(&format!("+            \"command\": \"{FOREIGN}\"")),
        "{output}"
    );
    assert!(output.contains("+  \"statusLine\""), "{output}");
    // A dry run changes nothing.
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), compact);
}

/// A link into an agent's worktree would go with the worktree: the script refuses to link from
/// one, and only to link.
#[test]
fn an_agent_worktree_is_refused_only_when_linking() {
    if !tools() {
        return;
    }
    let home = home();
    // A copy of the script inside what the data dir calls an agent worktree.
    let data = home.path().join(".local/share/tx-ide");
    let worktree = data.join("worktrees/repo-0123abcd/repo--agent");
    std::fs::create_dir_all(&worktree).unwrap();
    let copy = worktree.join("install");
    std::fs::copy(script(), &copy).unwrap();

    let run = install_at(&copy, home.path(), &["--dry-run", "--link-dotfiles"]);
    assert_eq!(run.code, Some(1), "{}", run.stdout);
    assert!(
        run.stdout.contains("run install from the main checkout"),
        "{}",
        run.stdout
    );
    for later in ["tx setup --bundles", "Rename the old install"] {
        assert!(
            !run.stdout.contains(later),
            "stops before anything else: {}",
            run.stdout
        );
    }

    let run = install_at(&copy, home.path(), &["--dry-run"]);
    assert_eq!(run.code, Some(0), "{}\n{}", run.stdout, run.stderr);
    assert!(
        run.stdout.contains("not linked (use --link-dotfiles)"),
        "{}",
        run.stdout
    );
}

/// `config.yaml` is tx setup's to write, once: the script only reports. `--upgrade` never
/// writes it.
#[test]
fn the_config_file_is_reported_and_never_written() {
    if !tools() {
        return;
    }
    let home = home();
    let file = home.path().join(".config/tx-ide/config.yaml");
    let subject = file.display().to_string();

    let output = dry_run(home.path(), &[]);
    assert!(
        reported(&output, &subject, "would be written by tx setup"),
        "{output}"
    );
    let output = dry_run(home.path(), &["--upgrade"]);
    assert!(
        reported(&output, &subject, "absent: --upgrade does not write it"),
        "{output}"
    );
    assert!(!file.exists(), "a dry run writes nothing");

    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "agent: { model: sonnet }\n").unwrap();
    for flags in [&[][..], &["--upgrade"]] {
        let output = dry_run(home.path(), flags);
        assert!(
            reported(&output, &subject, "exists: left as it is"),
            "{output}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "agent: { model: sonnet }\n"
    );
}
