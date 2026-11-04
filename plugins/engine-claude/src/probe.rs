//! What the installed Claude Code offers, read from Claude itself and cached per version: its
//! built-in tools. One headless launch in a scratch directory, with a scratch config dir and a
//! dummy token, against an API address nothing listens on: the `system/init` event, which comes
//! before the first request, names the tools. There is no model call and no login.

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::CLAUDE_BIN;

/// Under the cache directory.
pub const CACHE_DIR: &str = "tools";
/// Bumped when what a cache file holds changes, so older files are probed again.
const CACHE_FORMAT: u32 = 5;
const TIMEOUT: Duration = Duration::from_secs(30);
/// Nothing listens here: the request after `system/init` fails, and the probe is killed first.
const DEAD_API_URL: &str = "http://127.0.0.1:9";

/// What one Claude version offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltIns {
    pub version: String,
    /// The built-in tools, `Task` named `Agent`, the interactive-only ones included.
    pub tools: Vec<String>,
}

/// The installed Claude's version, from `claude --version` (`2.1.318 (Claude Code)`).
pub fn version() -> Result<String, String> {
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

/// The installed version's built-ins: from `<cache>/tools/claude-<version>-v<format>.json`, else
/// probed and written there.
pub fn built_ins(cache: &Path) -> Result<BuiltIns, String> {
    let version = version()?;
    let file = cache
        .join(CACHE_DIR)
        .join(format!("claude-{version}-v{CACHE_FORMAT}.json"));
    if let Ok(text) = std::fs::read_to_string(&file) {
        return serde_json::from_str(&text).map_err(|error| format!("{}: {error}", file.display()));
    }
    let probed = probe(&version)?;
    let written = serde_json::to_string(&probed).map_err(|error| error.to_string())?;
    std::fs::create_dir_all(cache.join(CACHE_DIR))
        .and_then(|()| std::fs::write(&file, written))
        .map_err(|error| format!("{}: {error}", file.display()))?;
    Ok(probed)
}

fn probe(version: &str) -> Result<BuiltIns, String> {
    let scratch = tempfile::Builder::new()
        .prefix("tx-claude-probe")
        .tempdir()
        .map_err(|error| format!("probe directory: {error}"))?;
    let mut child = Command::new(CLAUDE_BIN)
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
        .env("ANTHROPIC_AUTH_TOKEN", "tx-probe")
        .env("DISABLE_AUTOUPDATER", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{CLAUDE_BIN}: {error}"))?;
    let outcome = read_probe(&mut child, version);
    let _ = child.kill();
    let _ = child.wait();
    outcome
}

fn read_probe(child: &mut Child, version: &str) -> Result<BuiltIns, String> {
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let (init_sender, init_receiver) = mpsc::channel::<Value>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line)
                && value["type"] == "system"
                && value["subtype"] == "init"
            {
                let _ = init_sender.send(value);
                return;
            }
        }
    });
    let init = init_receiver
        .recv_timeout(TIMEOUT)
        .map_err(|_| "claude probe: no system/init".to_owned())?;
    let names = |key: &str| -> Vec<String> {
        init[key]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    let mut tools: Vec<String> = names("tools")
        .into_iter()
        .map(|tool| {
            if tool == "Task" {
                "Agent".to_owned()
            } else {
                tool
            }
        })
        .collect();
    for tool in crate::INTERACTIVE_TOOLS {
        if !tools.iter().any(|listed| listed == tool) {
            tools.push(tool.to_owned());
        }
    }
    tools.sort();
    Ok(BuiltIns {
        version: version.to_owned(),
        tools,
    })
}
