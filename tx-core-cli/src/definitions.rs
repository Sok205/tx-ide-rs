//! `tx definitions` and `tx check`: list and validate the agent, skill and MCP definitions, and
//! print what the active engines add to them. Both read only the definition roots and
//! `config.yaml`, never the core's state; an engine that runs as its own program is asked over
//! the protocol.

use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;

use tx_core::definitions::{
    AgentDefinition, DefinitionError, Definitions, McpServer, read_agent, read_skill,
};
use tx_core::engine::{AgentSummary, DefinitionsRequest, Finding};
use tx_core::plugin::Registry;
use tx_core::plugin::external::{ProcessEngine, engine_proxies};
use tx_core::profile::Profiles;
use tx_core::{Directories, EngineKind, LlmEngine, SkillProvider};

use crate::args::{DefinitionKind, Global};
use crate::setup::{Failure, Installation, Linked, exec_context};

/// The engines a core of this installation would have, without opening one: a proxy for the
/// engine of every active installed plugin, and, until the linked plugins are gone (PR 19), the
/// engine of each linked plugin that still runs compiled in, registered into a registry of its
/// own. That second half repeats on purpose what `active_engines` in `check.rs` does for the
/// kinds; both go with the linked plugins.
pub struct Engines {
    installed: Vec<ProcessEngine>,
    linked: Vec<Registry>,
}

impl Engines {
    pub fn read(
        global: &Global,
        installation: &Installation,
        linked: &Linked,
    ) -> Result<Self, Failure> {
        let context = Arc::new(exec_context(global, &installation.config));
        let installed = engine_proxies(
            &installation.index,
            &installation.activation,
            &context,
            &installation.config,
            &[],
        )
        .into_iter()
        .map(|(_, engine)| engine)
        .collect();
        let mut registries = Vec::new();
        for plugin in &linked.plugins {
            let entry = installation.entries.get(plugin.id());
            if installation.index.contains(plugin.id()) || entry.is_some_and(|entry| !entry.enabled)
            {
                continue;
            }
            let settings = entry.map_or(serde_json::Value::Null, |entry| entry.settings.clone());
            let mut registry = Registry::new(settings, installation.config.clone());
            plugin.register(&mut registry).map_err(Failure::Plugin)?;
            if !registry.engines().is_empty() {
                registries.push(registry);
            }
        }
        Ok(Self {
            installed,
            linked: registries,
        })
    }

    /// Every active engine, in kind order.
    pub fn all(&self) -> BTreeMap<EngineKind, &dyn LlmEngine> {
        let mut engines = BTreeMap::new();
        let installed = self.installed.iter().map(|engine| engine as &dyn LlmEngine);
        let linked = self
            .linked
            .iter()
            .flat_map(|registry| registry.engines().iter().map(Box::as_ref));
        for engine in installed.chain(linked) {
            engines.entry(engine.kind()).or_insert(engine);
        }
        engines
    }

    /// What every active engine adds to every agent, in kind order.
    pub fn notes(&self) -> Vec<String> {
        self.all()
            .values()
            .flat_map(|engine| engine.notes())
            .collect()
    }
}

/// Every skill of the definition roots that an engine provides, by name, from its reference
/// `SKILL.md`.
pub fn provided_skills(
    directories: &Directories,
) -> Result<BTreeMap<String, SkillProvider>, DefinitionError> {
    Ok(Definitions::new(directories.definition_roots())
        .skill_directories()?
        .into_iter()
        .filter_map(|(name, directory)| Some((name, read_skill(&directory).ok()?.provided_by?)))
        .collect())
}

/// `<path>:<line>`, or `<path>` for a finding without a line.
fn place(finding: &Finding) -> String {
    match finding.line {
        Some(line) => format!("{}:{line}", finding.path.display()),
        None => finding.path.display().to_string(),
    }
}

/// `name<TAB>description` per definition (for a server: its type and URL or command). One kind
/// prints bare rows; every kind prints each under a `<kind>:` heading, empty kinds included. An
/// agent or skill that does not read is listed as invalid; a broken `mcp.json` fails the listing.
pub fn list(
    directories: &Directories,
    kind: Option<DefinitionKind>,
    notes: &[String],
    out: &mut dyn Write,
) -> Result<(), DefinitionError> {
    let definitions = Definitions::new(directories.definition_roots());
    let kinds = match kind {
        Some(kind) => vec![kind],
        None => DefinitionKind::ALL.to_vec(),
    };
    for (index, kind) in kinds.iter().enumerate() {
        let rows = rows(&definitions, *kind)?;
        if kinds.len() > 1 {
            if index > 0 {
                writeln!(out).ok();
            }
            writeln!(out, "{}:", kind.as_str()).ok();
        }
        for (name, description) in rows {
            writeln!(out, "{name}\t{description}").ok();
        }
        if *kind == DefinitionKind::Agents {
            for note in notes {
                writeln!(out, "# {note}").ok();
            }
        }
    }
    Ok(())
}

