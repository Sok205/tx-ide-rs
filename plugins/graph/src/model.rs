//! The graph's data: agent records and the operation log folded into nodes and edges.
//!
//! - Nodes: every running agent, each ended agent active inside the window, and each ancestor
//!   needed to connect them (drawn dim). The operator is a node ("you") when they sent a message
//!   inside the window. `--tag` and `--group` narrow every one of these.
//! - Depth is lineage depth among the shown nodes: a node whose parent is not shown is a root
//!   (in column 1 when the parent record is gone, so the column-0 slot holds its stub).
//! - Edges join a parent to its child (lineage) and any two nodes that exchanged messages inside
//!   the window. One edge per pair: it runs from the left node to the right one, and counts the
//!   messages each way.
//!
//! Messages come from `message-sent` lines of core's `log.jsonl` (`actor` sent to `id`; an
//! empty actor is the operator), read from the end back to the window's start.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;
use tx_chat::transcript::metadata::Metadata;
use tx_core::{Agent, AgentId, CoreError, EngineKind, ListFilter, LlmEngineState, TxCore};

pub const OPERATOR_NAME: &str = "you";
/// The group walk up the parent chain stops at a hub, as the artifacts plugin's does.
pub const HUB_NAMES: [&str; 1] = ["tx-assistant"];
const MESSAGE_SENT: &str = "message-sent";
/// The log is read backwards in blocks of this size until a block starts before the window.
const LOG_BLOCK: u64 = 64 * 1024;

/// One `message-sent` line: `from` is `None` when the operator sent it.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub at: f64,
    pub from: Option<AgentId>,
    pub to: AgentId,
}

/// Context use from the transcript. Claude transcripts report no window, so the window is
/// often unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub tokens: u64,
    pub window: Option<u64>,
}

impl Context {
    pub fn ratio(&self) -> Option<f64> {
        self.window
            .map(|window| (self.tokens as f64 / window as f64).min(1.0))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeState {
    Operator,
    Starting,
    Working,
    Idle,
    /// Idle on a permission prompt or a question. Nothing produces it until the engines keep
    /// the idle reason (plan PR 4); [`node_state`] is where it will be read.
    NeedsYou,
    Ended,
}

impl NodeState {
    pub fn word(self) -> &'static str {
        match self {
            NodeState::Operator => "operator",
            NodeState::Starting => "starting",
            NodeState::Working => "working",
            NodeState::Idle => "idle",
            NodeState::NeedsYou => "needs you",
            NodeState::Ended => "ended",
        }
    }
}

/// The hook point for the "needs you" state: once the record says why an agent is idle, an
/// idle agent waiting on the operator maps to [`NodeState::NeedsYou`] here.
pub fn node_state(agent: &Agent) -> NodeState {
    match agent.state {
        LlmEngineState::Starting => NodeState::Starting,
        LlmEngineState::Working => NodeState::Working,
        LlmEngineState::Idle => NodeState::Idle,
        LlmEngineState::Ended => NodeState::Ended,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// `None` for the operator.
    pub id: Option<AgentId>,
    pub name: String,
    pub state: NodeState,
    pub depth: usize,
    /// The parent's index in [`Graph::nodes`] when the parent is shown.
    pub parent: Option<usize>,
    /// The record names a parent that no longer exists.
    pub parent_lost: bool,
    /// Shown only to connect a descendant: ended and quiet in the window.
    pub connector: bool,
    pub engine: Option<EngineKind>,
    pub definition: Option<String>,
    pub group: String,
    pub tags: Vec<String>,
    pub created_at: f64,
    pub last_activity: Option<f64>,
    pub context: Option<Context>,
}

/// One edge per pair of nodes, from the left node (`from`) to the right one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: usize,
    pub to: usize,
    pub lineage: bool,
    /// Messages sent `from` → `to` inside the window.
    pub forward: u32,
    /// Messages sent `to` → `from` inside the window.
    pub backward: u32,
}

impl Edge {
    pub fn messages(&self) -> u32 {
        self.forward + self.backward
    }
}

/// A message between two shown nodes, by node index.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Traffic {
    pub at: f64,
    pub from: usize,
    pub to: usize,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub tag: Option<String>,
    pub group: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Oldest first.
    pub traffic: Vec<Traffic>,
    /// Every group the unfiltered graph would show (the tag filter still applies), sorted: what
    /// the group filter cycles through.
    pub groups: Vec<String>,
    pub now: f64,
    /// Seconds.
    pub window: f64,
}

