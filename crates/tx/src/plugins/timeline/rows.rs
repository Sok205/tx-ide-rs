//! Transcript rows — the row taxonomy of the artifact-browser's `timeline.py`, ported.
//!
//! One row per transcript entry: `u` = the human typed, `p` = peer-agent mail, `s` =
//! harness/system injection, `a` = agent prose, `x` = tool call. Only `u` rows become "you
//! spoke" dots; the rest exist so segments and counts see all activity. `probe` is the milestone
//! slice of a tool call's RAW command, `None` on every other row.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::messages::fromisoformat_timestamp;

pub const EV_TEXT: usize = 130;
pub const TOOL_DETAIL: usize = 160;
pub const ROW_TEXT_CAP: usize = 4000;
pub const BLURB_CAP: usize = 220;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    User,
    Peer,
    System,
    Agent,
    Tool,
}

impl Kind {
    pub fn letter(self) -> char {
        match self {
            Kind::User => 'u',
            Kind::Peer => 'p',
            Kind::System => 's',
            Kind::Agent => 'a',
            Kind::Tool => 'x',
        }
    }

    /// Counted as a message (`u`, `p`, `a`).
    pub fn is_message(self) -> bool {
        matches!(self, Kind::User | Kind::Peer | Kind::Agent)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// Epoch seconds.
    pub at: f64,
    pub kind: Kind,
    pub text: String,
    pub probe: Option<String>,
}

/// Action diamonds: PRs and artifact manipulation only.
const MILESTONES: [(&str, &str); 2] = [
    (r"\bgh pr merge\b|\bgh pr ready\b", "PR merge/ready"),
    (r"\btx artifact (create|modify)\b(?!\s+--help)", "artifact"),
];

/// `regex` has no look-ahead: the `(?!\s+--help)` guard is checked by hand in [`milestone_at`].
static MILESTONE_RX: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    MILESTONES
        .iter()
        .map(|(pattern, label)| {
            let plain = pattern.replace(r"(?!\s+--help)", "");
            (Regex::new(&plain).expect("milestone pattern"), *label)
        })
        .collect()
});
static HELP_TAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s+--help").unwrap());
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

/// The first milestone match in `flat`: `(start, label)`.
fn milestone_at(flat: &str) -> Option<(usize, &'static str)> {
    for (regex, label) in MILESTONE_RX.iter() {
        for found in regex.find_iter(flat) {
            if *label == "artifact" && HELP_TAIL.is_match(&flat[found.end()..]) {
                continue;
            }
            return Some((found.start(), label));
        }
    }
    None
}

/// The label of the milestone a probe carries.
pub fn milestone_label(probe: &str) -> Option<&'static str> {
    milestone_at(probe).map(|(_, label)| label)
}

/// The milestone slice of a tool call's RAW command — from the match onward, `TOOL_DETAIL`
/// chars long — or `None` when the command is not a milestone. A command that MENTIONS a
/// milestone inside a `tx send-message` is quoting it, not doing it.
pub fn milestone_probe(detail: &str) -> Option<String> {
    let flat = WHITESPACE.replace_all(detail, " ");
    let flat = flat.trim();
    if flat.contains("send-message") {
        return None;
    }
    let (start, _label) = milestone_at(flat)?;
    Some(flat[start..].chars().take(TOOL_DETAIL).collect())
}

/// Python `t[:n] + " …[+k chars]"` over the stripped text.
pub fn cap(text: &str, limit: usize) -> String {
    let text = text.trim();
    let length = text.chars().count();
    if length > limit {
        let head: String = text.chars().take(limit).collect();
        format!("{head} …[+{} chars]", length - limit)
    } else {
        text.to_owned()
    }
}

macro_rules! regex {
    ($name:ident, $pattern:literal) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pattern).unwrap());
    };
}

