mod world;

use std::io::Write;

use tx_graph::model::{self, Context, Filter, NodeState};
use world::{HOUR, graph, index, names};

#[test]
fn shows_running_recent_and_connecting_agents() {
    let graph = graph(&Filter::default(), HOUR);
    assert_eq!(
        names(&graph),
        [
            "you",
            "old-lead",
            "legacy-worker",
            "tx-assistant",
            "pv-orchestrator",
            "openbot-research",
            "pv-pr4",
            "pv-pr5",
            "pv-pr6",
            "qa-pr4",
            "finished",
            "stray",
        ]
    );
    let node = |name| &graph.nodes[index(&graph, name)];
    assert!(node("old-lead").connector);
    assert_eq!(node("old-lead").state, NodeState::Ended);
    assert!(!node("finished").connector);
    assert!(node("stray").parent_lost);
    assert_eq!(node("stray").depth, 1);
    assert_eq!(node("qa-pr4").depth, 3);
    assert_eq!(node("legacy-worker").depth, 1);
    assert_eq!(node("you").state, NodeState::Operator);
}

#[test]
fn groups_follow_overrides_up_the_chain_but_stop_at_the_hub() {
    let graph = graph(&Filter::default(), HOUR);
    let group = |name| graph.nodes[index(&graph, name)].group.as_str();
    assert_eq!(group("qa-pr4"), "persistent-views");
    assert_eq!(group("pv-orchestrator"), "persistent-views");
    assert_eq!(group("tx-assistant"), "tx-system");
    assert_eq!(group("openbot-research"), "openbot-research");
    assert_eq!(
        graph.groups,
        [
            "finished",
            "legacy",
            "openbot-research",
            "persistent-views",
            "stray",
            "tx-system"
        ]
    );
}

#[test]
fn edges_count_messages_both_ways_inside_the_window() {
    let graph = graph(&Filter::default(), HOUR);
    let edge = |from, to| {
        let (from, to) = (index(&graph, from), index(&graph, to));
        graph
            .edges
            .iter()
            .find(|edge| edge.from == from && edge.to == to)
            .unwrap_or_else(|| panic!("no edge {from} → {to}"))
    };
    let orchestrator_pr4 = edge("pv-orchestrator", "pv-pr4");
    assert!(orchestrator_pr4.lineage);
    assert_eq!(
        (orchestrator_pr4.forward, orchestrator_pr4.backward),
        (7, 5)
    );
    let operator = edge("you", "tx-assistant");
    assert!(!operator.lineage);
    assert_eq!((operator.forward, operator.backward), (3, 0));
    // The one operator message to the orchestrator before the window is not counted.
    assert_eq!(edge("you", "pv-orchestrator").forward, 1);
    // Siblings in one column: the earlier one is `from`.
    let siblings = edge("pv-pr4", "pv-pr5");
    assert!(!siblings.lineage);
    assert_eq!(siblings.forward, 2);
    let research = edge("tx-assistant", "openbot-research");
    assert_eq!((research.forward, research.backward), (0, 1));
    assert!(edge("old-lead", "legacy-worker").lineage);
    assert_eq!(graph.traffic.len(), 26);
    assert!(
        graph
            .traffic
            .windows(2)
            .all(|pair| pair[0].at <= pair[1].at)
    );
}

#[test]
fn context_is_known_with_or_without_a_window() {
    let graph = graph(&Filter::default(), HOUR);
    let context = |name| graph.nodes[index(&graph, name)].context;
    assert_eq!(
        context("pv-pr4"),
        Some(Context {
            tokens: 75005,
            window: None
        })
    );
    assert_eq!(context("pv-pr4").unwrap().ratio(), None);
    assert_eq!(context("pv-pr6").unwrap().ratio(), Some(0.83));
    assert_eq!(context("pv-pr5"), None);
}

#[test]
fn a_group_filter_keeps_its_own_ancestors_only() {
    let graph = graph(
        &Filter {
            tag: None,
            group: Some("persistent-views".into()),
        },
        HOUR,
    );
    assert_eq!(
        names(&graph),
        [
            "you",
            "pv-orchestrator",
            "pv-pr4",
            "pv-pr5",
            "pv-pr6",
            "qa-pr4"
        ]
    );
    let orchestrator = &graph.nodes[1];
    assert_eq!((orchestrator.depth, orchestrator.parent), (0, None));
    assert!(!orchestrator.parent_lost);
    // The tag filter still applies to the groups the filter cycles through.
    let legacy = world::graph(
        &Filter {
            tag: Some("legacy".into()),
            group: None,
        },
        HOUR,
    );
    assert_eq!(names(&legacy), ["old-lead", "legacy-worker"]);
    assert_eq!(legacy.groups, ["legacy"]);
    // A parent kept out by the filter is still named.
    assert_eq!(
        graph.nodes[1].hidden_parent.as_deref(),
        Some("tx-assistant")
    );
    assert_eq!(graph.nodes[2].hidden_parent, None);
}

#[test]
fn a_wider_window_shows_more() {
    let graph = graph(&Filter::default(), 24.0 * HOUR);
    assert!(names(&graph).contains(&"ancient"));
    assert!(!names(&graph).contains(&"future"));
}

#[test]
fn the_log_is_read_backwards_across_blocks() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log.jsonl");
    let mut file = std::fs::File::create(&path).unwrap();
    let id = "a0000000-0000-4000-8000-00000000000a";
    for second in 0..5000 {
        writeln!(
            file,
            r#"{{"ts":{second}.5,"actor":"","type":"message-sent","id":"{id}","name":"a","msg":"from user, padded to make the lines long enough"}}"#
        )
        .unwrap();
        writeln!(file, r#"{{"ts":{second}.6,"actor":"","type":"state-changed","id":"{id}","name":"a","msg":"idle → working"}}"#).unwrap();
    }
    writeln!(file, "not json").unwrap();
    let messages = model::read_messages(&path, 1000.0, 4000.0).unwrap();
    assert_eq!(messages.len(), 3000);
    assert_eq!(messages[0].at, 1000.5);
    assert_eq!(messages.last().unwrap().at, 3999.5);
    assert!(messages.iter().all(|message| message.from.is_none()));
    assert_eq!(model::read_messages(&path, 0.0, 1e12).unwrap().len(), 5000);
    let missing = directory.path().join("missing.jsonl");
    assert!(model::read_messages(&missing, 0.0, 1.0).unwrap().is_empty());
}