impl Graph {
    pub fn columns(&self) -> usize {
        self.nodes
            .iter()
            .map(|node| node.depth)
            .max()
            .map_or(0, |depth| depth + 1)
    }

    pub fn index_of(&self, id: &AgentId) -> Option<usize> {
        self.nodes
            .iter()
            .position(|node| node.id.as_ref() == Some(id))
    }
}

pub fn load_agents(core: &TxCore) -> Result<Vec<Agent>, CoreError> {
    let mut agents = Vec::new();
    for id in core.list_agents(&ListFilter::new())? {
        match core.get_agent(&id) {
            Ok(agent) => agents.push(agent),
            Err(CoreError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(agents)
}

/// The agent's context use, from its current transcript; `None` when nothing is known.
pub fn transcript_context(core: &TxCore, agent: &Agent) -> Option<Context> {
    let conversation = core.conversation(&agent.id).ok()??;
    context_of(&conversation.transcript_path)
}

/// The latest context report in a transcript.
pub fn context_of(transcript: &Path) -> Option<Context> {
    let text = std::fs::read_to_string(transcript).ok()?;
    let mut metadata = Metadata::default();
    for line in text.lines() {
        if let Ok(entry) = serde_json::from_str::<Value>(line) {
            metadata.update(&entry);
        }
    }
    Some(Context {
        tokens: metadata.context_tokens?,
        window: metadata.context_window,
    })
}

/// The `message-sent` lines with `since <= ts <= until`, oldest first. A missing log has none.
pub fn read_messages(log: &Path, since: f64, until: f64) -> io::Result<Vec<Message>> {
    let mut file = match File::open(log) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut start = file.metadata()?.len();
    let mut buffer = Vec::new();
    while start > 0 {
        let block = LOG_BLOCK.min(start);
        start -= block;
        file.seek(SeekFrom::Start(start))?;
        let mut chunk = vec![0; block as usize];
        file.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buffer);
        buffer = chunk;
        let first_complete = if start == 0 {
            Some(&buffer[..])
        } else {
            buffer
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|newline| &buffer[newline + 1..])
        };
        let reached = first_complete
            .and_then(|lines| lines.split(|byte| *byte == b'\n').next())
            .and_then(|line| serde_json::from_slice::<Value>(line).ok())
            .and_then(|entry| entry["ts"].as_f64())
            .is_some_and(|ts| ts < since);
        if reached {
            break;
        }
    }
    let complete = if start == 0 {
        &buffer[..]
    } else {
        let newline = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(buffer.len());
        &buffer[(newline + 1).min(buffer.len())..]
    };
    Ok(complete
        .split(|byte| *byte == b'\n')
        .filter_map(parse_message)
        .filter(|message| since <= message.at && message.at <= until)
        .collect())
}

/// One log line as a [`Message`]; `None` for any other event or a malformed line.
pub fn parse_message(line: &[u8]) -> Option<Message> {
    let entry: Value = serde_json::from_slice(line).ok()?;
    if entry["type"] != MESSAGE_SENT {
        return None;
    }
    let to = entry["id"].as_str()?.parse().ok()?;
    let from = match entry["actor"].as_str()? {
        "" => None,
        actor => Some(actor.parse().ok()?),
    };
    Some(Message {
        at: entry["ts"].as_f64()?,
        from,
        to,
    })
}

fn own_group(agent: &Agent) -> Option<&str> {
    ["edit", "artifacts"].into_iter().find_map(|plugin| {
        agent
            .ext
            .get(plugin)?
            .get("group")?
            .as_str()
            .filter(|group| !group.is_empty())
    })
}

fn is_hub(agent: &Agent) -> bool {
    HUB_NAMES.contains(&agent.name.as_str())
}

/// The artifacts plugin's session group: the record's own override (`ext.edit.group`, else
/// `ext.artifacts.group`), else the first override up the parent chain (stopping after a hub
/// and at a cycle), else the first tag, else the name.
pub fn group_of(agent: &Agent, by_id: &HashMap<&str, &Agent>) -> String {
    if let Some(group) = own_group(agent) {
        return group.to_owned();
    }
    if !is_hub(agent) {
        let mut visited = HashSet::from([agent.id.as_str()]);
        let mut ancestor = parent_of(agent, by_id);
        while let Some(current) = ancestor {
            if !visited.insert(current.id.as_str()) {
                break;
            }
            if let Some(group) = own_group(current) {
                return group.to_owned();
            }
            if is_hub(current) {
                break;
            }
            ancestor = parent_of(current, by_id);
        }
    }
    agent.tags.first().unwrap_or(&agent.name).clone()
}

fn parent_of<'a>(agent: &Agent, by_id: &HashMap<&str, &'a Agent>) -> Option<&'a Agent> {
    by_id.get(agent.parent_id.as_ref()?.as_str()).copied()
}

