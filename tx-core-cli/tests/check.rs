//! `tx check` for plugins: every problem and warning of the contract, one plugin of the test's
//! own per case, each seen through `tx check --plugins`.

mod common;

use std::path::PathBuf;

use common::Cli;

struct Outcome {
    code: i32,
    out: String,
    err: String,
}

impl Outcome {
    fn lines(&self) -> Vec<&str> {
        self.out.lines().collect()
    }
}

/// `tx <args>` in `cli`. Left out of what it printed: the `needs` warnings about the bundles of
/// the shared root. What a shipped plugin needs on `PATH` depends on the machine, so it cannot
/// be pinned; a warning about a plugin of the test's own stays.
fn check(cli: &Cli, args: &[&str]) -> Outcome {
    let output = cli.run(args, &[], "");
    let shipped = format!("{}/", common::shipped().display());
    let out = String::from_utf8(output.stdout)
        .unwrap()
        .split_inclusive('\n')
        .filter(|line| {
            !(line.starts_with(&shipped) && line.contains("/plugin.yaml: warning: needs "))
        })
        .collect();
    Outcome {
        code: output.status.code().unwrap(),
        out,
        err: String::from_utf8(output.stderr).unwrap(),
    }
}

/// A sandbox whose shipped stamp is this tx's: nothing in it is wrong.
fn clean() -> Cli {
    let cli = Cli::new();
    write_stamp(&cli, env!("CARGO_PKG_VERSION"));
    cli
}

fn write_stamp(cli: &Cli, version: &str) {
    let shipped = cli.state().join("shipped");
    std::fs::create_dir_all(&shipped).unwrap();
    std::fs::write(
        shipped.join(".stamp"),
        format!(
            r#"{{"tx_version": "{version}", "api": 1, "bundles": "/b", "binaries": "/c", "linked": false}}"#
        ),
    )
    .unwrap();
}

/// A plugin `t` of the test's own whose manifest is the three required lines and `more`, and
/// whose `bin/` holds `programs`.
fn plugin_t(cli: &Cli, more: &str, programs: &[(&str, &str)]) -> PathBuf {
    let manifest = format!("api: 1\nid: t\ndescription: test\n{more}");
    cli.plugin("t", &manifest, programs).join("plugin.yaml")
}

/// How many installed plugins load: the entries of `tx plugins --json` that are not in error or
/// missing.
fn loading(cli: &Cli) -> usize {
    let rows: serde_json::Value = serde_json::from_str(&cli.ok(&["plugins", "--json"])).unwrap();
    rows.as_array()
        .unwrap()
        .iter()
        .filter(|row| !matches!(row["status"].as_str().unwrap(), "error" | "missing"))
        .count()
}

#[test]
fn a_clean_sandbox_ends_with_the_plugin_count() {
    let cli = clean();
    let plugins = loading(&cli);
    let all = check(&cli, &["check"]);
    assert_eq!(all.code, 0, "{}", all.out);
    let last = all.lines().pop().unwrap().to_owned();
    assert!(
        last.starts_with("ok: ") && last.ends_with(&format!(", {plugins} plugins")),
        "{last}"
    );
    assert!(last.contains(" agents, ") && last.contains(" MCP servers, "));
    let only = check(&cli, &["check", "--plugins"]);
    assert_eq!(
        (only.code, only.out),
        (0, format!("ok: {plugins} plugins\n"))
    );
}

#[test]
fn an_unknown_key_is_the_parsers_own_message_with_its_line() {
    let cli = clean();
    let path = plugin_t(&cli, "colour: red\n", &[]);
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    let line = outcome.lines()[0];
    assert!(
        line.starts_with(&format!("{}:4: ", path.display())) && line.contains("colour"),
        "{line}"
    );
    assert!(!outcome.out.contains("ok:"));
}

#[test]
fn a_bad_tier_and_an_unsupported_api_are_reported() {
    let cli = clean();
    let path = plugin_t(&cli, "tier: huge\n", &[]);
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert!(
        outcome.out.starts_with(&format!("{}:4: ", path.display())) && outcome.out.contains("huge"),
        "{}",
        outcome.out
    );
    let path = plugin_t(&cli, "", &[]);
    std::fs::write(&path, "api: 2\nid: t\ndescription: test\nfuture: yes\n").unwrap();
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!("{}: api 2 is not supported (this tx: 1)\n", path.display())
    );
}

#[test]
fn a_directory_that_is_not_the_manifests_id_is_reported() {
    let cli = clean();
    let manifest = "api: 1\nid: y\ndescription: test\n";
    let path = cli.plugin("x", manifest, &[]).join("plugin.yaml");
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!(
            "{}: id \"y\" is not the name of its directory, \"x\"\n",
            path.display()
        )
    );
}

