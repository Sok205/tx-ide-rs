//! What the core ships, listed by `TxCore::resources` (plugin id [`PLUGIN_ID`]) ahead of every
//! plugin's; a plugin that registers a resource of the same name is refused as a duplicate.
//! - The roles: `agents/…` resources from `tx-core/agents/`, which `tx ui install` writes under
//!   `<data>/roles/`, so a role is not tied to the plugin that first needed it.
//! - The definitions: `definitions/…` resources from the repo's `config/tx/` (`skills/`,
//!   `mcp.json`, later `agents/`), which `tx ui install` writes under `<data>/definitions/`.

/// The `plugin` field of every shipped role resource.
pub const PLUGIN_ID: &str = "tx-core";

/// Every file of `tx-core/agents/`, named `agents/<path>`, then every file of `config/tx/`, named
/// `definitions/<path>`.
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
        "definitions/skills/tx-artifacts/SKILL.md",
        include_bytes!("../../config/tx/skills/tx-artifacts/SKILL.md"),
    ),
    (
        "definitions/skills/tx-code-tours/SKILL.md",
        include_bytes!("../../config/tx/skills/tx-code-tours/SKILL.md"),
    ),
    (
        "definitions/skills/tx-code-tours/scripts/apply_tour.py",
        include_bytes!("../../config/tx/skills/tx-code-tours/scripts/apply_tour.py"),
    ),
    (
        "definitions/skills/tx-diff-review/SKILL.md",
        include_bytes!("../../config/tx/skills/tx-diff-review/SKILL.md"),
    ),
    (
        "definitions/skills/tx-mockup/SKILL.md",
        include_bytes!("../../config/tx/skills/tx-mockup/SKILL.md"),
    ),
    (
        "definitions/skills/tx-sessions/SKILL.md",
        include_bytes!("../../config/tx/skills/tx-sessions/SKILL.md"),
    ),
    (
        "definitions/mcp.json",
        include_bytes!("../../config/tx/mcp.json"),
    ),
];
