//! The roles and definitions the core ships (`tx_core::roles`): listed first, under the plugin id
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
fn the_shipped_roles_and_definitions_are_resources_of_the_core() {
    let (_dir, core) = core();
    let resources = core.resources("");
    let names: Vec<&str> = resources.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "agents/COMMON.md",
            "agents/COMMON.local.md",
            "agents/DEVELOPER.md",
            "agents/ORCHESTRATOR.md",
            "agents/QA.md",
            "agents/TX-ASSISTANT.md",
            "definitions/agents/claude.md",
            "definitions/mcp.json",
            "definitions/skills/batch/SKILL.md",
            "definitions/skills/built-in-browser/SKILL.md",
            "definitions/skills/chrome-browser/SKILL.md",
            "definitions/skills/claude-api/SKILL.md",
            "definitions/skills/code-review/SKILL.md",
            "definitions/skills/computer-use/SKILL.md",
            "definitions/skills/dataviz/SKILL.md",
            "definitions/skills/debug/SKILL.md",
            "definitions/skills/deep-research/SKILL.md",
            "definitions/skills/docs/SKILL.md",
            "definitions/skills/doctor/SKILL.md",
            "definitions/skills/docx/SKILL.md",
            "definitions/skills/fewer-permission-prompts/SKILL.md",
            "definitions/skills/google-workspace/SKILL.md",
            "definitions/skills/import-memory/SKILL.md",
            "definitions/skills/init/SKILL.md",
            "definitions/skills/keybindings-help/SKILL.md",
            "definitions/skills/loop/SKILL.md",
            "definitions/skills/morning/SKILL.md",
            "definitions/skills/pdf/SKILL.md",
            "definitions/skills/plugin-authoring/SKILL.md",
            "definitions/skills/pptx/SKILL.md",
            "definitions/skills/run-skill-generator/SKILL.md",
            "definitions/skills/run/SKILL.md",
            "definitions/skills/security-review/SKILL.md",
            "definitions/skills/simplify/SKILL.md",
            "definitions/skills/skill-creator/SKILL.md",
            "definitions/skills/tx-agents/SKILL.md",
            "definitions/skills/tx-agents/templates/SKILL.md",
            "definitions/skills/tx-agents/templates/agent.md",
            "definitions/skills/tx-agents/templates/mcp.json",
            "definitions/skills/tx-artifacts/SKILL.md",
            "definitions/skills/tx-code-tours/SKILL.md",
            "definitions/skills/tx-code-tours/scripts/apply_tour.py",
            "definitions/skills/tx-diff-review/SKILL.md",
            "definitions/skills/tx-mockup/SKILL.md",
            "definitions/skills/tx-sessions/SKILL.md",
            "definitions/skills/update-config/SKILL.md",
            "definitions/skills/verify/SKILL.md",
            "definitions/skills/workflow-authoring/SKILL.md",
            "definitions/skills/xlsx/SKILL.md",
        ]
    );
    assert!(resources.iter().all(|r| r.plugin == roles::PLUGIN_ID));
    assert_eq!(names.len(), roles::SHIPPED.len());
    assert!(core.plugin_resources("").is_empty(), "no plugin was loaded");
    assert_eq!(core.resources("").len(), roles::SHIPPED.len());
}

#[test]
fn every_file_of_the_roles_and_definitions_trees_is_shipped() {
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
    let definitions = manifest.join("../config/tx");
    files(&definitions, &definitions, "definitions", &mut expected);
    expected.sort();
    let mut shipped: Vec<String> = roles::SHIPPED
        .iter()
        .map(|(n, _)| (*n).to_owned())
        .collect();
    shipped.sort();
    assert_eq!(shipped, expected);
}
