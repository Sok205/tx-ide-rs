//! `tx definitions tools`: the names an agent definition's `tools` and `disallowedTools` can use.
//! Read from the engines, never hard-coded where an engine can tell:
//! - Claude's built-in tools (and bundled skills) from the `system/init` event of a headless
//!   launch against a dead API address (no model call, no login), cached per Claude version
//!   under `<cache>/tools/`;
//! - the Claude names Codex maps to a switch, from engine-codex;
//! - each MCP server's tools from `codex app-server`'s `mcpServerStatus/list`, which starts the
//!   server and asks it (the list comes from the server, so it is the same on every engine).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tx_core::Directories;
use tx_core::definitions::{Definitions, McpServer};

const CLAUDE_BIN: &str = "claude";
const CODEX_BIN: &str = "codex";
const CACHE_DIR: &str = "tools";
/// Nothing listens here: Claude emits `system/init` before its first request, which then fails.
const DEAD_API_URL: &str = "http://127.0.0.1:9";
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_PREFIX: &str = "mcp__";

/// What one Claude version offers, as its `system/init` names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaudeBuiltIns {
    pub version: String,
    pub tools: Vec<String>,
    pub bundled_skills: Vec<String>,
}

/// Claude's version, from `claude --version` (`2.1.318 (Claude Code)`).
fn claude_version() -> Result<String, String> {
    let output = Command::new(CLAUDE_BIN)
        .arg("--version")
        .output()
        .map_err(|error| format!("{CLAUDE_BIN} --version: {error}"))?;
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| format!("{CLAUDE_BIN} --version printed nothing"))
}

/// The built-ins of the installed Claude: from the cache when this version was probed before.
pub fn claude_built_ins(directories: &Directories) -> Result<ClaudeBuiltIns, String> {
    let version = claude_version()?;
    let cache = directories
        .cache
        .join(CACHE_DIR)
        .join(format!("claude-{version}.json"));
    if let Ok(text) = std::fs::read_to_string(&cache) {
        return serde_json::from_str(&text)
            .map_err(|error| format!("{}: {error}", cache.display()));
    }
    let built_ins = probe_claude(&version)?;
    std::fs::create_dir_all(cache.parent().unwrap_or(Path::new(".")))
        .and_then(|()| {
            std::fs::write(
                &cache,
                serde_json::to_string(&built_ins).unwrap_or_default(),
            )
        })
        .map_err(|error| format!("{}: {error}", cache.display()))?;
    Ok(built_ins)
}

/// One headless launch in a scratch directory with a scratch config dir and a dummy token, every
/// tool on, read up to its `system/init` line, then killed. `Task` is reported as `Agent`, the
/// name the interactive session and `--tools` use; the interactive-only tools are added.
fn probe_claude(version: &str) -> Result<ClaudeBuiltIns, String> {
    let scratch = tempfile::Builder::new()
        .prefix("tx-tools-probe")
        .tempdir()
        .map_err(|error| format!("probe directory: {error}"))?;
    let mut command = Command::new(CLAUDE_BIN);
    command
        .args([
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--tools",
            "default",
            "--no-session-persistence",
            "probe",
        ])
        .current_dir(scratch.path())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", scratch.path())
        .env("CLAUDE_CONFIG_DIR", scratch.path().join("config"))
        .env("ANTHROPIC_BASE_URL", DEAD_API_URL)
        .env("ANTHROPIC_AUTH_TOKEN", "tx-tools-probe")
        .env("DISABLE_AUTOUPDATER", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let init = first_line_matching(command, |line| {
        line["type"] == "system" && line["subtype"] == "init"
    })?;
    let names = |key: &str| -> Vec<String> {
        init[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(|name| if name == "Task" { "Agent" } else { name }.to_owned())
            .collect()
    };
    let mut tools = names("tools");
    for tool in tx_engine_claude::INTERACTIVE_TOOLS {
        if !tools.iter().any(|listed| listed == tool) {
            tools.push(tool.to_owned());
        }
    }
    tools.sort();
    let mut bundled_skills = names("skills");
    bundled_skills.sort();
    Ok(ClaudeBuiltIns {
        version: version.to_owned(),
        tools,
        bundled_skills,
    })
}

/// Spawns `command` with piped stdout and returns its first JSON line `wanted` accepts, then kills
/// it; gives up after [`PROBE_TIMEOUT`].
fn first_line_matching(
    mut command: Command,
    wanted: impl Fn(&Value) -> bool + Send + 'static,
) -> Result<Value, String> {
    let program = format!("{:?}", command.get_program());
    let mut child = command
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && wanted(&value)
            {
                let _ = sender.send(value);
                return;
            }
        }
    });
    let result = receiver
        .recv_timeout(PROBE_TIMEOUT)
        .map_err(|_| format!("{program}: no answer within {}s", PROBE_TIMEOUT.as_secs()));
    end(&mut child);
    result
}

