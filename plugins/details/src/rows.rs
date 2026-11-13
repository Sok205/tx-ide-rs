//! [`Details`] as display rows, shared by the plain-text output and the popup: each front end
//! styles the same rows its own way.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use tx_client::render::reltime;

use crate::model::{Details, McpDetails, ProfileDetails};

pub enum Row {
    /// The agent's name and id.
    Title(String, String),
    Heading(String),
    Field(String, String),
    Item(String),
    Note(String),
    Blank,
    /// A line of the prompt text (`--prompt`).
    Text(String),
}

const NONE: &str = "—";

pub fn rows(details: &Details) -> Vec<Row> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |duration| duration.as_secs_f64());
    let field = |label: &str, value: String| Row::Field(label.to_owned(), value);
    let mut rows = vec![
        Row::Title(details.name.clone(), details.id.clone()),
        field("engine", details.engine.clone()),
        field("state", details.state.clone()),
        field(
            "active",
            format!(
                "{} ago   created {} ago",
                reltime(details.last_activity_at, now),
                reltime(Some(details.created_at), now)
            ),
        ),
        field("cwd", home_relative(&details.working_directory)),
        field(
            "branch",
            details
                .branch
                .clone()
                .unwrap_or_else(|| "(detached)".into()),
        ),
        field("tags", list_or_none(&details.tags)),
        field(
            "parent",
            details
                .parent
                .as_ref()
                .map_or(NONE.to_owned(), |parent| parent.name.clone()),
        ),
        field(
            "children",
            list_or_none(
                &details
                    .children
                    .iter()
                    .map(|child| match child.state.as_str() {
                        "ended" => format!("{} (ended)", child.name),
                        _ => child.name.clone(),
                    })
                    .collect::<Vec<_>>(),
            ),
        ),
        Row::Blank,
        Row::Heading(format!("engine data (ext.{})", details.engine)),
    ];
    match &details.engine_data {
        Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                let text = match value {
                    Value::String(text) => home_relative(Path::new(text)),
                    other => other.to_string(),
                };
                rows.push(field(key, text));
            }
        }
        _ => rows.push(Row::Item(NONE.into())),
    }

    rows.push(Row::Blank);
    match &details.definition {
        Some(definition) => {
            let source = definition.source.as_ref().map_or_else(
                || "not found in any definitions root".to_owned(),
                |path| {
                    let origin = if definition.shipped {
                        "shipped"
                    } else {
                        "user"
                    };
                    format!("{origin}: {}", home_relative(path))
                },
            );
            rows.push(Row::Heading(format!("definition  {}", definition.agent)));
            rows.push(field("source", source));
            rows.push(field("roles", list_or_none(&details.roles)));
            let extras = &definition.extras;
            let extras: Vec<String> = extras
                .skills
                .iter()
                .map(|name| format!("skill:{name}"))
                .chain(extras.mcp_servers.iter().map(|name| format!("mcp:{name}")))
                .chain(extras.tools.iter().map(|name| format!("tool:{name}")))
                .collect();
            rows.push(field("extras", list_or_none(&extras)));
        }
        None => {
            rows.push(Row::Heading(format!("definition  {NONE}")));
            rows.push(field("roles", list_or_none(&details.roles)));
        }
    }

    rows.push(Row::Blank);
    match &details.prompt {
        Some(prompt) => {
            rows.push(Row::Heading("prompt".into()));
            rows.push(field("file", home_relative(&prompt.path)));
            rows.push(field(
                "written",
                format!(
                    "{} ago, {} bytes",
                    reltime(Some(prompt.written_at), now),
                    prompt.bytes
                ),
            ));
            for heading in &prompt.headings {
                rows.push(Row::Item(format!("# {heading}")));
            }
        }
        None => rows.push(Row::Heading(format!("prompt  {NONE} (no launch profile)"))),
    }

    match (&details.profile, &details.profile_error) {
        (Some(profile), _) => profile_rows(&mut rows, details, profile),
        (None, error) => {
            rows.push(Row::Blank);
            rows.push(Row::Heading(
                "tools, mcp servers, skills  cannot be resolved".into(),
            ));
            rows.push(Row::Item(error.clone().unwrap_or_default()));
        }
    }

    rows.push(Row::Blank);
    rows.push(Row::Note(
        "tools, MCP servers and skills: resolved from the definitions as they are now; \
         the prompt: from the last launch"
            .into(),
    ));

    if let Some(text) = details
        .prompt
        .as_ref()
        .and_then(|prompt| prompt.text.as_ref())
    {
        rows.push(Row::Blank);
        rows.push(Row::Heading("prompt text".into()));
        rows.extend(text.lines().map(|line| Row::Text(line.to_owned())));
    }
    rows
}

