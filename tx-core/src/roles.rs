//! The roles and skills the core ships: `agents/…` resources (plugin id [`PLUGIN_ID`]) listed by
//! `TxCore::resources` ahead of every plugin's, which `tx ui install` writes under
//! `<data>/roles/`. One tree, `tx-core/agents/`, so a role is not tied to the plugin that first
//! needed it; a plugin that registers a resource of the same name is refused as a duplicate.

/// The `plugin` field of every shipped role resource.
pub const PLUGIN_ID: &str = "tx-core";

/// Every file of `tx-core/agents/`, named `agents/<path>`.
pub const SHIPPED: &[(&str, &[u8])] = &[
    ("agents/COMMON.md", include_bytes!("../agents/COMMON.md")),
    (
        "agents/COMMON.local.md",
        include_bytes!("../agents/COMMON.local.md"),
    ),
    (
        "agents/DEVELOPER.md",
        include_bytes!("../agents/DEVELOPER.md"),
    ),
    (
        "agents/ORCHESTRATOR.md",
        include_bytes!("../agents/ORCHESTRATOR.md"),
    ),
    ("agents/QA.md", include_bytes!("../agents/QA.md")),
    (
        "agents/TX-ASSISTANT.md",
        include_bytes!("../agents/TX-ASSISTANT.md"),
    ),
    (
        "agents/skills/tx-artifacts/SKILL.md",
        include_bytes!("../agents/skills/tx-artifacts/SKILL.md"),
    ),
    (
        "agents/skills/tx-code-tours/SKILL.md",
        include_bytes!("../agents/skills/tx-code-tours/SKILL.md"),
    ),
    (
        "agents/skills/tx-code-tours/scripts/apply_tour.py",
        include_bytes!("../agents/skills/tx-code-tours/scripts/apply_tour.py"),
    ),
    (
        "agents/skills/tx-diff-review/SKILL.md",
        include_bytes!("../agents/skills/tx-diff-review/SKILL.md"),
    ),
    (
        "agents/skills/tx-mockup/SKILL.md",
        include_bytes!("../agents/skills/tx-mockup/SKILL.md"),
    ),
    (
        "agents/skills/tx-sessions/SKILL.md",
        include_bytes!("../agents/skills/tx-sessions/SKILL.md"),
    ),
];
