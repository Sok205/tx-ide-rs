//! `tx definitions tools`: the names an agent definition's `tools` and `disallowedTools` can use,
//! as the active engines report them: each engine's own tool names, then the MCP servers'
//! tools as the first engine that can list them lists them.

use std::collections::BTreeMap;
use std::io::Write;

use tx_core::Directories;
use tx_core::definitions::{DefinitionError, Definitions, McpServer};
use tx_core::engine::{EngineTools, ToolsRequest};

use crate::definitions::Engines;

/// Per active engine in kind order its heading, rows and footer, a blank line between engines
/// (an engine that reports nothing prints nothing); then a `mcp <server>:` section per server,
/// or why there are none. A listing that failed as a whole fails the command, after the
/// engines' sections.
pub fn list(
    directories: &Directories,
    engines: &Engines,
    out: &mut dyn Write,
) -> Result<(), DefinitionError> {
    let failed = |message: String| DefinitionError::new(&directories.cache, None, message);
    let servers: BTreeMap<String, McpServer> = Definitions::new(directories.definition_roots())
        .mcp_servers()
        .map_err(|error| failed(error.to_string()))?
        .into_iter()
        .map(|(name, (server, _))| (name, server))
        .collect();
    let request = ToolsRequest {
        cache: &directories.cache,
        mcp_servers: &servers,
    };
    let mut printed = false;
    let mut listing = None;
    for engine in engines.all().values() {
        let report = engine
            .tools(&request)
            .map_err(|error| failed(error.to_string()))?;
        if report == EngineTools::default() {
            continue;
        }
        if printed {
            writeln!(out).ok();
        }
        printed = true;
        if !report.heading.is_empty() {
            writeln!(out, "{}", report.heading).ok();
        }
        for (name, note) in &report.rows {
            writeln!(out, "{name}\t{note}").ok();
        }
        for line in &report.footer {
            writeln!(out, "{line}").ok();
        }
        if listing.is_none() {
            listing = report.mcp;
        }
    }
    let Some(listing) = listing else {
        writeln!(
            out,
            "\n# no MCP tools listed: no active engine can list them"
        )
        .ok();
        return Ok(());
    };
    for (name, tools) in listing.map_err(failed)? {
        writeln!(out, "\nmcp {name}:").ok();
        match tools {
            Ok(tools) => {
                for (tool, description) in tools {
                    let description = description.split_whitespace().collect::<Vec<_>>().join(" ");
                    writeln!(out, "{tool}\t{description}").ok();
                }
            }
            Err(reason) => {
                writeln!(out, "# no tools listed: {reason}").ok();
            }
        }
    }
    Ok(())
}