/// The tools, MCP servers and skills sections; create-time extras marked.
fn profile_rows(rows: &mut Vec<Row>, details: &Details, profile: &ProfileDetails) {
    let field = |label: &str, value: String| Row::Field(label.to_owned(), value);
    rows.push(Row::Blank);
    let extra_tools = details
        .definition
        .as_ref()
        .map(|definition| definition.extras.tools.clone())
        .unwrap_or_default();
    match &profile.tools {
        Some(tools) => {
            rows.push(Row::Heading(format!("tools  ({})", tools.len())));
            rows.push(Row::Item(
                tools
                    .iter()
                    .map(|tool| marked(tool, &extra_tools))
                    .collect::<Vec<_>>()
                    .join("  "),
            ));
        }
        None => rows.push(Row::Heading("tools  engine default".into())),
    }
    rows.push(field("disallowed", list_or_none(&profile.disallowed_tools)));

    rows.push(Row::Blank);
    let extra_servers = details
        .definition
        .as_ref()
        .map(|definition| definition.extras.mcp_servers.clone())
        .unwrap_or_default();
    rows.push(Row::Heading(format!(
        "mcp servers  ({})",
        profile.mcp_servers.len()
    )));
    if profile.mcp_servers.is_empty() {
        rows.push(Row::Item(NONE.into()));
    }
    for (name, server) in &profile.mcp_servers {
        let description = match server {
            McpDetails::Stdio { command, args, env } => {
                let mut text = format!(
                    "stdio  {}",
                    [command.clone()]
                        .iter()
                        .chain(args)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                if !env.is_empty() {
                    text.push_str(&format!("   env: {}", env.join(", ")));
                }
                text
            }
            McpDetails::Http { url, headers } => {
                let mut text = format!("http   {url}");
                if !headers.is_empty() {
                    text.push_str(&format!("   headers: {}", headers.join(", ")));
                }
                text
            }
        };
        rows.push(field(&marked(name, &extra_servers), description));
    }

    rows.push(Row::Blank);
    let extra_skills = details
        .definition
        .as_ref()
        .map(|definition| definition.extras.skills.clone())
        .unwrap_or_default();
    rows.push(Row::Heading(format!("skills  ({})", profile.skills.len())));
    if profile.skills.is_empty() {
        rows.push(Row::Item(NONE.into()));
    }
    for skill in &profile.skills {
        let location = skill.path.as_deref().map(home_relative).unwrap_or_default();
        rows.push(field(
            &marked(&skill.name, &extra_skills),
            format!("{:<16}{location}", skill.provider),
        ));
    }
}

/// The widest field label, so every value starts in one column.
pub fn label_width(rows: &[Row]) -> usize {
    rows.iter()
        .filter_map(|row| match row {
            Row::Field(label, _) => Some(label.chars().count()),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn marked(name: &str, extras: &[String]) -> String {
    if extras.iter().any(|extra| extra == name) {
        format!("{name} (extra)")
    } else {
        name.to_owned()
    }
}

fn list_or_none(items: &[String]) -> String {
    if items.is_empty() {
        NONE.to_owned()
    } else {
        items.join(", ")
    }
}

fn home_relative(path: &Path) -> String {
    let text = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && text.starts_with(&home) => {
            format!("~{}", &text[home.len()..])
        }
        _ => text,
    }
}
