//! What the installed Claude Code offers, read from Claude itself and cached per version: its
//! built-in tools and its bundled skills. One headless launch in a scratch directory, with a
//! scratch config dir and a dummy token, against a local listener standing in for the API: the
//! `system/init` event names the tools and the user-invocable skills, and the first request's
//! skill listing names the skills only the model sees. Skills the Claude account gates in are not
//! loaded with a dummy token, so they cannot be named here. The listener answers with an error, so there is
//! no model call and no login.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
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
const CACHE_FORMAT: u32 = 4;
const TIMEOUT: Duration = Duration::from_secs(30);
/// The line that opens the request's skill listing; each skill follows as `- <name>: …`.
const LISTING_HEADER: &str = "The following skills are available for use with the Skill tool:";

/// What one Claude version offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuiltIns {
    pub version: String,
    /// The built-in tools, `Task` named `Agent`, the interactive-only ones included.
    pub tools: Vec<String>,
    /// Every bundled skill the version loads, user-invocable (`system/init` `skills`) or only
    /// model-visible (the request's listing): what tx turns off unless an agent lists it. Names a
    /// plugin scopes (`plugin:skill`) are left out.
    pub bundled_skills: Vec<String>,
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
    // Some bundled skills load only in a git checkout (`init`, `security-review`), as agents run.
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(scratch.path())
        .status()
        .map_err(|error| format!("git init: {error}"))?;
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|error| format!("probe listener: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("probe listener: {error}"))?;
    let (body_sender, body_receiver) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let Some((path, body)) = read_request(&mut stream) else {
                continue;
            };
            let _ = stream.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
            if path.starts_with("/v1/messages") && body.contains("\"system\"") {
                let _ = body_sender.send(body);
                return;
            }
        }
    });
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
            "--settings",
            r#"{"disableBundledSkills":false}"#,
            "probe",
        ])
        .current_dir(scratch.path())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", scratch.path())
        .env("CLAUDE_CONFIG_DIR", scratch.path().join("config"))
        .env("ANTHROPIC_BASE_URL", format!("http://{address}"))
        .env("ANTHROPIC_AUTH_TOKEN", "tx-probe")
        .env("DISABLE_AUTOUPDATER", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{CLAUDE_BIN}: {error}"))?;
    let outcome = read_probe(&mut child, &body_receiver, version);
    let _ = child.kill();
    let _ = child.wait();
    outcome
}

fn read_probe(
    child: &mut Child,
    bodies: &mpsc::Receiver<String>,
    version: &str,
) -> Result<BuiltIns, String> {
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
    let body = bodies
        .recv_timeout(TIMEOUT)
        .map_err(|_| "claude probe: no first request".to_owned())?;
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
    // Not `slash_commands`: they include local commands (`clear`, `compact`) that are not skills.
    let mut bundled_skills: Vec<String> = names("skills")
        .into_iter()
        .chain(listed_skills(&body))
        .filter(|name| !name.contains(':'))
        .collect();
    bundled_skills.sort();
    bundled_skills.dedup();
    Ok(BuiltIns {
        version: version.to_owned(),
        tools,
        bundled_skills,
    })
}

/// The skill names of the request's listing: `- <name>: …` lines after [`LISTING_HEADER`].
fn listed_skills(body: &str) -> Vec<String> {
    let Ok(request) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let mut texts = Vec::new();
    collect_texts(&request, &mut texts);
    texts
        .iter()
        .filter_map(|text| text.split_once(LISTING_HEADER).map(|(_, rest)| rest))
        .flat_map(|rest| {
            // A description may run over several lines; the listing ends at the first blank one.
            rest.trim_start()
                .lines()
                .take_while(|line| !line.trim().is_empty())
                .filter_map(|line| line.strip_prefix("- "))
                .filter_map(|line| line.split_once(": ").map(|(name, _)| name.to_owned()))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn collect_texts<'a>(value: &'a Value, texts: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => texts.push(text),
        Value::Array(items) => items.iter().for_each(|item| collect_texts(item, texts)),
        Value::Object(fields) => fields.values().for_each(|item| collect_texts(item, texts)),
        _ => {}
    }
}

/// One HTTP/1.1 request: its path and body (by `content-length`).
fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, String)> {
    stream.set_read_timeout(Some(TIMEOUT)).ok()?;
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let path = request_line.split_whitespace().nth(1)?.to_owned();
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().ok()?;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some((path, String::from_utf8_lossy(&body).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_listing_names_its_skills() {
        let body = serde_json::json!({"system": [{"type": "text", "text": format!(
            "intro\n\n{LISTING_HEADER}\n\n- loop: Re-runs.\nmore of it: here\n- tx:pdf: PDFs.\n- keybindings-help: Keys.\n\n- after: not a skill"
        )}]})
        .to_string();
        assert_eq!(listed_skills(&body), ["loop", "tx:pdf", "keybindings-help"]);
    }
}
