//! Definitions: agents (`agents/<name>.md`, Claude's agent format, reduced), skills
//! (`skills/<name>/SKILL.md`, the Agent Skills standard) and MCP servers (`mcp.json`, Claude's
//! `.mcp.json` format), read from layered roots. A later root wins by name: an agent, a skill, or
//! a server key inside `mcp.json`. The roots are `Directories::definition_roots`: the shipped
//! `<data>/definitions/`, then the user's `<config>/definitions/`.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};

use crate::agent::DefinitionChoice;
use crate::profile::{BuiltInTools, ResolvedProfile};

pub const AGENTS_DIR: &str = "agents";
pub const AGENT_SUFFIX: &str = ".md";
pub const SKILLS_DIR: &str = "skills";
pub const SKILL_FILE: &str = "SKILL.md";
pub const MCP_FILE: &str = "mcp.json";
const FRONTMATTER_DELIMITER: &str = "---";

/// A definition file that cannot be used, at `path` and, when known, a 1-based `line`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{}{}: {message}", path.display(), line.map(|line| format!(":{line}")).unwrap_or_default())]
pub struct DefinitionError {
    pub path: PathBuf,
    pub line: Option<usize>,
    pub message: String,
}

impl DefinitionError {
    pub fn new(path: &Path, line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            line,
            message: message.into(),
        }
    }

    fn io(path: &Path, error: io::Error) -> Self {
        Self::new(path, None, error.to_string())
    }
}

/// An agent: its frontmatter and the prompt after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    /// The built-in tools it gets; `None` is the engine's default set.
    pub tools: Option<Vec<String>>,
    pub mcp_servers: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub skills: Vec<String>,
    pub prompt: String,
    pub path: PathBuf,
    /// The frontmatter text, for `line_of`; its line 1 is the file's line 2.
    frontmatter: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AgentFrontmatter {
    name: String,
    description: String,
    #[serde(default)]
    tools: Option<NameList>,
    #[serde(default)]
    mcp_servers: NameList,
    #[serde(default)]
    disallowed_tools: NameList,
    #[serde(default)]
    skills: NameList,
}

/// A list of names, written as a YAML list or as one comma-separated string (`Bash, Read`), as
/// Claude accepts both.
#[derive(Default)]
struct NameList(Vec<String>);

impl<'de> Deserialize<'de> for NameList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NameListVisitor;

        impl<'de> Visitor<'de> for NameListVisitor {
            type Value = NameList;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a list of names or a comma-separated string")
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<NameList, E> {
                Ok(NameList(
                    text.split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .collect(),
                ))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<NameList, A::Error> {
                let mut names = Vec::new();
                while let Some(name) = sequence.next_element::<String>()? {
                    names.push(name);
                }
                Ok(NameList(names))
            }
        }

        deserializer.deserialize_any(NameListVisitor)
    }
}

impl AgentDefinition {
    /// The 1-based line of the file on which the frontmatter key `key` starts, when present.
    pub fn line_of(&self, key: &str) -> Option<usize> {
        self.frontmatter
            .lines()
            .position(|line| {
                line.strip_prefix(key)
                    .is_some_and(|rest| rest.starts_with(':'))
            })
            .map(|index| index + 2)
    }
}

/// A skill: its directory and the two frontmatter keys every engine reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    pub directory: PathBuf,
}

/// The Agent Skills standard allows further keys (`license`, `metadata`, …); only these two are
/// read.
#[derive(Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
}