#[test]
fn a_second_copy_of_an_id_names_the_first() {
    let cli = clean();
    let copy = cli.plugins_dir().join("mockup");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::copy(
        common::shipped().join("mockup/plugin.yaml"),
        copy.join("plugin.yaml"),
    )
    .unwrap();
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!(
            "{}: plugin \"mockup\" is also installed in {}\n",
            copy.display(),
            common::shipped().join("mockup").display()
        )
    );
}

#[test]
fn a_verb_that_is_a_built_in_or_another_plugins_is_the_cores_own_wording() {
    let cli = clean();
    let verbs = "verbs:\n  - {name: create, usage: \"create\", bin: prog}\n  \
                 - {name: mockup, usage: \"mockup\", bin: prog}\n";
    let path = plugin_t(&cli, verbs, &[("prog", "exit 0")]);
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!(
            "{0}: verb \"create\" is a built-in\n{0}: verb \"mockup\" is already \"mockup\"'s\n",
            path.display()
        )
    );
}

#[test]
fn an_engine_kind_or_ext_key_another_plugin_has_goes_to_the_later_plugin() {
    let cli = clean();
    let programs = [("prog", "exit 0")];
    cli.plugin(
        "s",
        "api: 1\nid: s\ndescription: test\nengine: {kind: zz, ext_key: zz, bin: prog}\n",
        &programs,
    );
    let path = plugin_t(
        &cli,
        "engine: {kind: zz, ext_key: other, bin: prog}\n",
        &programs,
    );
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!(
            "{}: engine kind \"zz\" is already \"s\"'s\n",
            path.display()
        )
    );
    plugin_t(
        &cli,
        "engine: {kind: yy, ext_key: zz, bin: prog}\n",
        &programs,
    );
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!("{}: ext key \"zz\" is already \"s\"'s\n", path.display())
    );
}

#[test]
fn a_program_that_is_missing_or_not_executable_is_reported() {
    let cli = clean();
    let verb = "verbs:\n  - {name: tt, usage: \"tt\", bin: prog}\n";
    let path = plugin_t(&cli, verb, &[]);
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!("{}: bin/prog is missing\n", path.display())
    );
    let program = path.parent().unwrap().join("bin");
    std::fs::create_dir_all(&program).unwrap();
    std::fs::write(program.join("prog"), "#!/bin/sh\n").unwrap();
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!("{}: bin/prog is not executable\n", path.display())
    );
}

#[test]
fn a_resource_that_is_missing_is_reported() {
    let cli = clean();
    let path = plugin_t(&cli, "resources: [keys/t.tmux]\n", &[]);
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!("{}: resource keys/t.tmux is missing\n", path.display())
    );
}

#[test]
fn a_required_plugin_that_is_absent_disabled_or_broken_is_reported() {
    let cli = clean();
    let path = plugin_t(&cli, "requires: [nope]\n", &[]);
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!(
            "{}: requires \"nope\", which is not installed\n",
            path.display()
        )
    );
    plugin_t(&cli, "requires: [mockup]\n", &[]);
    cli.write_yaml("plugins:\n  mockup: { enabled: false }\n");
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!(
            "{}: requires \"mockup\", which is disabled\n",
            path.display()
        )
    );
    cli.write_yaml("");
    let broken = cli
        .plugin(
            "broken",
            "api: 1\nid: broken\ndescription: test\ncolour: red\n",
            &[],
        )
        .join("plugin.yaml");
    plugin_t(&cli, "requires: [broken]\n", &[]);
    let outcome = check(&cli, &["check", "--plugins", "t"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!(
            "{}: requires \"broken\", which does not load\n",
            path.display()
        )
    );
    let outcome = check(&cli, &["check", "--plugins"]);
    assert!(outcome.out.starts_with(&format!("{}:", broken.display())));
}

#[test]
fn an_engine_that_nothing_provides_is_reported_with_the_active_ones() {
    let cli = clean();
    cli.write_yaml("agent:\n  engine: kodex\n");
    let outcome = check(&cli, &["check"]);
    assert_eq!(outcome.code, 1);
    let config = cli.state().join("config.yaml");
    assert!(
        outcome.out.contains(&format!(
            "{}: agent.engine \"kodex\" is not an active engine (active: claude, codex)\n",
            config.display()
        )),
        "{}",
        outcome.out
    );
    assert!(!outcome.out.contains("ok:"));
}

#[test]
fn a_stamp_of_another_version_or_none_is_reported() {
    let cli = clean();
    write_stamp(&cli, "0.0.1");
    let shipped = cli.state().join("shipped");
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!(
            "{}: plugins are from tx 0.0.1, this is tx {}: run install --upgrade\n",
            shipped.display(),
            env!("CARGO_PKG_VERSION")
        )
    );
    std::fs::remove_dir_all(&shipped).unwrap();
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(
        outcome.out,
        format!(
            "{}: no shipped plugins are installed: run install\n",
            shipped.display()
        )
    );
}