fn end(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// One MCP server's tools as `mcp__<server>__<tool>` with their descriptions, or why there are
/// none (not logged in, a start failure).
pub type ServerTools = Result<Vec<(String, String)>, String>;

/// Asks `codex app-server` for each server's tools: one app-server with every server defined
/// through `-c`, one `mcpServerStatus/list` per server name.
pub fn mcp_tools(
    servers: &BTreeMap<String, McpServer>,
) -> Result<BTreeMap<String, ServerTools>, String> {
    let mut results = BTreeMap::new();
    let mut command = Command::new(CODEX_BIN);
    command.arg("app-server");
    for (name, server) in servers {
        let unset: Vec<&str> = server
            .variables()
            .into_iter()
            .filter(|variable| std::env::var_os(variable).is_none())
            .collect();
        if !unset.is_empty() {
            results.insert(
                name.clone(),
                Err(format!("needs ${} to be set", unset.join(", $"))),
            );
            continue;
        }
        match tx_engine_codex::server_overrides(name, server) {
            Ok(overrides) => {
                for override_ in overrides {
                    command.args(["-c", &override_]);
                }
            }
            Err(error) => {
                results.insert(name.clone(), Err(error.to_string()));
            }
        }
    }
    let queried: Vec<&String> = servers
        .keys()
        .filter(|name| !results.contains_key(*name))
        .collect();
    if queried.is_empty() {
        return Ok(results);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{CODEX_BIN} app-server: {error}"))?;
    let outcome = query_app_server(&mut child, &queried);
    end(&mut child);
    results.extend(outcome?);
    Ok(results)
}

fn query_app_server(
    child: &mut Child,
    servers: &[&String],
) -> Result<BTreeMap<String, ServerTools>, String> {
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (sender, receiver) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && value.get("id").is_some()
                && sender.send(value).is_err()
            {
                return;
            }
        }
    });
    let mut send = |message: Value| {
        writeln!(stdin, "{message}").map_err(|error| format!("codex app-server: {error}"))
    };
    let answer = |id: u64| -> Result<Value, String> {
        loop {
            let message = receiver
                .recv_timeout(PROBE_TIMEOUT)
                .map_err(|_| "codex app-server: no answer".to_owned())?;
            if message["id"] == id {
                return Ok(message);
            }
        }
    };
    send(
        serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
        "params": {"clientInfo": {"name": "tx", "version": env!("CARGO_PKG_VERSION")}}}),
    )?;
    answer(0)?;
    send(serde_json::json!({"jsonrpc": "2.0", "method": "initialized"}))?;
    let mut results = BTreeMap::new();
    for (index, name) in servers.iter().enumerate() {
        let id = index as u64 + 1;
        send(
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "mcpServerStatus/list",
            "params": {"serverName": name, "detail": "toolsAndAuthOnly"}}),
        )?;
        let message = answer(id)?;
        results.insert((*name).clone(), server_tools(name, &message));
    }
    Ok(results)
}

fn server_tools(name: &str, message: &Value) -> ServerTools {
    if let Some(error) = message.get("error") {
        return Err(error["message"].as_str().unwrap_or("error").to_owned());
    }
    let entry = message["result"]["data"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["name"] == name)
        .ok_or_else(|| "not started".to_owned())?;
    if let Some(error) = entry["toolsError"].as_str() {
        // Codex's message carries the whole transport type; its last clause is the reason.
        if error.contains("Auth required") {
            return Err("auth required: log codex in to this server (`codex mcp login`)".into());
        }
        return Err(error.rsplit(" error: ").next().unwrap_or(error).to_owned());
    }
    let mut tools: Vec<(String, String)> = entry["tools"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(tool, definition)| {
            (
                format!("{MCP_PREFIX}{name}__{tool}"),
                definition["description"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    tools.sort();
    Ok(tools)
}

/// The rows of `tx definitions tools`: Claude's built-ins, the baseline, the Codex switches, then
/// each MCP server's tools.
pub fn list(directories: &Directories, out: &mut dyn Write) -> Result<(), String> {
    let claude = claude_built_ins(directories)?;
    writeln!(out, "claude (Claude Code {}):", claude.version).ok();
    for tool in &claude.tools {
        let note = if tx_engine_claude::INTERACTIVE_TOOLS.contains(&tool.as_str()) {
            "built-in, interactive only"
        } else {
            "built-in"
        };
        writeln!(out, "{tool}\t{note}").ok();
    }
    writeln!(
        out,
        "# claude adds to every agent's tools: {}",
        tx_engine_claude::BASELINE_TOOLS.join(", ")
    )
    .ok();
    writeln!(
        out,
        "\ncodex (the Claude names it switches off when left out):"
    )
    .ok();
    for (names, switch) in tx_engine_codex::BUILT_IN_SWITCHES {
        for name in names {
            writeln!(out, "{name}\t{}", switch.join(" ")).ok();
        }
    }
    let servers: BTreeMap<String, McpServer> = Definitions::new(directories.definition_roots())
        .mcp_servers()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|(name, (server, _))| (name, server))
        .collect();
    for (name, tools) in mcp_tools(&servers)? {
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
