//! The roles and harness the core ships (`tx_core::roles`): listed first, under the plugin id
//! `tx-core`, written for `tx`.

use tx_core::{CoreConfig, PluginConfig, TxCore, roles};

fn core() -> (tempfile::TempDir, TxCore) {
    let dir = tempfile::Builder::new()
        .prefix("tx")
        .tempdir_in("/tmp")
        .unwrap();
    let mut core = TxCore::open(CoreConfig::new(tx_core::Directories::under(
        dir.path().join("state"),
    )))
    .unwrap();
    core.plug(vec![], &PluginConfig::default()).unwrap();
    (dir, core)
}

#[test]
fn the_shipped_roles_and_harness_are_resources_of_the_core() {
    let (_dir, core) = core();
    let resources = core.resources("");
    let names: Vec<&str> = resources.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "agents/COMMON.md",
            "agents/COMMON.local.md",
            "agents/TX-ASSISTANT.md",
            "harness/agents/claude.md",
            "harness/agents/developer.md",
            "harness/agents/layout-author.md",
            "harness/agents/orchestrator.md",
            "harness/agents/qa.md",
            "harness/mcp.json",
            "harness/skills/batch/SKILL.md",
            "harness/skills/built-in-browser/SKILL.md",
            "harness/skills/chrome-browser/SKILL.md",
            "harness/skills/claude-api/SKILL.md",
            "harness/skills/code-review/SKILL.md",
            "harness/skills/computer-use/SKILL.md",
            "harness/skills/dataviz/SKILL.md",
            "harness/skills/debug/SKILL.md",
            "harness/skills/deep-research/SKILL.md",
            "harness/skills/docs/SKILL.md",
            "harness/skills/doctor/SKILL.md",
            "harness/skills/docx/SKILL.md",
            "harness/skills/fewer-permission-prompts/SKILL.md",
            "harness/skills/google-workspace/SKILL.md",
            "harness/skills/import-memory/SKILL.md",
            "harness/skills/init/SKILL.md",
            "harness/skills/keybindings-help/SKILL.md",
            "harness/skills/loop/SKILL.md",
            "harness/skills/morning/SKILL.md",
            "harness/skills/pdf/SKILL.md",
            "harness/skills/plugin-authoring/SKILL.md",
            "harness/skills/pptx/SKILL.md",
            "harness/skills/run-skill-generator/SKILL.md",
            "harness/skills/run/SKILL.md",
            "harness/skills/security-review/SKILL.md",
            "harness/skills/simplify/SKILL.md",
            "harness/skills/skill-creator/SKILL.md",
            "harness/skills/tx-agents/SKILL.md",
            "harness/skills/tx-agents/templates/SKILL.md",
            "harness/skills/tx-agents/templates/agent.md",
            "harness/skills/tx-agents/templates/mcp.json",
            "harness/skills/tx-artifacts/SKILL.md",
            "harness/skills/tx-code-tours/SKILL.md",
            "harness/skills/tx-code-tours/scripts/apply_tour.py",
            "harness/skills/tx-diff-review/SKILL.md",
            "harness/skills/tx-mockup/SKILL.md",
            "harness/skills/tx-sessions/SKILL.md",
            "harness/skills/update-config/SKILL.md",
            "harness/skills/verify/SKILL.md",
            "harness/skills/workflow-authoring/SKILL.md",
            "harness/skills/xlsx/SKILL.md",
        ]
    );
    assert!(resources.iter().all(|r| r.plugin == roles::PLUGIN_ID));
    assert_eq!(names.len(), roles::SHIPPED.len());
    assert!(core.plugin_resources("").is_empty(), "no plugin was loaded");
    assert_eq!(core.resources("").len(), roles::SHIPPED.len());
}

#[test]
fn every_file_of_the_roles_and_harness_trees_is_shipped() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut expected = Vec::new();
    fn files(dir: &std::path::Path, root: &std::path::Path, prefix: &str, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files(&path, root, prefix, out);
            } else {
                let relative = path.strip_prefix(root).unwrap().to_string_lossy();
                out.push(format!("{prefix}/{relative}"));
            }
        }
    }
    let roles = manifest.join("agents");
    files(&roles, &roles, "agents", &mut expected);
    let harness = manifest.join("../config/tx");
    files(&harness, &harness, "harness", &mut expected);
    expected.sort();
    let mut shipped: Vec<String> = roles::SHIPPED
        .iter()
        .map(|(n, _)| (*n).to_owned())
        .collect();
    shipped.sort();
    assert_eq!(shipped, expected);
}