fn rows(
    definitions: &Definitions,
    kind: DefinitionKind,
) -> Result<Vec<(String, String)>, DefinitionError> {
    Ok(match kind {
        DefinitionKind::Agents => definitions
            .agent_files()?
            .into_iter()
            .map(|(name, path)| {
                let description = read_agent(&path).map(|agent| agent.description);
                (name, described(description))
            })
            .collect(),
        DefinitionKind::Skills => definitions
            .skill_directories()?
            .into_iter()
            .map(|(name, directory)| {
                let description = read_skill(&directory).map(|skill| skill.description);
                (name, described(description))
            })
            .collect(),
        DefinitionKind::Mcp => definitions
            .mcp_servers()?
            .into_iter()
            .map(|(name, (server, _))| {
                let description = match server {
                    McpServer::Stdio { command, args, .. } => {
                        format!("stdio {}", [vec![command], args].concat().join(" "))
                    }
                    McpServer::Http { url, .. } => format!("http {url}"),
                };
                (name, description)
            })
            .collect(),
        DefinitionKind::Tools => unreachable!("listed by crate::tools"),
    })
}

/// The description on one line; a file that does not read is still listed, pointing at `check`.
fn described(description: Result<String, DefinitionError>) -> String {
    match description {
        Ok(text) => text.split_whitespace().collect::<Vec<_>>().join(" "),
        Err(_) => INVALID.to_owned(),
    }
}

const INVALID: &str = "(invalid: see `tx check`)";

/// What [`check`] found: whether there were no problems, and what was checked, for the caller to
/// close with `ok: <summary>` (and more, when it checked more).
pub struct Checked {
    pub clean: bool,
    /// `<a> agents, <s> skills, <m> MCP servers`.
    pub summary: String,
}

/// Prints `path:line: message` per problem and returns whether there were none, with what was
/// checked; the caller prints the closing line. Then what the consulted engines say: every
/// active engine, or only `engine` (which the caller made sure is active). Their notes, as
/// `# <note>`, and their warnings; with an engine, what it carries on every launch that no flag
/// removes, its problems (each fails the check) and per agent what it gets that its file does
/// not say.
pub fn check(
    directories: &Directories,
    profiles: &Profiles,
    engines: &Engines,
    engine: Option<EngineKind>,
    names: &[String],
    out: &mut dyn Write,
) -> Result<Checked, DefinitionError> {
    let report = Definitions::new(directories.definition_roots()).check(names, profiles)?;
    for problem in &report.problems {
        writeln!(out, "{problem}").ok();
    }
    let consulted: Vec<(EngineKind, &dyn LlmEngine)> = engines
        .all()
        .into_iter()
        .filter(|(kind, _)| engine.is_none_or(|wanted| wanted == *kind))
        .collect();
    let agents: Vec<AgentSummary> = report
        .agent_definitions
        .iter()
        .map(AgentDefinition::summary)
        .collect();
    let provided = provided_skills(directories)?;
    let request = DefinitionsRequest {
        agents: &agents,
        provided_skills: &provided,
        cache: &directories.cache,
    };
    for (_, consulted) in &consulted {
        for note in consulted.notes() {
            writeln!(out, "# {note}").ok();
        }
    }
    for (_, consulted) in &consulted {
        for warning in consulted.warnings(&request) {
            writeln!(out, "{}: warning: {}", place(&warning), warning.message).ok();
        }
    }
    let mut engine_problems = 0;
    if let Some((kind, consulted)) = consulted.first()
        && engine.is_some()
    {
        let checked = consulted
            .check(&request)
            .map_err(|error| DefinitionError::new(&directories.cache, None, error.to_string()))?;
        for item in &checked.always_present {
            writeln!(out, "{kind}: always present: {item}").ok();
        }
        for problem in &checked.problems {
            writeln!(out, "{}: {}", place(problem), problem.message).ok();
        }
        engine_problems = checked.problems.len();
        for note in &checked.notes {
            writeln!(out, "{}: {kind}: {}", note.path.display(), note.message).ok();
        }
    }
    let clean = report.problems.is_empty() && engine_problems == 0;
    Ok(Checked {
        clean,
        summary: format!(
            "{} agents, {} skills, {} MCP servers",
            report.agents, report.skills, report.servers
        ),
    })
}