/// One server of `mcp.json`. `type` may be left out for a stdio server, as in Claude's format.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum McpServer {
    Stdio {
        command: String,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        #[serde(skip_serializing_if = "BTreeMap::is_empty")]
        headers: BTreeMap<String, String>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct McpServerFields {
    #[serde(rename = "type")]
    transport: Option<String>,
    command: Option<String>,
    args: Option<Vec<String>>,
    env: Option<BTreeMap<String, String>>,
    url: Option<String>,
    headers: Option<BTreeMap<String, String>>,
}

impl<'de> Deserialize<'de> for McpServer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let fields = McpServerFields::deserialize(deserializer)?;
        let refuse = |transport: &str, key: &str| {
            de::Error::custom(format!("a {transport} server has no `{key}`"))
        };
        match fields.transport.as_deref().unwrap_or("stdio") {
            "stdio" => {
                if fields.url.is_some() {
                    return Err(refuse("stdio", "url"));
                }
                if fields.headers.is_some() {
                    return Err(refuse("stdio", "headers"));
                }
                Ok(McpServer::Stdio {
                    command: fields
                        .command
                        .ok_or_else(|| de::Error::missing_field("command"))?,
                    args: fields.args.unwrap_or_default(),
                    env: fields.env.unwrap_or_default(),
                })
            }
            "http" => {
                for (key, present) in [
                    ("command", fields.command.is_some()),
                    ("args", fields.args.is_some()),
                    ("env", fields.env.is_some()),
                ] {
                    if present {
                        return Err(refuse("http", key));
                    }
                }
                Ok(McpServer::Http {
                    url: fields.url.ok_or_else(|| de::Error::missing_field("url"))?,
                    headers: fields.headers.unwrap_or_default(),
                })
            }
            other => Err(de::Error::custom(format!(
                "unknown server type {other:?} (tx supports stdio and http)"
            ))),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct McpFile {
    mcp_servers: BTreeMap<String, McpServer>,
}

/// One `mcp.json`, read: its servers and its text (for the line of a server key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpDefinitions {
    pub path: PathBuf,
    pub servers: BTreeMap<String, McpServer>,
    text: String,
}

impl McpDefinitions {
    /// The 1-based line on which the server key `name` is written.
    pub fn line_of(&self, name: &str) -> Option<usize> {
        let quoted = format!("\"{name}\"");
        self.text
            .lines()
            .position(|line| {
                line.split_once(&quoted)
                    .is_some_and(|(_, rest)| rest.trim_start().starts_with(':'))
            })
            .map(|index| index + 1)
    }
}

/// Splits `text` into the frontmatter between the opening and closing `---` lines and the body.
fn split_frontmatter<'a>(
    path: &Path,
    text: &'a str,
) -> Result<(&'a str, &'a str), DefinitionError> {
    let Some(rest) = text
        .strip_prefix(FRONTMATTER_DELIMITER)
        .and_then(|rest| rest.strip_prefix('\n'))
    else {
        return Err(DefinitionError::new(
            path,
            Some(1),
            "the file must open with a `---` frontmatter block",
        ));
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == FRONTMATTER_DELIMITER {
            return Ok((&rest[..offset], &rest[offset + line.len()..]));
        }
        offset += line.len();
    }
    Err(DefinitionError::new(
        path,
        Some(1),
        "unclosed frontmatter block",
    ))
}

/// Parses YAML frontmatter that starts on line 2 of the file at `path`.
fn parse_frontmatter<T: for<'de> Deserialize<'de>>(
    path: &Path,
    frontmatter: &str,
) -> Result<T, DefinitionError> {
    serde_norway::from_str(frontmatter).map_err(|error| {
        let line = error.location().map(|location| location.line() + 1);
        DefinitionError::new(path, line, strip_location(&error.to_string()))
    })
}

/// serde_norway and serde_json end their messages with ` at line L column C`; the line is
/// reported as `path:line` instead.
fn strip_location(message: &str) -> String {
    match message.rfind(" at line ") {
        Some(index) => message[..index].to_owned(),
        None => message.to_owned(),
    }
}

fn stem_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default()
        .strip_suffix(AGENT_SUFFIX)
        .unwrap_or_default()
        .to_owned()
}

/// Reads `agents/<name>.md`; `name` must match the file name.
pub fn read_agent(path: &Path) -> Result<AgentDefinition, DefinitionError> {
    let text = std::fs::read_to_string(path).map_err(|error| DefinitionError::io(path, error))?;
    let (frontmatter, body) = split_frontmatter(path, &text)?;
    let parsed: AgentFrontmatter = parse_frontmatter(path, frontmatter)?;
    let agent = AgentDefinition {
        name: parsed.name,
        description: parsed.description,
        tools: parsed.tools.map(|tools| tools.0),
        mcp_servers: parsed.mcp_servers.0,
        disallowed_tools: parsed.disallowed_tools.0,
        skills: parsed.skills.0,
        prompt: body.trim().to_owned(),
        path: path.to_owned(),
        frontmatter: frontmatter.to_owned(),
    };
    let stem = stem_of(path);
    if agent.name != stem {
        return Err(DefinitionError::new(
            path,
            agent.line_of("name"),
            format!(
                "name {:?} does not match the file name {stem:?}",
                agent.name
            ),
        ));
    }
    Ok(agent)
}

