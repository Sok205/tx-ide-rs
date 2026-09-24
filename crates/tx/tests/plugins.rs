//! `config.json` `plugins`: the loader switches feature components on and off, and a broken
//! config never breaks a verb.

use std::path::Path;
use std::process::{Command, Output};

const ALL: [&str; 10] = [
    "engine.claude",
    "engine.codex",
    "engine.antigravity",
    "verbs.home",
    "verbs.sessions",
    "verbs.listing",
    "verbs.hooks",
    "verbs.chat",
    "verbs.artifacts",
    "verbs.install",
];

fn tx(home: &Path, config: Option<&str>, args: &[&str]) -> Output {
    let tx_home = home.join("tx-home");
    std::fs::create_dir_all(&tx_home).unwrap();
    if let Some(config) = config {
        std::fs::write(tx_home.join("config.json"), config).unwrap();
    }
    Command::new(env!("CARGO_BIN_EXE_tx"))
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("TX_IDE_HOME", &tx_home)
        .env("TMUX_TMPDIR", home)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn statuses(output: &Output) -> Vec<(String, String)> {
    text(&output.stdout)
        .lines()
        .map(|line| {
            let (id, status) = line.split_once(' ').unwrap();
            (id.to_owned(), status.trim().to_owned())
        })
        .collect()
}

#[test]
fn without_a_plugins_section_everything_is_active() {
    let dir = tempfile::tempdir().unwrap();
    let out = tx(dir.path(), None, &["_plugins"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let expected: Vec<(String, String)> = ALL
        .iter()
        .map(|id| ((*id).to_owned(), "active".to_owned()))
        .collect();
    assert_eq!(statuses(&out), expected);
    assert_eq!(text(&out.stderr), "");
}

#[test]
fn a_disabled_verb_group_is_gone_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    let config =
        r#"{"plugins": {"verbs.chat": {"disabled": true}, "engine.codex": {"disabled": true}}}"#;
    let fork = tx(dir.path(), Some(config), &["fork", "x"]);
    assert_eq!(fork.status.code(), Some(2));
    assert!(
        text(&fork.stderr).starts_with("tx: unknown command: fork\n"),
        "{}",
        text(&fork.stderr)
    );
    let help = tx(dir.path(), Some(config), &["--help"]);
    assert!(
        !text(&help.stdout).contains("\n  fork "),
        "fork left in help"
    );

    let listed = statuses(&tx(dir.path(), Some(config), &["_plugins"]));
    let status = |id: &str| {
        listed
            .iter()
            .find(|(known, _)| known == id)
            .map(|(_, s)| s.as_str())
    };
    assert_eq!(status("verbs.chat"), Some("disabled"));
    assert_eq!(status("engine.codex"), Some("disabled"));
    assert_eq!(status("engine.claude"), Some("active"));
}

#[test]
fn a_malformed_config_loads_the_defaults_silently() {
    // Only the reference's own config readers (reconcile, sync) may fail on a broken file.
    let dir = tempfile::tempdir().unwrap();
    let out = tx(dir.path(), Some("{oops"), &["_init-home"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stderr), "");
}

#[test]
fn a_bad_plugins_section_warns_and_keeps_going() {
    let dir = tempfile::tempdir().unwrap();
    let cases = [
        (
            r#"{"plugins": []}"#,
            "tx: config.json: 'plugins' must be an object; using the defaults\n",
        ),
        (
            r#"{"plugins": {"ghost": {}}}"#,
            "tx: config.json plugins: unknown component `ghost`\n",
        ),
        (
            r#"{"plugins": {"verbs.chat": {"disabled": "yes"}}}"#,
            "tx: config.json: plugins.verbs.chat.disabled must be true or false; ignoring it\n",
        ),
        (
            r#"{"plugins": {"verbs.chat": true}}"#,
            "tx: config.json: plugins.verbs.chat must be an object; ignoring it\n",
        ),
    ];
    for (config, warning) in cases {
        let out = tx(dir.path(), Some(config), &["_init-home"]);
        assert!(out.status.success(), "{config}: {}", text(&out.stderr));
        assert_eq!(text(&out.stderr), warning, "config: {config}");
    }
}