#[test]
fn a_program_not_on_the_path_is_a_warning_and_the_run_is_clean() {
    let cli = clean();
    let path = plugin_t(&cli, "needs: [no-such-program-anywhere]\n", &[]);
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 0);
    let plugins = loading(&cli);
    assert_eq!(
        outcome.out,
        format!(
            "{}: warning: needs no-such-program-anywhere, which is not on PATH\nok: {plugins} plugins\n",
            path.display()
        )
    );
}

#[test]
fn an_entry_that_names_no_plugin_is_a_warning_and_a_retired_one_says_why() {
    let cli = clean();
    // `ide` is installed: its entry names a plugin.
    cli.write_yaml("plugins:\n  pikcer: {}\n  hooks: {}\n  ide: { enabled: true }\n");
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 0, "{}", outcome.out);
    let config = cli.state().join("config.yaml");
    let warnings: Vec<&str> = outcome
        .lines()
        .into_iter()
        .filter(|l| l.contains("warning"))
        .collect();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert_eq!(
        warnings[0],
        format!(
            "{}: warning: plugins.hooks: no such plugin (retired: agent state moved into core and the engine adapters)",
            config.display()
        )
    );
    assert_eq!(
        warnings[1],
        format!(
            "{}: warning: plugins.pikcer: no such plugin",
            config.display()
        )
    );
}

#[test]
fn a_resource_that_replaces_the_cores_is_a_warning() {
    let cli = clean();
    let path = plugin_t(&cli, "resources: [agents/COMMON.md]\n", &[]);
    std::fs::create_dir_all(path.parent().unwrap().join("agents")).unwrap();
    std::fs::write(path.parent().unwrap().join("agents/COMMON.md"), "mine\n").unwrap();
    let outcome = check(&cli, &["check", "--plugins"]);
    assert_eq!(outcome.code, 0);
    assert_eq!(
        outcome.lines()[0],
        format!(
            "{}: warning: agents/COMMON.md replaces \"tx-core\"'s",
            path.display()
        )
    );
    // A later plugin replaces the earlier plugin's, not the core's.
    let other = cli.plugin(
        "u",
        "api: 1\nid: u\ndescription: test\nresources: [agents/COMMON.md]\n",
        &[],
    );
    std::fs::create_dir_all(other.join("agents")).unwrap();
    std::fs::write(other.join("agents/COMMON.md"), "later\n").unwrap();
    let outcome = check(&cli, &["check", "--plugins"]);
    assert!(
        outcome.out.contains("agents/COMMON.md replaces \"t\"'s"),
        "{}",
        outcome.out
    );
}

#[test]
fn named_plugins_are_the_only_ones_checked_and_an_unknown_id_fails() {
    let cli = clean();
    plugin_t(&cli, "colour: red\n", &[]);
    let outcome = check(&cli, &["check", "--plugins", "mockup"]);
    assert_eq!(
        (outcome.code, outcome.out),
        (0, "ok: 1 plugins\n".to_owned())
    );
    let outcome = check(&cli, &["check", "--plugins", "nope"]);
    assert_eq!(outcome.code, 1);
    assert_eq!(outcome.out, "");
    assert_eq!(outcome.err, "tx check: no plugin \"nope\"\n");
}

#[test]
fn definitions_alone_say_nothing_about_plugins() {
    let cli = clean();
    plugin_t(&cli, "colour: red\n", &[]);
    for args in [
        &["check", "developer"][..],
        &["check", "--engine", "codex"][..],
    ] {
        let outcome = check(&cli, args);
        assert!(
            !outcome.out.contains("plugin.yaml"),
            "{args:?}: {}",
            outcome.out
        );
        let last = outcome.lines().pop().unwrap().to_owned();
        assert!(
            last.starts_with("ok: ") && last.ends_with(" MCP servers"),
            "{last}"
        );
    }
}

#[test]
fn plugins_and_an_engine_do_not_go_together() {
    let cli = clean();
    for args in [
        &["check", "--plugins", "--engine", "claude"][..],
        &["check", "--engine", "claude", "--plugins"][..],
    ] {
        let outcome = check(&cli, args);
        assert_eq!(outcome.code, 2, "{args:?}");
        assert!(
            outcome
                .err
                .starts_with("tx: --plugins takes no --engine\nusage: tx")
        );
    }
}

#[test]
fn a_check_writes_nothing() {
    let cli = clean();
    // The sandbox keeps all five directories in one, so the index would land in it.
    check(&cli, &["check"]);
    check(&cli, &["check", "--plugins"]);
    assert!(!cli.state().join("plugins-index.json").exists());
}