regex!(
    COMMAND_NAME,
    r"<command-name>\s*(/[^<\s]+)\s*</command-name>"
);
regex!(COMMAND_ARGS, r"<command-args>([^<]*)</command-args>");
regex!(BASH_INPUT, r"(?s)^\s*<bash-input>(.*?)</bash-input>");
regex!(
    SHELL_COMMAND,
    r"(?s)^\s*<user_shell_command>.*?<command>(.*?)</command>"
);
regex!(IMAGE_SOURCE, r"\[Image: source: [^\]]+\]");
regex!(IMAGE_NAME, r"<image name=[^>]*>");
regex!(
    CAVEAT,
    r"(?s)^Caveat: The messages below.*?</command-message>"
);
regex!(PROMPT_OPEN, r"^<tx-command-prompt[^>]*>");
regex!(PROMPT_CLOSE, r"</tx-command-prompt>\s*$");
regex!(FROM_AGENT, r#"^<from-agent session="([^"]+)">"#);
regex!(FROM_CLAUDE, r#"^<from-claude session="([^"]+)">"#);
regex!(FROM_CLOSE, r"</from-(agent|claude)>\s*$");
regex!(PEER, r"^\s*<from-(agent|claude)\b");
regex!(PASTED_OPEN, r"^\s*<pasted_content\b[^>]*>\s*");
regex!(PASTED_CLOSE, r"\s*</pasted_content>");
regex!(PEER_TOKEN, r"(?s)^\[peer ([^\]]+)\]\s*(.*)");

const SYSTEM_PATTERNS: [&str; 12] = [
    r"^\s*\[SYSTEM NOTIFICATION",
    r"^\s*<task-notification>",
    r"^\s*<system-reminder>",
    r"^\s*<local-command-caveat",
    r"^\s*<bash-stdout",
    r"^\s*<bash-stderr",
    r"^\s*Caveat: The messages below",
    r"^\s*<local-command-stdout>",
    r"^\s*\[Request interrupted",
    r"^\s*This session is being continued from",
    r"^\s*<(environment_context|user_instructions|permissions|turn_context|ENVIRONMENT|turn_aborted)",
    r"^\s*# AGENTS\.md",
];
static SYSTEM_RX: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    SYSTEM_PATTERNS
        .iter()
        .map(|pattern| Regex::new(pattern).unwrap())
        .collect()
});

/// A message the harness delivered as pasted content (`<pasted_content id="…">…</pasted_content>`
/// — long peer envelopes arrive like that) is classified and displayed by what is inside the
/// wrapper. Not in the Python reference, which misses such envelopes.
fn unwrap_pasted(raw: &str) -> String {
    if !PASTED_OPEN.is_match(raw) {
        return raw.to_owned();
    }
    let inner = PASTED_OPEN.replace(raw, "");
    PASTED_CLOSE.replace(&inner, "").into_owned()
}

/// The display form of what the human typed (slash commands, `!` shell lines, pastes, peer
/// envelopes folded to a `[peer NAME]` prefix).
pub fn clean_user(raw: &str) -> String {
    let unwrapped = unwrap_pasted(raw);
    let raw = unwrapped.as_str();
    if let Some(found) = COMMAND_NAME.captures(raw) {
        let arguments = COMMAND_ARGS
            .captures(raw)
            .map(|captures| captures[1].trim().to_owned())
            .filter(|arguments| !arguments.is_empty());
        return match arguments {
            Some(arguments) => format!("[cmd] {} {arguments}", &found[1]),
            None => format!("[cmd] {}", &found[1]),
        };
    }
    if let Some(found) = BASH_INPUT.captures(raw) {
        return format!("[!] {}", found[1].trim());
    }
    if let Some(found) = SHELL_COMMAND.captures(raw) {
        return format!("[!] {}", found[1].trim());
    }
    let text = IMAGE_SOURCE.replace_all(raw, "[screenshot]");
    let text = IMAGE_NAME.replace_all(&text, "[screenshot] ");
    let text = CAVEAT.replace(&text, "");
    let text = PROMPT_OPEN.replace(&text, "[pane] ");
    let text = PROMPT_CLOSE.replace(&text, "");
    let text = FROM_AGENT.replace(&text, "[peer $1] ");
    let text = FROM_CLAUDE.replace(&text, "[peer $1] ");
    let text = FROM_CLOSE.replace(&text, "");
    text.trim().to_owned()
}

/// `u` = genuinely the human; `p` = peer-agent mail; `s` = system/injected.
pub fn user_kind(raw: &str) -> Kind {
    let unwrapped = unwrap_pasted(raw);
    let raw = unwrapped.as_str();
    if PEER.is_match(raw) {
        return Kind::Peer;
    }
    if SYSTEM_RX.iter().any(|regex| regex.is_match(raw)) {
        return Kind::System;
    }
    Kind::User
}

/// `[peer TOKEN] snippet` → `(token, snippet)`.
pub fn peer_parts(text: &str) -> Option<(String, String)> {
    let found = PEER_TOKEN.captures(text)?;
    Some((found[1].trim().to_owned(), found[2].to_owned()))
}

