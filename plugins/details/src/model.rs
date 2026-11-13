//! What `tx details` reports: an agent's record, its last launch's prompt, and the profile its
//! definition and roles resolve to now. Nothing is read from the transcript or the process.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use serde::Serialize;
use serde_json::Value;
use tx_core::definitions::{Definitions, McpServer};
use tx_core::{Agent, CoreError, ListFilter, ResolvedProfile, TxCore};

#[derive(Clone, Debug, Serialize)]
pub struct Details {
    pub id: String,
    pub name: String,
    pub engine: String,
    pub state: String,
    pub created_at: f64,
    pub last_activity_at: Option<f64>,
    pub working_directory: PathBuf,
    pub branch: Option<String>,
    pub tags: Vec<String>,
    pub parent: Option<AgentReference>,
    pub children: Vec<AgentReference>,
    pub engine_data: Value,
    pub definition: Option<DefinitionDetails>,
    pub roles: Vec<String>,
    pub prompt: Option<PromptDetails>,
    /// What the definition and roles resolve to now; `None` when they no longer resolve (a role
    /// or definition since removed), with the reason in `profile_error`.
    #[serde(flatten)]
    pub profile: Option<ProfileDetails>,
    pub profile_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileDetails {
    /// `None`: no definition, the engine's own base set.
    pub tools: Option<Vec<String>>,
    pub disallowed_tools: Vec<String>,
    pub mcp_servers: BTreeMap<String, McpDetails>,
    pub skills: Vec<SkillDetails>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AgentReference {
    pub id: String,
    pub name: String,
    pub state: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DefinitionDetails {
    pub agent: String,
    /// The file the definition resolves from now; `None` when no root holds it any more.
    pub source: Option<PathBuf>,
    /// `true`: from the shipped definitions, not the operator's own.
    pub shipped: bool,
    pub extras: Extras,
}

#[derive(Clone, Debug, Serialize)]
pub struct Extras {
    pub skills: Vec<String>,
    pub mcp_servers: Vec<String>,
    pub tools: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PromptDetails {
    pub path: PathBuf,
    pub bytes: usize,
    pub written_at: f64,
    pub headings: Vec<String>,
    /// Only with `--prompt`.
    pub text: Option<String>,
}

/// An MCP server with its secrets left out: `env` and `headers` are key names only.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpDetails {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<String>,
    },
    Http {
        url: String,
        headers: Vec<String>,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct SkillDetails {
    pub name: String,
    /// `tx` for a granted directory, else the engine provider (`claude-bundled`, …).
    pub provider: String,
    pub path: Option<PathBuf>,
}

pub fn details(core: &TxCore, agent: &Agent, with_prompt: bool) -> Result<Details, CoreError> {
    let parent = match &agent.parent_id {
        Some(parent_id) => Some(reference(&core.get_agent(parent_id)?)),
        None => None,
    };
    let mut children = Vec::new();
    for child_id in core.list_agents(&ListFilter::new().parent(agent.id.clone()))? {
        children.push(reference(&core.get_agent(&child_id)?));
    }
    let definition_name = agent
        .definition
        .as_ref()
        .map(|choice| choice.agent.clone())
        .or_else(|| core.config().agent_defaults.definition.clone());
    let definitions = Definitions::new(core.config().directories.definition_roots());
    let definition = match definition_name {
        Some(name) => Some(definition_details(core, &definitions, agent, name)?),
        None => None,
    };
    // A record naming no roles runs its definition's `roles`. A definition that no longer reads
    // fails `agent_profile` the same way, reported as `profile_error`.
    let roles = match &definition {
        Some(definition) if agent.roles.is_empty() => definitions
            .agent(&definition.agent)
            .map(|read| read.roles)
            .unwrap_or_default(),
        _ => agent.roles.clone(),
    };
    let (profile, profile_error) = match core.agent_profile(agent) {
        Ok(profile) => (Some(profile_details(profile)), None),
        Err(error) => (None, Some(error.to_string())),
    };
    Ok(Details {
        id: agent.id.to_string(),
        name: agent.name.clone(),
        engine: agent.engine.as_str().to_owned(),
        state: agent.state.as_str().to_owned(),
        created_at: agent.created_at,
        last_activity_at: agent.last_activity_at,
        working_directory: agent.working_directory.clone(),
        branch: branch(&agent.working_directory),
        tags: agent.tags.clone(),
        parent,
        children,
        engine_data: agent
            .ext
            .get(agent.engine.as_str())
            .cloned()
            .unwrap_or(Value::Null),
        definition,
        roles,
        prompt: prompt(&core.profile_path(&agent.id), with_prompt),
        profile,
        profile_error,
    })
}

fn profile_details(profile: ResolvedProfile) -> ProfileDetails {
    let mut skills: Vec<SkillDetails> = profile
        .skill_dirs
        .iter()
        .map(|directory| SkillDetails {
            name: directory
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            provider: "tx".to_owned(),
            path: Some(directory.clone()),
        })
        .collect();
    skills.extend(
        profile
            .provided_skills
            .iter()
            .map(|(name, provider)| SkillDetails {
                name: name.clone(),
                provider: provider.as_str().to_owned(),
                path: None,
            }),
    );
    ProfileDetails {
        tools: profile.tools,
        disallowed_tools: profile.disallowed_tools,
        mcp_servers: profile
            .mcp_servers
            .into_iter()
            .map(|(name, server)| (name, mcp_details(server)))
            .collect(),
        skills,
    }
}

fn reference(agent: &Agent) -> AgentReference {
    AgentReference {
        id: agent.id.to_string(),
        name: agent.name.clone(),
        state: agent.state.as_str().to_owned(),
    }
}

fn definition_details(
    core: &TxCore,
    definitions: &Definitions,
    agent: &Agent,
    name: String,
) -> Result<DefinitionDetails, CoreError> {
    let roots = core.config().directories.definition_roots();
    let source = definitions.agent_files()?.remove(&name);
    let shipped = source
        .as_ref()
        .is_some_and(|path| path.starts_with(&roots[0]));
    let choice = agent.definition.clone().unwrap_or_default();
    Ok(DefinitionDetails {
        agent: name,
        source,
        shipped,
        extras: Extras {
            skills: choice.skills,
            mcp_servers: choice.mcp_servers,
            tools: choice.tools,
        },
    })
}

/// The current branch of `directory`; `None` when detached or not a git checkout.
fn branch(directory: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !name.is_empty()).then_some(name)
}

/// The profile file of the last launch; `None` when there is none (never launched, or the launch
/// failed and removed it).
fn prompt(path: &Path, with_text: bool) -> Option<PromptDetails> {
    let text = std::fs::read_to_string(path).ok()?;
    let written_at = std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0.0, |duration| duration.as_secs_f64());
    Some(PromptDetails {
        path: path.to_owned(),
        bytes: text.len(),
        written_at,
        headings: text
            .lines()
            .filter_map(|line| line.strip_prefix("# "))
            .map(str::to_owned)
            .collect(),
        text: with_text.then_some(text),
    })
}

fn mcp_details(server: McpServer) -> McpDetails {
    match server {
        McpServer::Stdio { command, args, env } => McpDetails::Stdio {
            command,
            args,
            env: env.into_keys().collect(),
        },
        McpServer::Http { url, headers } => McpDetails::Http {
            url,
            headers: headers.into_keys().collect(),
        },
    }
}
