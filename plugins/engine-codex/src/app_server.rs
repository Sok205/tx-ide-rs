//! The MCP servers' tools, from `codex app-server`'s `mcpServerStatus/list`, which starts each
//! server and asks it (the list comes from the server, so it is the same on every engine): what
//! `tx definitions tools` shows under `mcp <server>:`.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;
use tx_core::definitions::McpServer;
use tx_core::engine::ServerTools;

use crate::{CODEX_BIN, server_overrides};

const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const MCP_PREFIX: &str = "mcp__";

fn end(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Asks `codex app-server` for each server's tools (`mcp__<server>__<tool>` with their
/// descriptions, or why there are none: not logged in, a start failure): one app-server with every server defined
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
        match server_overrides(name, server) {
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
