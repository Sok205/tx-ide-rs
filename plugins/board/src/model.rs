//! The board: three columns of cards built from the issues and the agent records. Pure: no
//! `gh`, no tmux. An agent belongs to an issue through the tag `issue-<n>` (what `s` on a card
//! gives the agent it starts).

use serde::Serialize;
use serde_json::Value;
use tx_core::{Agent, CoreError, ListFilter, LlmEngineState, TxCore};

use crate::github::{Issue, Repository};

pub const TAG_PREFIX: &str = "issue-";

pub fn issue_tag(number: u64) -> String {
    format!("{TAG_PREFIX}{number}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColumnKind {
    Backlog,
    InProgress,
    Done,
}

impl ColumnKind {
    pub const ALL: [ColumnKind; 3] = [
        ColumnKind::Backlog,
        ColumnKind::InProgress,
        ColumnKind::Done,
    ];

    pub fn title(self) -> &'static str {
        match self {
            ColumnKind::Backlog => "Backlog",
            ColumnKind::InProgress => "In progress",
            ColumnKind::Done => "Done",
        }
    }
}

/// An agent on an issue, as the card shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CardAgent {
    pub name: String,
    pub state: LlmEngineState,
}

impl CardAgent {
    pub fn running(&self) -> bool {
        self.state != LlmEngineState::Ended
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Card {
    pub issue: Issue,
    /// Running agents first, then ended ones, each group newest first.
    pub agents: Vec<CardAgent>,
}

impl Card {
    /// The agent Enter / a / p show: the first running one.
    pub fn live_agent(&self) -> Option<&CardAgent> {
        self.agents.iter().find(|agent| agent.running())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Column {
    pub kind: ColumnKind,
    pub cards: Vec<Card>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Board {
    pub repository: Repository,
    pub columns: Vec<Column>,
}

impl Board {
    pub fn empty(repository: Repository) -> Self {
        Self::build(repository, Vec::new(), &[])
    }

    /// Open and unassigned: Backlog. Open with an assignee or a running agent: In progress.
    /// Closed: Done. The order of `issues` (as `gh` lists them, newest activity first) is kept.
    pub fn build(repository: Repository, issues: Vec<Issue>, agents: &[Agent]) -> Self {
        let mut columns: Vec<Column> = ColumnKind::ALL
            .into_iter()
            .map(|kind| Column {
                kind,
                cards: Vec::new(),
            })
            .collect();
        for issue in issues {
            let agents = agents_of(&issue, agents);
            let kind = if !issue.is_open() {
                ColumnKind::Done
            } else if !issue.assignees.is_empty() || agents.iter().any(CardAgent::running) {
                ColumnKind::InProgress
            } else {
                ColumnKind::Backlog
            };
            let column = columns
                .iter_mut()
                .find(|column| column.kind == kind)
                .expect("every kind has a column");
            column.cards.push(Card { issue, agents });
        }
        Self {
            repository,
            columns,
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("the board serialises")
    }
}

fn agents_of(issue: &Issue, agents: &[Agent]) -> Vec<CardAgent> {
    let tag = issue_tag(issue.number);
    let mut found: Vec<&Agent> = agents
        .iter()
        .filter(|agent| agent.tags.contains(&tag))
        .collect();
    found.sort_by(|left, right| {
        let running = |agent: &Agent| agent.state != LlmEngineState::Ended;
        running(right)
            .cmp(&running(left))
            .then(right.created_at.total_cmp(&left.created_at))
    });
    found
        .into_iter()
        .map(|agent| CardAgent {
            name: agent.name.clone(),
            state: agent.state,
        })
        .collect()
}

/// Every agent record, running or ended.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::User;

    fn issue(number: u64, open: bool, assignee: Option<&str>) -> Issue {
        Issue {
            number,
            title: format!("issue {number}"),
            state: if open { "OPEN" } else { "CLOSED" }.to_owned(),
            assignees: assignee
                .map(|login| {
                    vec![User {
                        login: login.to_owned(),
                    }]
                })
                .unwrap_or_default(),
            ..Issue::default()
        }
    }

    fn agent(name: &str, tags: &[&str], state: LlmEngineState, created_at: f64) -> Agent {
        let mut agent: Agent = serde_json::from_value(serde_json::json!({
            "id": format!("00000000-0000-0000-0000-{:012}", created_at as u64),
            "name": name,
            "engine": "claude",
            "working_directory": "/tmp",
            "roles": [],
            "state": "idle",
            "created_at": created_at,
            "last_modified_at": created_at,
            "tags": tags,
        }))
        .unwrap();
        agent.state = state;
        agent
    }

    #[test]
    fn issues_land_in_their_columns() {
        let repository = Repository::parse("o/n").unwrap();
        let agents = [
            agent("old", &["issue-3"], LlmEngineState::Ended, 1.0),
            agent("new", &["issue-3"], LlmEngineState::Working, 2.0),
            agent("done", &["issue-4"], LlmEngineState::Ended, 3.0),
        ];
        let board = Board::build(
            repository,
            vec![
                issue(1, true, None),
                issue(2, true, Some("me")),
                issue(3, true, None),
                issue(4, true, None),
                issue(5, false, Some("me")),
            ],
            &agents,
        );
        let numbers = |kind: ColumnKind| -> Vec<u64> {
            board.columns[kind as usize]
                .cards
                .iter()
                .map(|card| card.issue.number)
                .collect()
        };
        assert_eq!(numbers(ColumnKind::Backlog), [1, 4]);
        assert_eq!(numbers(ColumnKind::InProgress), [2, 3]);
        assert_eq!(numbers(ColumnKind::Done), [5]);
        let card = &board.columns[1].cards[1];
        assert_eq!(
            card.agents
                .iter()
                .map(|agent| &agent.name)
                .collect::<Vec<_>>(),
            ["new", "old"]
        );
        assert_eq!(card.live_agent().unwrap().name, "new");
        assert!(board.columns[0].cards[1].live_agent().is_none());
    }
}