/// Reads `skills/<name>/SKILL.md`; `name` must match the directory name.
pub fn read_skill(directory: &Path) -> Result<SkillDefinition, DefinitionError> {
    let path = directory.join(SKILL_FILE);
    let text = std::fs::read_to_string(&path).map_err(|error| DefinitionError::io(&path, error))?;
    let (frontmatter, _) = split_frontmatter(&path, &text)?;
    let parsed: SkillFrontmatter = parse_frontmatter(&path, frontmatter)?;
    let directory_name = directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if parsed.name != directory_name {
        return Err(DefinitionError::new(
            &path,
            frontmatter
                .lines()
                .position(|line| line.starts_with("name:"))
                .map(|index| index + 2),
            format!(
                "name {:?} does not match the directory name {directory_name:?}",
                parsed.name
            ),
        ));
    }
    Ok(SkillDefinition {
        name: parsed.name,
        description: parsed.description,
        directory: directory.to_owned(),
    })
}

/// Reads one `mcp.json`.
pub fn read_mcp(path: &Path) -> Result<McpDefinitions, DefinitionError> {
    let text = std::fs::read_to_string(path).map_err(|error| DefinitionError::io(path, error))?;
    let file: McpFile = serde_json::from_str(&text).map_err(|error| {
        DefinitionError::new(path, Some(error.line()), strip_location(&error.to_string()))
    })?;
    Ok(McpDefinitions {
        path: path.to_owned(),
        servers: file.mcp_servers,
        text,
    })
}

/// The entries of `directory`, sorted; none when it does not exist.
fn entries(directory: &Path) -> Result<Vec<PathBuf>, DefinitionError> {
    let read = match std::fs::read_dir(directory) {
        Ok(read) => read,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(DefinitionError::io(directory, error)),
    };
    let mut paths = read
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| DefinitionError::io(directory, error))?;
    paths.sort();
    Ok(paths)
}

/// The definition roots, lowest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definitions {
    roots: Vec<PathBuf>,
}

impl Definitions {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        Self {
            roots: roots.into_iter().collect(),
        }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Every agent name with the file that wins for it.
    pub fn agent_files(&self) -> Result<BTreeMap<String, PathBuf>, DefinitionError> {
        let mut files = BTreeMap::new();
        for root in &self.roots {
            for path in entries(&root.join(AGENTS_DIR))? {
                if path.is_file() && path.extension().is_some_and(|extension| extension == "md") {
                    files.insert(stem_of(&path), path);
                }
            }
        }
        Ok(files)
    }

    /// Every skill name with the directory that wins for it: one holding a `SKILL.md`.
    pub fn skill_directories(&self) -> Result<BTreeMap<String, PathBuf>, DefinitionError> {
        let mut directories = BTreeMap::new();
        for root in &self.roots {
            for path in entries(&root.join(SKILLS_DIR))? {
                if path.join(SKILL_FILE).is_file() {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    directories.insert(name, path);
                }
            }
        }
        Ok(directories)
    }

    /// Every `mcp.json` that exists, lowest root first.
    pub fn mcp_files(&self) -> Vec<PathBuf> {
        self.roots
            .iter()
            .map(|root| root.join(MCP_FILE))
            .filter(|path| path.is_file())
            .collect()
    }

    pub fn agent(&self, name: &str) -> Result<AgentDefinition, DefinitionError> {
        match self.agent_files()?.get(name) {
            Some(path) => read_agent(path),
            None => Err(self.unknown("agent", name, AGENTS_DIR)),
        }
    }

    pub fn skill(&self, name: &str) -> Result<SkillDefinition, DefinitionError> {
        match self.skill_directories()?.get(name) {
            Some(directory) => read_skill(directory),
            None => Err(self.unknown("skill", name, SKILLS_DIR)),
        }
    }

    /// Every server across the `mcp.json` files, a later file's key winning, with the file it
    /// came from.
    pub fn mcp_servers(&self) -> Result<BTreeMap<String, (McpServer, PathBuf)>, DefinitionError> {
        let mut servers = BTreeMap::new();
        for path in self.mcp_files() {
            for (name, server) in read_mcp(&path)?.servers {
                servers.insert(name, (server, path.clone()));
            }
        }
        Ok(servers)
    }

