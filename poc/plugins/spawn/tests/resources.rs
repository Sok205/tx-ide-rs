//! The spawn plugin's resources, listed through the core.

use tx_core::{CoreConfig, PluginConfig, TxCore};
use tx_spawn::SpawnPlugin;

fn core(config: &PluginConfig) -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("txc")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
        dir.path().join("state"),
    )))
    .unwrap();
    core.plug(vec![Box::new(SpawnPlugin)], config).unwrap();
    (dir, core)
}

#[test]
fn the_key_fragment_declares_its_key() {
    let (_dir, core) = core(&PluginConfig::default());
    let keys = core.resources("keys/");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].plugin, "spawn");
    assert_eq!(keys[0].name, "keys/spawn.tmux");
    let text = std::str::from_utf8(keys[0].bytes).unwrap();
    assert!(text.lines().any(|line| line == "# keys: n"), "{text}");
    assert!(text.contains("$DATA/bin/tmux-new-agent"), "{text}");
}

#[test]
fn the_helper_script_is_a_script() {
    let (_dir, core) = core(&PluginConfig::default());
    let bin = core.resources("bin/");
    assert_eq!(bin.len(), 1);
    assert_eq!(bin[0].name, "bin/tmux-new-agent");
    assert!(bin[0].bytes.starts_with(b"#!"));
    assert!(
        std::str::from_utf8(bin[0].bytes)
            .unwrap()
            .contains("_new-agent")
    );
}

#[test]
fn the_shipped_roles_and_their_skills_are_resources() {
    let (_dir, core) = core(&PluginConfig::default());
    let names: Vec<String> = core
        .resources("agents/")
        .into_iter()
        .map(|resource| resource.name)
        .collect();
    assert_eq!(
        names,
        [
            "agents/COMMON.md",
            "agents/DEVELOPER.md",
            "agents/ORCHESTRATOR.md",
            "agents/QA.md",
            "agents/skills/tx-artifacts/SKILL.md",
            "agents/skills/tx-code-tours/SKILL.md",
            "agents/skills/tx-code-tours/scripts/apply_tour.py",
            "agents/skills/tx-diff-review/SKILL.md",
            "agents/skills/tx-sessions/SKILL.md",
        ]
    );
}

#[test]
fn a_disabled_plugin_publishes_nothing() {
    let mut config = PluginConfig::default();
    config.disabled.insert("spawn".to_owned());
    let (_dir, core) = core(&config);
    assert!(core.resources("").is_empty());
}

fn bare_tx() -> regex::Regex {
    regex::Regex::new(r"(^\s*(?:[-*]\s+|\d+\.\s+)?|`|\$\()tx\s").unwrap()
}

/// A bare `tx ` command word: at a line's start (after indentation or a list marker), after a
/// backtick, or inside `$(`. The shipped roles and skills are the txc editions and must name
/// `txc` commands only; `tx:` frontmatter and `tx-…` skill names are not command words.
#[test]
fn the_shipped_roles_and_skills_name_no_tx_command() {
    let bare_tx = bare_tx();
    let (_dir, core) = core(&PluginConfig::default());
    let resources = core.resources("agents/");
    assert!(!resources.is_empty());
    let offending: Vec<String> = resources
        .iter()
        .flat_map(|resource| {
            std::str::from_utf8(resource.bytes)
                .unwrap()
                .lines()
                .enumerate()
                .filter(|(_, line)| bare_tx.is_match(line))
                .map(|(index, line)| format!("{}:{}: {line}", resource.name, index + 1))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        offending.is_empty(),
        "bare tx commands:\n{}",
        offending.join("\n")
    );
}

#[test]
fn the_checker_catches_a_bare_tx_command() {
    let bare_tx = bare_tx();
    for line in [
        "tx spawn worker --role DEVELOPER",
        "  tx artifact modify <id>",
        "- tx ls",
        "2. tx kill it",
        "run `tx send-message orchestrator`",
        "SELF=$(tx whoami)",
    ] {
        assert!(bare_tx.is_match(line), "{line}");
    }
    for line in [
        "tx:",
        "  skills: [tx-sessions, tx-artifacts]",
        "txc create worker --prompt hi",
        "`txc artifact path <id>`",
        "SELF=$(txc whoami)",
        "the tx-tour runtime",
    ] {
        assert!(!bare_tx.is_match(line), "{line}");
    }
}