fn last_activity(agent: &Agent) -> f64 {
    agent.last_activity_at.unwrap_or(agent.last_modified_at)
}

/// Folds records and messages into the graph at `now`, over the `window` seconds before it.
/// `context` reads one shown agent's context use.
pub fn build(
    agents: &[Agent],
    messages: &[Message],
    filter: &Filter,
    now: f64,
    window: f64,
    context: impl Fn(&Agent) -> Option<Context>,
) -> Graph {
    let since = now - window;
    let messages: Vec<&Message> = messages
        .iter()
        .filter(|message| since <= message.at && message.at <= now)
        .collect();
    let by_id: HashMap<&str, &Agent> = agents
        .iter()
        .filter(|agent| agent.created_at <= now)
        .map(|agent| (agent.id.as_str(), agent))
        .collect();
    let groups_by_id: HashMap<&str, String> = by_id
        .iter()
        .map(|(id, agent)| (*id, group_of(agent, &by_id)))
        .collect();
    let tagged = |agent: &Agent| {
        filter
            .tag
            .as_ref()
            .is_none_or(|tag| agent.tags.contains(tag))
    };
    let passes = |agent: &Agent| {
        tagged(agent)
            && filter
                .group
                .as_ref()
                .is_none_or(|group| &groups_by_id[agent.id.as_str()] == group)
    };
    let lively =
        |agent: &Agent| agent.state != LlmEngineState::Ended || last_activity(agent) >= since;

    let groups: Vec<String> = by_id
        .values()
        .filter(|agent| tagged(agent) && lively(agent))
        .map(|agent| groups_by_id[agent.id.as_str()].clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut shown: HashMap<&str, bool> = HashMap::new();
    for agent in by_id
        .values()
        .filter(|agent| passes(agent) && lively(agent))
    {
        shown.insert(agent.id.as_str(), false);
    }
    let base: Vec<&str> = shown.keys().copied().collect();
    for id in base {
        let mut ancestor = parent_of(by_id[id], &by_id);
        while let Some(current) = ancestor.filter(|current| passes(current)) {
            if shown.contains_key(current.id.as_str()) {
                break;
            }
            shown.insert(current.id.as_str(), true);
            ancestor = parent_of(current, &by_id);
        }
    }

    let mut ordered: Vec<&Agent> = shown.keys().map(|id| by_id[id]).collect();
    ordered.sort_by(|left, right| {
        left.created_at
            .total_cmp(&right.created_at)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    let operator_present = messages
        .iter()
        .any(|message| message.from.is_none() && shown.contains_key(message.to.as_str()));

    let mut nodes = Vec::new();
    if operator_present {
        nodes.push(Node {
            id: None,
            name: OPERATOR_NAME.to_owned(),
            state: NodeState::Operator,
            depth: 0,
            parent: None,
            parent_lost: false,
            connector: false,
            engine: None,
            definition: None,
            group: String::new(),
            tags: Vec::new(),
            created_at: f64::MIN,
            last_activity: None,
            context: None,
        });
    }
    let offset = nodes.len();
    let index_by_id: HashMap<&str, usize> = ordered
        .iter()
        .enumerate()
        .map(|(position, agent)| (agent.id.as_str(), offset + position))
        .collect();
    for agent in &ordered {
        let parent = agent
            .parent_id
            .as_ref()
            .and_then(|parent| index_by_id.get(parent.as_str()).copied());
        nodes.push(Node {
            id: Some(agent.id.clone()),
            name: agent.name.clone(),
            state: node_state(agent),
            depth: 0,
            parent,
            parent_lost: agent
                .parent_id
                .as_ref()
                .is_some_and(|parent| !by_id.contains_key(parent.as_str())),
            connector: shown[agent.id.as_str()],
            engine: Some(agent.engine),
            definition: agent.definition.as_ref().map(|choice| choice.agent.clone()),
            group: groups_by_id[agent.id.as_str()].clone(),
            tags: agent.tags.clone(),
            created_at: agent.created_at,
            last_activity: agent.last_activity_at,
            context: context(agent),
        });
    }
    for index in 0..nodes.len() {
        let mut depth = 0;
        let mut root = index;
        while let Some(parent) = nodes[root].parent.filter(|_| depth < nodes.len()) {
            depth += 1;
            root = parent;
        }
        // A lost parent still holds column 0: the "parent ended" stub is drawn there.
        nodes[index].depth = depth + usize::from(nodes[root].parent_lost);
    }

    let node_of = |id: Option<&AgentId>| match id {
        None => operator_present.then_some(0),
        Some(id) => index_by_id.get(id.as_str()).copied(),
    };
    let mut edges: Vec<Edge> = Vec::new();
    let mut edge_by_pair: HashMap<(usize, usize), usize> = HashMap::new();
    let mut edge_for = |edges: &mut Vec<Edge>, from: usize, to: usize| {
        let pair = (from.min(to), from.max(to));
        *edge_by_pair.entry(pair).or_insert_with(|| {
            let (left, right) = if (nodes[from].depth, from) <= (nodes[to].depth, to) {
                (from, to)
            } else {
                (to, from)
            };
            edges.push(Edge {
                from: left,
                to: right,
                lineage: false,
                forward: 0,
                backward: 0,
            });
            edges.len() - 1
        })
    };
    for (index, node) in nodes.iter().enumerate() {
        if let Some(parent) = node.parent {
            let edge = edge_for(&mut edges, parent, index);
            edges[edge].lineage = true;
        }
    }
    let mut traffic = Vec::new();
    for message in &messages {
        let (Some(from), Some(to)) = (node_of(message.from.as_ref()), node_of(Some(&message.to)))
        else {
            continue;
        };
        if from == to {
            continue;
        }
        let edge = edge_for(&mut edges, from, to);
        if edges[edge].from == from {
            edges[edge].forward += 1;
        } else {
            edges[edge].backward += 1;
        }
        traffic.push(Traffic {
            at: message.at,
            from,
            to,
        });
    }
    traffic.sort_by(|left, right| left.at.total_cmp(&right.at));

    Graph {
        nodes,
        edges,
        traffic,
        groups,
        now,
        window,
    }
}