    fn unknown(&self, kind: &str, name: &str, directory: &str) -> DefinitionError {
        let searched = self
            .roots
            .iter()
            .map(|root| root.join(directory).display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let user = self.roots.last().map(|root| root.join(directory));
        DefinitionError::new(
            &user.unwrap_or_default(),
            None,
            format!("unknown {kind} {name:?} (searched {searched})"),
        )
    }

    /// `resolved` (the roles' profile) with the agent definition `choice` on top:
    /// - the text is the roles' text, then the agent's prompt;
    /// - the skills are the roles' grants, then the agent's and the `--skill` extras, each name
    ///   once;
    /// - the tools are every built-in when the agent has no `tools`, else its list with the
    ///   `--tool` extras added;
    /// - `disallowedTools` is the agent's;
    /// - the MCP servers are the agent's and the `--mcp` extras, from the winning `mcp.json` keys.
    pub fn apply(
        &self,
        mut resolved: ResolvedProfile,
        choice: &DefinitionChoice,
    ) -> Result<ResolvedProfile, DefinitionError> {
        let agent = self.agent(&choice.agent)?;
        if !agent.prompt.is_empty() {
            resolved.text = [resolved.text.as_str(), agent.prompt.as_str()]
                .into_iter()
                .filter(|text| !text.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
        }
        let skill_directories = self.skill_directories()?;
        for name in agent.skills.iter().chain(&choice.skills) {
            let directory = skill_directories
                .get(name)
                .ok_or_else(|| self.unknown("skill", name, SKILLS_DIR))?;
            if !resolved
                .skill_dirs
                .iter()
                .any(|granted| granted.file_name() == directory.file_name())
            {
                resolved.skill_dirs.push(directory.clone());
            }
        }
        resolved.tools = match agent.tools {
            None => BuiltInTools::All,
            Some(mut tools) => {
                for tool in &choice.tools {
                    if !tools.contains(tool) {
                        tools.push(tool.clone());
                    }
                }
                BuiltInTools::Listed(tools)
            }
        };
        resolved.disallowed_tools = agent.disallowed_tools;
        let servers = self.mcp_servers()?;
        for name in agent.mcp_servers.iter().chain(&choice.mcp_servers) {
            let (server, _) = servers
                .get(name)
                .ok_or_else(|| self.unknown("MCP server", name, MCP_FILE))?;
            resolved.mcp_servers.insert(name.clone(), server.clone());
        }
        Ok(resolved)
    }

    /// Validates the winning definitions: every agent, skill and `mcp.json` when `names` is
    /// empty, else the agents, skills and servers of those names. Returns what was checked and
    /// every problem found, not just the first.
    pub fn check(&self, names: &[String]) -> Result<CheckReport, DefinitionError> {
        let mut report = CheckReport::default();
        let agent_files = self.agent_files()?;
        let skill_directories = self.skill_directories()?;
        let mut servers: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut mcp_files = Vec::new();
        for path in self.mcp_files() {
            match read_mcp(&path) {
                Ok(definitions) => {
                    for name in definitions.servers.keys() {
                        servers.insert(name.clone(), path.clone());
                    }
                    mcp_files.push(definitions);
                }
                Err(error) => report.problems.push(error),
            }
        }
        let wanted = |name: &str| names.is_empty() || names.iter().any(|wanted| wanted == name);
        for name in names {
            if !agent_files.contains_key(name)
                && !skill_directories.contains_key(name)
                && !servers.contains_key(name)
            {
                report.problems.push(DefinitionError::new(
                    self.roots
                        .last()
                        .map(PathBuf::as_path)
                        .unwrap_or(Path::new("")),
                    None,
                    format!("no agent, skill or MCP server named {name:?}"),
                ));
            }
        }
        for (name, path) in agent_files.iter().filter(|(name, _)| wanted(name)) {
            report.agents += 1;
            if !is_bare_name(name) {
                report.problems.push(DefinitionError::new(
                    path,
                    None,
                    format!("agent name {name:?} must match [A-Za-z0-9_-]+"),
                ));
            }
            match read_agent(path) {
                Ok(agent) => {
                    check_agent(&agent, &skill_directories, &servers, &mut report.problems)
                }
                Err(error) => report.problems.push(error),
            }
        }
        for (name, directory) in skill_directories.iter().filter(|(name, _)| wanted(name)) {
            report.skills += 1;
            if !is_bare_name(name) {
                report.problems.push(DefinitionError::new(
                    directory,
                    None,
                    format!("skill name {name:?} must match [A-Za-z0-9_-]+"),
                ));
            }
            if let Err(error) = read_skill(directory) {
                report.problems.push(error);
            }
        }
        for definitions in &mcp_files {
            for (name, server) in definitions
                .servers
                .iter()
                .filter(|(name, _)| wanted(name))
                // A server a later file overrides is not checked.
                .filter(|(name, _)| servers.get(*name) == Some(&definitions.path))
            {
                report.servers += 1;
                check_server(definitions, name, server, &mut report.problems);
            }
        }
        Ok(report)
    }
}

/// What `Definitions::check` looked at, and what is wrong.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckReport {
    pub agents: usize,
    pub skills: usize,
    pub servers: usize,
    pub problems: Vec<DefinitionError>,
}

/// Agent, skill and server names become path fragments and tool-name parts.
fn is_bare_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

const MCP_TOOL_PREFIX: &str = "mcp__";

/// The references of one agent: its skills and servers exist, `tools` names no MCP tool (Claude
/// ignores those there), and an `mcp__<server>` cut names one of its servers.
fn check_agent(
    agent: &AgentDefinition,
    skill_directories: &BTreeMap<String, PathBuf>,
    servers: &BTreeMap<String, PathBuf>,
    problems: &mut Vec<DefinitionError>,
) {
    let problem =
        |key: &str, message: String| DefinitionError::new(&agent.path, agent.line_of(key), message);
    for skill in &agent.skills {
        if !skill_directories.contains_key(skill) {
            problems.push(problem("skills", format!("unknown skill {skill:?}")));
        }
    }
    for server in &agent.mcp_servers {
        if !servers.contains_key(server) {
            problems.push(problem(
                "mcpServers",
                format!("unknown MCP server {server:?} (not in any {MCP_FILE})"),
            ));
        }
    }
    for tool in agent.tools.iter().flatten() {
        if tool.starts_with(MCP_TOOL_PREFIX) {
            problems.push(problem(
                "tools",
                format!(
                    "{tool:?}: `tools` lists built-in tools only; list the server in \
                     `mcpServers` and cut its tools with `disallowedTools`"
                ),
            ));
        }
    }
    for tool in &agent.disallowed_tools {
        let Some(rest) = tool.strip_prefix(MCP_TOOL_PREFIX) else {
            continue;
        };
        let server = rest.split("__").next().unwrap_or_default();
        if !agent.mcp_servers.iter().any(|listed| listed == server) {
            problems.push(problem(
                "disallowedTools",
                format!("{tool:?} cuts from {server:?}, which is not in `mcpServers`"),
            ));
        }
    }
}

/// `${VAR}` stands for a secret, and only as the whole value of an `env` or `headers` entry, or
/// as `Bearer ${VAR}` in the `Authorization` header: the forms every engine can pass through.
fn check_server(
    definitions: &McpDefinitions,
    name: &str,
    server: &McpServer,
    problems: &mut Vec<DefinitionError>,
) {
    let problem = |message: String| {
        DefinitionError::new(&definitions.path, definitions.line_of(name), message)
    };
    if !is_bare_name(name) {
        problems.push(problem(format!(
            "server name {name:?} must match [A-Za-z0-9_-]+"
        )));
    }
    let (plain, (secrets_key, secrets)): (Vec<(&str, &String)>, _) = match server {
        McpServer::Stdio { command, args, env } => (
            std::iter::once(("command", command))
                .chain(args.iter().map(|argument| ("args", argument)))
                .collect(),
            ("env", env),
        ),
        McpServer::Http { url, headers } => (vec![("url", url)], ("headers", headers)),
    };
    for (key, value) in plain {
        if value.contains("${") {
            problems.push(problem(format!(
                "{name}.{key}: `${{VAR}}` is allowed only as a whole `env` or `headers` value"
            )));
        }
    }
    for (entry, value) in secrets {
        let bearer = secrets_key == "headers" && entry.eq_ignore_ascii_case(AUTHORIZATION_HEADER);
        let allowed =
            variable_name(value).is_some() || (bearer && bearer_variable(value).is_some());
        if value.contains("${") && !allowed {
            problems.push(problem(format!(
                "{name}.{secrets_key}.{entry}: a variable must be the whole value, `${{NAME}}`{}",
                if bearer { " (or `Bearer ${NAME}`)" } else { "" }
            )));
        }
    }
}

/// The one header that may hold a variable inside its value: `Bearer ${NAME}`, which Codex
/// passes as `bearer_token_env_var`.
pub const AUTHORIZATION_HEADER: &str = "Authorization";
const BEARER_PREFIX: &str = "Bearer ";

/// `NAME` when `value` is exactly `${NAME}`.
pub fn variable_name(value: &str) -> Option<&str> {
    value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .filter(|name| {
            name.chars()
                .next()
                .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// `NAME` when `value` is exactly `Bearer ${NAME}`.
pub fn bearer_variable(value: &str) -> Option<&str> {
    value.strip_prefix(BEARER_PREFIX).and_then(variable_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn an_agent_reads_lists_and_comma_strings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents/pm.md");
        write(
            &path,
            "---\nname: pm\ndescription: Triages.\ntools: Bash, Read\nmcpServers: [linear]\n\
             disallowedTools: mcp__linear__delete_comment\nskills:\n  - tx-artifacts\n---\n\nThe prompt.\n",
        );
        let agent = read_agent(&path).unwrap();
        assert_eq!(
            agent.tools,
            Some(vec!["Bash".to_owned(), "Read".to_owned()])
        );
        assert_eq!(agent.mcp_servers, ["linear"]);
        assert_eq!(agent.disallowed_tools, ["mcp__linear__delete_comment"]);
        assert_eq!(agent.skills, ["tx-artifacts"]);
        assert_eq!(agent.prompt, "The prompt.");
        assert_eq!(agent.line_of("skills"), Some(7));
    }

    #[test]
    fn an_unknown_key_is_refused_at_its_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents/pm.md");
        write(&path, "---\nname: pm\ndescription: x\nmodel: opus\n---\n");
        let error = read_agent(&path).unwrap_err();
        assert_eq!(error.line, Some(4));
        assert!(
            error.message.starts_with("unknown field `model`"),
            "{error}"
        );
    }

    #[test]
    fn the_name_must_match_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agents/pm.md");
        write(&path, "---\nname: other\ndescription: x\n---\n");
        let error = read_agent(&path).unwrap_err();
        assert_eq!(error.line, Some(2));
    }

    #[test]
    fn servers_default_to_stdio_and_refuse_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp.json");
        write(
            &path,
            r#"{"mcpServers": {"probe": {"command": "python3", "args": ["p.py"]},
                "linear": {"type": "http", "url": "https://mcp.linear.app/mcp"}}}"#,
        );
        let definitions = read_mcp(&path).unwrap();
        assert!(matches!(
            definitions.servers["probe"],
            McpServer::Stdio { .. }
        ));
        assert_eq!(definitions.line_of("linear"), Some(2));
        write(
            &path,
            "{\"mcpServers\": {\n  \"x\": {\"type\": \"http\", \"url\": \"u\", \"command\": \"c\"}}}",
        );
        let error = read_mcp(&path).unwrap_err();
        assert_eq!(error.message, "a http server has no `command`");
        assert_eq!(error.line, Some(2));
    }

    #[test]
    fn a_later_root_wins_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let (shipped, user) = (dir.path().join("data"), dir.path().join("config"));
        write(
            &shipped.join("agents/a.md"),
            "---\nname: a\ndescription: shipped\n---\n",
        );
        write(
            &user.join("agents/a.md"),
            "---\nname: a\ndescription: yours\n---\n",
        );
        write(
            &shipped.join("skills/s/SKILL.md"),
            "---\nname: s\ndescription: d\n---\n",
        );
        write(
            &shipped.join("mcp.json"),
            r#"{"mcpServers": {"m": {"command": "a"}}}"#,
        );
        write(
            &user.join("mcp.json"),
            r#"{"mcpServers": {"m": {"command": "b"}}}"#,
        );
        let definitions = Definitions::new([shipped, user.clone()]);
        assert_eq!(definitions.agent("a").unwrap().description, "yours");
        assert_eq!(definitions.skill("s").unwrap().description, "d");
        let servers = definitions.mcp_servers().unwrap();
        assert_eq!(servers["m"].1, user.join("mcp.json"));
        assert!(definitions.agent("missing").is_err());
    }

    #[test]
    fn apply_puts_the_definition_and_its_extras_on_the_roles() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        write(
            &root.join("agents/pm.md"),
            "---\nname: pm\ndescription: d\ntools: Bash\nmcpServers: [probe]\n\
             disallowedTools: mcp__probe__x\nskills: [a]\n---\nTriage.\n",
        );
        write(
            &root.join("agents/plain.md"),
            "---\nname: plain\ndescription: d\n---\n",
        );
        for skill in ["a", "b"] {
            write(
                &root.join(format!("skills/{skill}/SKILL.md")),
                &format!("---\nname: {skill}\ndescription: d\n---\n"),
            );
        }
        write(
            &root.join("mcp.json"),
            r#"{"mcpServers": {"probe": {"command": "p"}, "web": {"type": "http", "url": "u"}}}"#,
        );
        let definitions = Definitions::new([root.clone()]);
        let roles = ResolvedProfile {
            text: "Roles.".into(),
            skill_dirs: vec![root.join("skills/a")],
            ..ResolvedProfile::default()
        };
        let choice = DefinitionChoice {
            agent: "pm".into(),
            skills: vec!["b".into()],
            mcp_servers: vec!["web".into()],
            tools: vec!["Read".into(), "Bash".into()],
        };
        let resolved = definitions.apply(roles, &choice).unwrap();
        assert_eq!(resolved.text, "Roles.\n\nTriage.");
        assert_eq!(
            resolved.skill_dirs,
            [root.join("skills/a"), root.join("skills/b")]
        );
        assert_eq!(
            resolved.tools,
            BuiltInTools::Listed(vec!["Bash".into(), "Read".into()])
        );
        assert_eq!(resolved.disallowed_tools, ["mcp__probe__x"]);
        assert_eq!(
            resolved.mcp_servers.keys().collect::<Vec<_>>(),
            ["probe", "web"]
        );
        let plain = definitions
            .apply(ResolvedProfile::default(), &DefinitionChoice::new("plain"))
            .unwrap();
        assert_eq!(plain.tools, BuiltInTools::All);
        assert!(plain.mcp_servers.is_empty());
        let unknown = DefinitionChoice {
            mcp_servers: vec!["nope".into()],
            ..DefinitionChoice::new("plain")
        };
        assert!(
            definitions
                .apply(ResolvedProfile::default(), &unknown)
                .is_err()
        );
    }

    #[test]
    fn check_reports_every_problem_with_its_line() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_owned();
        write(
            &root.join("agents/pm.md"),
            "---\nname: pm\ndescription: d\ntools: Bash, mcp__probe__get\nmcpServers: [probe, nope]\n\
             disallowedTools: mcp__other__x\nskills: [missing]\n---\n",
        );
        write(&root.join("agents/bad.md"), "---\nname: bad\n---\n");
        write(
            &root.join("mcp.json"),
            "{\"mcpServers\": {\n  \"probe\": {\"command\": \"${BIN}\", \"env\": {\"A\": \"x${B}\", \"C\": \"${C}\"}},\n  \"web\": {\"type\": \"http\", \"url\": \"u\", \"headers\": {\"Authorization\": \"Bearer ${T}\", \"X\": \"Bearer ${T}\"}}\n}}",
        );
        let report = Definitions::new([root.clone()]).check(&[]).unwrap();
        let lines: Vec<String> = report
            .problems
            .iter()
            .map(|problem| {
                let path = problem
                    .path
                    .strip_prefix(&root)
                    .unwrap()
                    .display()
                    .to_string();
                format!("{path}:{}: {}", problem.line.unwrap_or(0), problem.message)
            })
            .collect();
        assert_eq!(
            lines,
            [
                "agents/bad.md:2: missing field `description`",
                "agents/pm.md:7: unknown skill \"missing\"",
                "agents/pm.md:5: unknown MCP server \"nope\" (not in any mcp.json)",
                "agents/pm.md:4: \"mcp__probe__get\": `tools` lists built-in tools only; list the \
                 server in `mcpServers` and cut its tools with `disallowedTools`",
                "agents/pm.md:6: \"mcp__other__x\" cuts from \"other\", which is not in `mcpServers`",
                "mcp.json:2: probe.command: `${VAR}` is allowed only as a whole `env` or `headers` value",
                "mcp.json:2: probe.env.A: a variable must be the whole value, `${NAME}`",
                "mcp.json:3: web.headers.X: a variable must be the whole value, `${NAME}`",
            ]
        );
        assert_eq!((report.agents, report.skills, report.servers), (2, 0, 2));
        let named = Definitions::new([root])
            .check(&["probe".to_owned(), "x".to_owned()])
            .unwrap();
        assert_eq!((named.agents, named.servers), (0, 1));
        assert_eq!(
            named.problems[0].message,
            "no agent, skill or MCP server named \"x\""
        );
    }
}