pub fn tool_line(name: &str, detail: &str) -> String {
    let detail = WHITESPACE.replace_all(detail, " ");
    let detail = detail.trim();
    if detail.is_empty() {
        format!("⚙ {name}")
    } else {
        format!("⚙ {name} · {}", cap(detail, TOOL_DETAIL))
    }
}

pub(super) fn iso_timestamp(value: Option<&Value>) -> Option<f64> {
    let text = value?.as_str()?;
    fromisoformat_timestamp(&text.replace('Z', "+00:00"))
}

/// Python `str(value)` for a tool-input field: strings verbatim, anything else serialised.
fn stringify(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// The first truthy of `command`, `file_path`, `prompt`, `description`.
fn tool_detail(input: &Value) -> String {
    ["command", "file_path", "prompt", "description"]
        .iter()
        .filter_map(|key| input.get(*key))
        .map(stringify)
        .find(|detail| !detail.is_empty())
        .unwrap_or_default()
}

fn push_user(out: &mut Vec<Row>, at: f64, raw: &str) {
    let kind = user_kind(raw);
    let text = clean_user(raw);
    if text.is_empty() {
        return;
    }
    // A screenshot paste is TWO transcript entries at the identical timestamp (typed text +
    // marker-only) — fold the second into the first instead of doubling the dot.
    if let Some(last) = out.last_mut()
        && kind == Kind::User
        && last.kind == Kind::User
        && last.at == at
    {
        last.text = cap(&format!("{}\n{text}", last.text), ROW_TEXT_CAP);
        return;
    }
    out.push(Row {
        at,
        kind,
        text: cap(&text, ROW_TEXT_CAP),
        probe: None,
    });
}

fn rows_from_claude(entry: &Value, out: &mut Vec<Row>) {
    if entry
        .get("isSidechain")
        .is_some_and(|flag| flag.as_bool() == Some(true))
    {
        return;
    }
    let Some(kind) = entry.get("type").and_then(Value::as_str) else {
        return;
    };
    if kind != "user" && kind != "assistant" {
        return;
    }
    let Some(at) = iso_timestamp(entry.get("timestamp")) else {
        return;
    };
    let content = entry
        .get("message")
        .and_then(|message| message.get("content"));
    if kind == "user" {
        let raw = match content {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|block| block.get("text").and_then(Value::as_str))
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => String::new(),
        };
        if !raw.is_empty() {
            push_user(out, at, &raw);
        }
        return;
    }
    let Some(Value::Array(blocks)) = content else {
        return;
    };
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if !text.is_empty() {
                    out.push(Row {
                        at,
                        kind: Kind::Agent,
                        text: cap(text, ROW_TEXT_CAP),
                        probe: None,
                    });
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("?");
                let detail = block.get("input").map(tool_detail).unwrap_or_default();
                out.push(Row {
                    at,
                    kind: Kind::Tool,
                    text: tool_line(name, &detail),
                    probe: milestone_probe(&detail),
                });
            }
            _ => {}
        }
    }
}

