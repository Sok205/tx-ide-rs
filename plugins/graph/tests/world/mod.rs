//! The fixture world: `tests/fixtures/world/world.json` (records and `now`), its `log.jsonl`
//! and the transcripts its records name (relative to `transcripts/`).
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::Value;
use tx_core::Agent;
use tx_graph::model::{self, Context, Filter, Graph};

pub const HOUR: f64 = 3600.0;

pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/world")
}

fn world() -> Value {
    serde_json::from_str(&std::fs::read_to_string(dir().join("world.json")).unwrap()).unwrap()
}

pub fn now() -> f64 {
    world()["now"].as_f64().unwrap()
}

pub fn agents() -> Vec<Agent> {
    world()["agents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| Agent::from_json(&record.to_string()).unwrap())
        .collect()
}

pub fn context(agent: &Agent) -> Option<Context> {
    let path = agent
        .ext
        .get(agent.engine.as_str())?
        .get("transcript_path")?
        .as_str()?;
    model::context_of(&dir().join("transcripts").join(path))
}

pub fn graph(filter: &Filter, window: f64) -> Graph {
    let now = now();
    let messages = model::read_messages(&dir().join("log.jsonl"), now - window, now).unwrap();
    model::build(&agents(), &messages, filter, now, window, context)
}

pub fn names(graph: &Graph) -> Vec<&str> {
    graph.nodes.iter().map(|node| node.name.as_str()).collect()
}

pub fn index(graph: &Graph, name: &str) -> usize {
    graph
        .nodes
        .iter()
        .position(|node| node.name == name)
        .unwrap_or_else(|| panic!("no node {name}"))
}
