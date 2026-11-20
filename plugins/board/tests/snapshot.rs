//! One frame of the board over a fixture of issues and two agent records, against the expected
//! text. Set `TX_BOARD_WRITE_SNAPSHOT=1` to rewrite the expectation after a deliberate change.

use std::path::Path;

use tx_board::github::{Repository, parse_issues};
use tx_board::model::Board;
use tx_board::run::snapshot;
use tx_core::{Agent, LlmEngineState};

/// Epoch seconds of 2026-11-20T17:00:00Z.
const NOW: f64 = 1_795_194_000.0;

fn agent(name: &str, tag: &str, state: LlmEngineState) -> Agent {
    let mut agent: Agent = serde_json::from_value(serde_json::json!({
        "id": "00000000-0000-0000-0000-000000000001",
        "name": name,
        "engine": "claude",
        "working_directory": "/tmp",
        "roles": [],
        "state": "idle",
        "created_at": 1.0,
        "last_modified_at": 1.0,
        "tags": [tag],
    }))
    .unwrap();
    agent.state = state;
    agent
}

#[test]
fn a_frame_over_the_fixture() {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let issues =
        parse_issues(&std::fs::read_to_string(fixtures.join("issues.json")).unwrap()).unwrap();
    let agents = [
        agent("issue-116", "issue-116", LlmEngineState::Working),
        agent("fixer", "issue-108", LlmEngineState::Ended),
    ];
    let board = Board::build(
        Repository::parse("tx-devs/tx-ide").unwrap(),
        issues,
        &agents,
    );
    let text = snapshot(board, NOW, NOW - 12.0, 116, 32);
    let expected = fixtures.join("snapshot-116x32.txt");
    if std::env::var_os("TX_BOARD_WRITE_SNAPSHOT").is_some() {
        std::fs::write(&expected, format!("{text}\n")).unwrap();
    }
    let wanted = std::fs::read_to_string(&expected).unwrap_or_default();
    assert_eq!(text.trim_end(), wanted.trim_end(), "\n{text}");
}
