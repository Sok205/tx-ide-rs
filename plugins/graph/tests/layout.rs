mod world;

use ratatui::layout::Rect;
use tx_core::Agent;
use tx_graph::layout::{self, BOX_HEIGHT, BOX_WIDTH, COMPACT_HEIGHT, MARGIN, ROW_GAP};
use tx_graph::model::{self, Filter, Graph};
use world::{HOUR, graph, index};

fn overlaps(left: Rect, right: Rect) -> bool {
    left.intersects(right)
}

fn assert_no_overlap(boxes: &[Rect]) {
    for (first, left) in boxes.iter().enumerate() {
        for right in &boxes[first + 1..] {
            assert!(!overlaps(*left, *right), "{left:?} overlaps {right:?}");
        }
    }
}

#[test]
fn columns_are_depth_and_first_children_share_their_parents_row() {
    let graph = graph(&Filter::default(), HOUR);
    let placement = layout::place(&graph, 140);
    assert!(!placement.compact);
    let at = |name| placement.boxes[index(&graph, name)];
    assert_eq!(placement.column_x.len(), 4);
    assert_eq!(at("tx-assistant").x, MARGIN);
    assert_eq!(at("pv-orchestrator").x, placement.column_x[1]);
    assert_eq!(at("qa-pr4").x, placement.column_x[3]);
    let last_right = placement.column_x[3] + BOX_WIDTH;
    assert!(
        (140 - MARGIN - 3..=140 - MARGIN).contains(&last_right),
        "the last column ends at the margin, give or take the rounding: {last_right}"
    );
    assert_eq!(at("pv-orchestrator").y, at("tx-assistant").y);
    assert_eq!(at("pv-pr4").y, at("pv-orchestrator").y);
    assert_eq!(at("qa-pr4").y, at("pv-pr4").y);
    let slot = BOX_HEIGHT + ROW_GAP;
    assert_eq!(at("pv-pr5").y, at("pv-pr4").y + slot);
    assert_eq!(at("pv-pr6").y, at("pv-pr5").y + slot);
    // openbot-research is the assistant's second child: below the orchestrator's subtree in
    // column 1 (the orchestrator's own slot).
    assert_eq!(at("openbot-research").y, at("pv-orchestrator").y + slot);
    assert_no_overlap(&placement.boxes);
}

#[test]
fn trees_stack_below_everything_placed_before_them() {
    let graph = graph(&Filter::default(), HOUR);
    let placement = layout::place(&graph, 140);
    let at = |name| placement.boxes[index(&graph, name)];
    assert_eq!(at("you").y, 0);
    assert_eq!(at("old-lead").y, BOX_HEIGHT + ROW_GAP);
    let assistant_tree_bottom = ["pv-pr6", "openbot-research", "qa-pr4"]
        .iter()
        .map(|name| at(name).bottom())
        .max()
        .unwrap();
    assert_eq!(at("finished").y, assistant_tree_bottom + ROW_GAP);
    assert_eq!(at("stray").y, at("finished").bottom() + ROW_GAP);
    assert_eq!(placement.height, at("stray").bottom());
}

#[test]
fn narrow_screens_get_compact_boxes() {
    let graph = graph(&Filter::default(), HOUR);
    assert!(!layout::is_compact(
        3 * BOX_WIDTH + 2 * layout::COLUMN_GAP + 2 * MARGIN,
        4
    ));
    assert!(layout::is_compact(
        3 * BOX_WIDTH + 2 * layout::COLUMN_GAP + 2 * MARGIN - 1,
        4
    ));
    let placement = layout::place(&graph, 80);
    assert!(placement.compact);
    assert!(
        placement
            .boxes
            .iter()
            .all(|rect| rect.height == COMPACT_HEIGHT)
    );
    assert!(placement.boxes.iter().all(|rect| rect.right() <= 80));
    assert_no_overlap(&placement.boxes);
    // One column fits full boxes in less room.
    assert!(!layout::is_compact(BOX_WIDTH + 2 * MARGIN, 1));
}

/// An orchestrator with twenty workers: one tall column, nothing overlapping.
#[test]
fn a_wide_forest_stacks_one_tall_column() {
    let now = 1_000_000.0;
    let record = |number: usize, name: String, parent: Option<String>| {
        let parent = parent.map_or("null".to_owned(), |parent| format!("\"{parent}\""));
        Agent::from_json(&format!(
            r#"{{"version":2,"id":"{number:08x}-0000-4000-8000-000000000000","name":"{name}","engine":"claude","working_directory":"/tmp","roles":[],"state":"working","created_at":{},"last_modified_at":{now},"tags":[],"parent_id":{parent}}}"#,
            now - 1000.0 + number as f64
        ))
        .unwrap()
    };
    let lead = "00000000-0000-4000-8000-000000000000".to_owned();
    let mut agents = vec![record(0, "lead".into(), None)];
    agents.extend(
        (1..=20).map(|number| record(number, format!("worker-{number}"), Some(lead.clone()))),
    );
    let graph: Graph = model::build(&agents, &[], &Filter::default(), now, HOUR, |_| None);
    let placement = layout::place(&graph, 120);
    assert_eq!(placement.column_x.len(), 2);
    assert_eq!(placement.height, 20 * (BOX_HEIGHT + ROW_GAP) - ROW_GAP);
    assert_no_overlap(&placement.boxes);
}
