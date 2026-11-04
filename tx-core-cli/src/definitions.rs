//! `tx definitions` and `tx check`: list and validate the agent, skill and MCP definitions. Both
//! read only the definition roots, never the core's state.

use std::io::Write;

use tx_core::definitions::{DefinitionError, Definitions, McpServer, read_agent, read_skill};
use tx_core::{Directories, EngineKind, SkillProvider};

use crate::args::DefinitionKind;

/// The one line saying what Claude adds to every agent's tools, shown wherever agents are.
fn baseline_line() -> String {
    format!(
        "# claude adds to every agent's tools: {}",
        tx_engine_claude::BASELINE_TOOLS.join(", ")
    )
}

/// `name<TAB>description` per definition (for a server: its type and URL or command). One kind
/// prints bare rows; every kind prints each under a `<kind>:` heading, empty kinds included. An
/// agent or skill that does not read is listed as invalid; a broken `mcp.json` fails the listing.
pub fn list(
    directories: &Directories,
    kind: Option<DefinitionKind>,
    out: &mut dyn Write,
) -> Result<(), DefinitionError> {
    let definitions = Definitions::new(directories.definition_roots());
    let kinds = match kind {
        Some(kind) => vec![kind],
        None => DefinitionKind::ALL.to_vec(),
    };
    if kind == Some(DefinitionKind::Tools) {
        return crate::tools::list(directories, out)
            .map_err(|message| DefinitionError::new(&directories.cache, None, message));
    }
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
            writeln!(out, "{}", baseline_line()).ok();
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

/// Prints `path:line: message` per problem and returns whether there were none; on success, one
/// line saying what was checked. With an engine, it also prints what that engine carries on
/// every launch that no flag removes, then per agent what it gets that its file does not say.
/// The Claude baseline is always shown.
pub fn check(
    directories: &Directories,
    engine: Option<EngineKind>,
    names: &[String],
    out: &mut dyn Write,
) -> Result<bool, DefinitionError> {
    let report = Definitions::new(directories.definition_roots()).check(names)?;
    let mut engine_problems = 0;
    for problem in &report.problems {
        writeln!(out, "{problem}").ok();
    }
    writeln!(out, "{}", baseline_line()).ok();
    if let Some(engine) = engine {
        let (forced, notes): (&[&str], fn(&_) -> Vec<String>) = match engine {
            EngineKind::Claude => (
                &tx_engine_claude::FORCED,
                tx_engine_claude::definition_notes,
            ),
            EngineKind::Codex => (&tx_engine_codex::FORCED, tx_engine_codex::definition_notes),
        };
        for item in forced {
            writeln!(out, "{engine}: always present: {item}").ok();
        }
        // Claude drops a `tools` name it does not know without a word: refuse it here.
        if engine == EngineKind::Claude {
            let built_ins = crate::tools::claude_built_ins(directories)
                .map_err(|message| DefinitionError::new(&directories.cache, None, message))?;
            for agent in &report.agent_definitions {
                let unknown: Vec<&str> = agent
                    .tools
                    .iter()
                    .chain(
                        agent
                            .disallowed_tools
                            .iter()
                            .filter(|tool| !tool.starts_with("mcp__")),
                    )
                    .map(String::as_str)
                    .filter(|tool| {
                        *tool != "Task" && !built_ins.tools.iter().any(|known| known == tool)
                    })
                    .collect();
                if !unknown.is_empty() {
                    engine_problems += 1;
                    writeln!(
                        out,
                        "{}:{}: not a Claude Code {} tool, which it would drop silently: {}",
                        agent.path.display(),
                        agent.line_of("tools").unwrap_or(1),
                        built_ins.version,
                        unknown.join(", ")
                    )
                    .ok();
                }
            }
        }
        // Reference skills: Claude must have each one; Codex has none of them.
        let skills = Definitions::new(directories.definition_roots()).skill_directories()?;
        let bundled = match engine {
            EngineKind::Claude => Some(
                crate::tools::claude_built_ins(directories)
                    .map_err(|message| DefinitionError::new(&directories.cache, None, message))?,
            ),
            EngineKind::Codex => None,
        };
        for agent in &report.agent_definitions {
            for name in &agent.skills {
                let Some(provider) = skills
                    .get(name)
                    .and_then(|directory| tx_core::definitions::read_skill(directory).ok())
                    .and_then(|skill| skill.provided_by)
                else {
                    continue;
                };
                let problem = match (engine, provider, &bundled) {
                    (EngineKind::Codex, provider, _) => Some(format!(
                        "skill {name:?} is provided by {}: not on codex",
                        provider.as_str()
                    )),
                    (EngineKind::Claude, SkillProvider::ClaudeBundled, Some(built_ins))
                        if !built_ins.bundled_skills.contains(name) =>
                    {
                        Some(format!(
                            "Claude Code {} has no bundled skill {name:?}",
                            built_ins.version
                        ))
                    }
                    (EngineKind::Claude, SkillProvider::ClaudeSynced, _)
                        if !tx_engine_claude::synced_skill_exists(name) =>
                    {
                        Some(format!("no claude.ai-synced skill {name:?} on this machine"))
                    }
                    _ => None,
                };
                if let Some(problem) = problem {
                    engine_problems += 1;
                    writeln!(
                        out,
                        "{}:{}: {problem}",
                        agent.path.display(),
                        agent.line_of("skills").unwrap_or(1)
                    )
                    .ok();
                }
            }
        }
        for agent in &report.agent_definitions {
            for note in notes(agent) {
                writeln!(out, "{}: {engine}: {note}", agent.path.display()).ok();
            }
        }
    }
    let clean = report.problems.is_empty() && engine_problems == 0;
    if clean {
        writeln!(
            out,
            "ok: {} agents, {} skills, {} MCP servers",
            report.agents, report.skills, report.servers
        )
        .ok();
    }
    Ok(clean)
}