fn rows_from_codex(entry: &Value, out: &mut Vec<Row>) {
    let Some(at) = iso_timestamp(entry.get("timestamp")) else {
        return;
    };
    let payload = entry.get("payload").cloned().unwrap_or(Value::Null);
    match payload.get("type").and_then(Value::as_str) {
        Some("message") => {
            let text = payload
                .get("content")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter(|item| {
                            matches!(
                                item.get("type").and_then(Value::as_str),
                                Some("input_text" | "output_text")
                            )
                        })
                        .map(|item| item.get("text").and_then(Value::as_str).unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            match payload.get("role").and_then(Value::as_str) {
                Some("user") => {
                    let kind = user_kind(text);
                    let cleaned = clean_user(text);
                    if !cleaned.is_empty() {
                        out.push(Row {
                            at,
                            kind,
                            text: cap(&cleaned, ROW_TEXT_CAP),
                            probe: None,
                        });
                    }
                }
                Some("assistant") => out.push(Row {
                    at,
                    kind: Kind::Agent,
                    text: cap(text, ROW_TEXT_CAP),
                    probe: None,
                }),
                _ => {}
            }
        }
        Some("function_call" | "local_shell_call") => {
            let name = payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("shell");
            let mut detail = String::new();
            if let Some(arguments) = payload.get("arguments").and_then(Value::as_str) {
                detail = match serde_json::from_str::<Value>(arguments) {
                    Ok(parsed) => match parsed.get("command") {
                        Some(Value::String(command)) => command.clone(),
                        Some(Value::Array(words)) => {
                            words.iter().map(stringify).collect::<Vec<_>>().join(" ")
                        }
                        _ => arguments.chars().take(200).collect(),
                    },
                    Err(_) => arguments.chars().take(200).collect(),
                };
            }
            if detail.is_empty()
                && let Some(action) = payload.get("action").filter(|action| action.is_object())
            {
                detail = match action.get("command") {
                    Some(Value::Array(words)) => {
                        words.iter().map(stringify).collect::<Vec<_>>().join(" ")
                    }
                    Some(Value::Null) | None => String::new(),
                    Some(other) => stringify(other),
                };
            }
            out.push(Row {
                at,
                kind: Kind::Tool,
                text: tool_line(name, &detail),
                probe: milestone_probe(&detail),
            });
        }
        _ => {}
    }
}

/// One JSONL line → zero or more rows appended to `out`.
pub fn parse_line(line: &str, out: &mut Vec<Row>) {
    let Ok(entry) = serde_json::from_str::<Value>(line) else {
        return;
    };
    parse_entry(&entry, out);
}

pub(super) fn parse_entry(entry: &Value, out: &mut Vec<Row>) {
    if !entry.is_object() {
        return;
    }
    if entry.get("payload").is_some() {
        rows_from_codex(entry, out);
    } else {
        rows_from_claude(entry, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_taxonomy() {
        assert_eq!(
            user_kind("<from-agent session=\"x\">hi</from-agent>"),
            Kind::Peer
        );
        assert_eq!(user_kind("  <system-reminder>…"), Kind::System);
        assert_eq!(user_kind("do the thing"), Kind::User);
    }

    #[test]
    fn pasted_wrapper_is_looked_through() {
        let raw = "\n\n<pasted_content id=\"ce6d\">\n<from-agent session=\"nowcast-w7-review\">W7 done</from-agent>\n</pasted_content>";
        assert_eq!(user_kind(raw), Kind::Peer);
        assert_eq!(clean_user(raw), "[peer nowcast-w7-review] W7 done");
        assert_eq!(
            user_kind("<pasted_content id=\"x\">plain paste</pasted_content>"),
            Kind::User
        );
        assert_eq!(
            clean_user("<pasted_content id=\"x\">plain paste</pasted_content>"),
            "plain paste"
        );
    }

    #[test]
    fn clean_user_forms() {
        assert_eq!(
            clean_user("<command-name>/loop</command-name><command-args> 5m </command-args>"),
            "[cmd] /loop 5m"
        );
        assert_eq!(clean_user(" <bash-input>ls -la</bash-input>"), "[!] ls -la");
        assert_eq!(
            clean_user("<from-agent session=\"and-48\">ping</from-agent>"),
            "[peer and-48] ping"
        );
        assert_eq!(
            peer_parts("[peer and-48] ping"),
            Some(("and-48".into(), "ping".into()))
        );
    }

    #[test]
    fn milestone_detection() {
        assert_eq!(
            milestone_label("tx artifact create --title x"),
            Some("artifact")
        );
        assert_eq!(milestone_probe("tx artifact create --help"), None);
        assert_eq!(milestone_probe("tx send-message x 'gh pr merge'"), None);
        assert_eq!(
            milestone_probe("cd /x && gh pr merge 12").as_deref(),
            Some("gh pr merge 12")
        );
    }

    #[test]
    fn cap_counts_chars() {
        assert_eq!(cap("ééé", 2), "éé …[+1 chars]");
        assert_eq!(cap("  ab  ", 5), "ab");
    }

    #[test]
    fn claude_user_fold_and_tools() {
        let mut out = Vec::new();
        let base =
            r#"{"type":"user","timestamp":"2026-10-11T08:00:00Z","message":{"content":"hello"}}"#;
        parse_line(base, &mut out);
        parse_line(
            r#"{"type":"user","timestamp":"2026-10-11T08:00:00Z","message":{"content":[{"type":"text","text":"[Image: source: x]"}]}}"#,
            &mut out,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "hello\n[screenshot]");
        parse_line(
            r#"{"type":"assistant","timestamp":"2026-10-11T08:00:01Z","message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"tx artifact modify abcd1234 --file x"}}]}}"#,
            &mut out,
        );
        assert_eq!(out[1].kind, Kind::Tool);
        assert!(
            out[1]
                .probe
                .as_deref()
                .unwrap()
                .starts_with("tx artifact modify abcd1234")
        );
    }
}
