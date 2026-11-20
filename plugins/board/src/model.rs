//! The board: three columns of cards built from the issues and the agent records, through a
//! query in GitHub's search language. Pure: no `gh`, no tmux. An agent belongs to an issue through the tag `issue-<n>`
//! (what `s` on a card gives the agent it starts).

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::Value;
use tx_core::{Agent, CoreError, ListFilter, LlmEngineState, TxCore};

use crate::github::{Issue, Repository};

pub const TAG_PREFIX: &str = "issue-";

pub fn issue_tag(number: u64) -> String {
    format!("{TAG_PREFIX}{number}")
}

/// A filter in GitHub's issue search language, applied to the issues the board holds:
/// `label:bug -label:wip assignee:sok no:assignee author:ada milestone:0.2 is:open "free text"`.
/// Every term must hold; a `-` in front of a term negates it. Free words match the title, the
/// body and the number; `in:`, `sort:` and `type:` are accepted and ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Query {
    pub text: String,
    terms: Vec<Term>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Term {
    negated: bool,
    kind: TermKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TermKind {
    Label(String),
    Assignee(String),
    Author(String),
    Milestone(String),
    NoAssignee,
    NoLabel,
    NoMilestone,
    Open,
    Closed,
    Word(String),
    Ignored,
}

impl Query {
    pub fn parse(text: &str) -> Self {
        let terms = tokens(text).into_iter().map(term).collect();
        Self {
            text: text.to_owned(),
            terms,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn matches(&self, issue: &Issue) -> bool {
        self.terms
            .iter()
            .all(|term| holds(&term.kind, issue) != term.negated)
    }
}

/// Whitespace-separated words, with double quotes keeping spaces inside a word (the quotes
/// themselves dropped): `label:"help wanted"` is one token `label:help wanted`.
fn tokens(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for character in text.chars() {
        match character {
            '"' => quoted = !quoted,
            character if character.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            character => word.push(character),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

fn term(token: String) -> Term {
    let (negated, token) = match token.strip_prefix('-') {
        Some(rest) if !rest.is_empty() => (true, rest.to_owned()),
        _ => (false, token),
    };
    let kind = match token.split_once(':') {
        Some((qualifier, value)) if !value.is_empty() => {
            let value = value.to_lowercase();
            match qualifier.to_lowercase().as_str() {
                "label" => TermKind::Label(value),
                "assignee" => TermKind::Assignee(value),
                "author" => TermKind::Author(value),
                "milestone" => TermKind::Milestone(value),
                "no" => match value.as_str() {
                    "assignee" => TermKind::NoAssignee,
                    "label" => TermKind::NoLabel,
                    "milestone" => TermKind::NoMilestone,
                    _ => TermKind::Word(token.to_lowercase()),
                },
                "is" | "state" => match value.as_str() {
                    "open" => TermKind::Open,
                    "closed" => TermKind::Closed,
                    "issue" => TermKind::Ignored,
                    _ => TermKind::Word(token.to_lowercase()),
                },
                "in" | "sort" | "type" | "repo" | "org" | "user" => TermKind::Ignored,
                _ => TermKind::Word(token.to_lowercase()),
            }
        }
        _ => TermKind::Word(token.to_lowercase()),
    };
    Term { negated, kind }
}

fn holds(kind: &TermKind, issue: &Issue) -> bool {
    let same = |left: &str, right: &str| left.eq_ignore_ascii_case(right);
    match kind {
        TermKind::Label(name) => issue.labels.iter().any(|label| same(&label.name, name)),
        TermKind::Assignee(login) => {
            (login == "@me" && !issue.assignees.is_empty())
                || issue.assignees.iter().any(|user| same(&user.login, login))
        }
        TermKind::Author(login) => same(&issue.author.login, login),
        TermKind::Milestone(title) => issue
            .milestone
            .as_ref()
            .is_some_and(|milestone| same(&milestone.title, title)),
        TermKind::NoAssignee => issue.assignees.is_empty(),
        TermKind::NoLabel => issue.labels.is_empty(),
        TermKind::NoMilestone => issue.milestone.is_none(),
        TermKind::Open => issue.is_open(),
        TermKind::Closed => !issue.is_open(),
        TermKind::Word(word) => {
            issue.title.to_lowercase().contains(word)
                || issue.body.to_lowercase().contains(word)
                || issue
                    .number
                    .to_string()
                    .starts_with(word.trim_start_matches('#'))
        }
        TermKind::Ignored => true,
    }
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
    /// How many issues the query left out.
    pub hidden: usize,
}

impl Board {
    pub fn empty(repository: Repository) -> Self {
        Self::build(repository, Vec::new(), &[], &Query::default())
    }

    /// Open and unassigned: Backlog. Open with an assignee or a running agent: In progress.
    /// Closed: Done. The order of `issues` (as `gh` lists them, newest activity first) is kept;
    /// the issues `query` does not match are counted and left out.
    pub fn build(
        repository: Repository,
        issues: Vec<Issue>,
        agents: &[Agent],
        query: &Query,
    ) -> Self {
        let mut columns: Vec<Column> = ColumnKind::ALL
            .into_iter()
            .map(|kind| Column {
                kind,
                cards: Vec::new(),
            })
            .collect();
        let mut hidden = 0;
        for issue in issues {
            if !query.matches(&issue) {
                hidden += 1;
                continue;
            }
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
            hidden,
        }
    }

    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("the board serialises")
    }
}

/// The logins assigned to any of `issues`, each once, sorted.
pub fn assignees_of(issues: &[Issue]) -> Vec<String> {
    let logins: BTreeSet<&str> = issues
        .iter()
        .flat_map(|issue| issue.assignees.iter().map(|user| user.login.as_str()))
        .collect();
    logins.into_iter().map(str::to_owned).collect()
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
    use crate::github::{Label, User};

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

    fn label(name: &str) -> Label {
        Label {
            name: name.to_owned(),
            color: String::new(),
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
            &Query::default(),
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
        assert_eq!(board.hidden, 0);
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

    #[test]
    fn a_query_speaks_github_search() {
        let mut bug = issue(7, true, Some("me"));
        bug.title = "tx send drops bytes".to_owned();
        bug.body = "long lines".to_owned();
        bug.labels = vec![label("bug"), label("help wanted")];
        bug.author = User {
            login: "ada".to_owned(),
        };
        bug.milestone = Some(crate::github::Milestone {
            title: "0.2".to_owned(),
        });
        let plain = issue(8, false, None);
        let holds = |text: &str, issue: &Issue| Query::parse(text).matches(issue);
        assert!(Query::parse("  ").is_empty());
        assert!(holds("", &bug) && holds("", &plain));
        assert!(holds("label:bug", &bug) && !holds("label:bug", &plain));
        assert!(holds("label:\"help wanted\"", &bug));
        assert!(holds("-label:bug", &plain) && !holds("-label:bug", &bug));
        assert!(holds("label:BUG assignee:me", &bug));
        assert!(holds("assignee:@me", &bug) && !holds("assignee:@me", &plain));
        assert!(holds("no:assignee", &plain) && !holds("no:assignee", &bug));
        assert!(holds("no:label no:milestone", &plain));
        assert!(holds("author:ada milestone:0.2", &bug));
        assert!(holds("is:open", &bug) && !holds("is:open", &plain));
        assert!(holds("state:closed", &plain) && holds("-is:open", &plain));
        assert!(holds("send", &bug) && holds("LINES", &bug) && !holds("send", &plain));
        assert!(holds("\"drops bytes\"", &bug) && !holds("\"bytes drops\"", &bug));
        assert!(holds("#8", &plain) && holds("8", &plain) && !holds("#8", &bug));
        assert!(holds(
            "in:title sort:updated type:issue is:issue send",
            &bug
        ));
        assert!(holds("weird:thing", &bug) == bug.title.contains("weird:thing"));
        let board = Board::build(
            Repository::parse("o/n").unwrap(),
            vec![bug, plain],
            &[],
            &Query::parse("label:bug is:closed"),
        );
        assert_eq!(board.hidden, 2);
    }

    #[test]
    fn the_assignees_of_the_issues_are_listed_once() {
        let issues = [
            issue(1, true, Some("zed")),
            issue(2, true, Some("amy")),
            issue(3, false, Some("zed")),
            issue(4, true, None),
        ];
        assert_eq!(assignees_of(&issues), ["amy", "zed"]);
    }
}
