//! Where each node's box goes on the canvas. Columns are lineage depth, spread over the width.
//! Rows follow the tree: a first child sits on its parent's row and later siblings stack below,
//! so a lineage edge mostly runs along its row. Trees (and the operator) stack vertically, each
//! starting below everything placed before it. The canvas is as tall as it needs: the view
//! scrolls it.

use ratatui::layout::Rect;

use crate::model::Graph;

/// A full box: a heavy border around the state line and the context line.
pub const BOX_WIDTH: u16 = 24;
pub const BOX_HEIGHT: u16 = 4;
/// The compact form: one borderless line, `glyph name`.
pub const COMPACT_WIDTH: u16 = 18;
pub const COMPACT_HEIGHT: u16 = 1;
/// Room between two columns for the curve, its count badge and the arrowhead.
pub const COLUMN_GAP: u16 = 10;
pub const ROW_GAP: u16 = 1;
/// Cells left free at the canvas's left and right edges.
pub const MARGIN: u16 = 2;
/// Full boxes need room for this many columns; below it every box is compact.
const FULL_COLUMNS: u16 = 3;
const NARROWEST_BOX: u16 = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// One per node, in canvas coordinates: x from the canvas's left, y from its top (before
    /// scrolling).
    pub boxes: Vec<Rect>,
    pub compact: bool,
    /// Rows the canvas needs.
    pub height: u16,
    /// Each column's left edge.
    pub column_x: Vec<u16>,
}

/// Whether `width` is too narrow for full boxes.
pub fn is_compact(width: u16, columns: usize) -> bool {
    let columns = (columns as u16).clamp(1, FULL_COLUMNS);
    width < columns * BOX_WIDTH + (columns - 1) * COLUMN_GAP + 2 * MARGIN
}

pub fn place(graph: &Graph, width: u16) -> Placement {
    let columns = graph.columns().max(1);
    let compact = is_compact(width, columns);
    let (mut box_width, box_height) = if compact {
        (COMPACT_WIDTH, COMPACT_HEIGHT)
    } else {
        (BOX_WIDTH, BOX_HEIGHT)
    };
    let usable = width.saturating_sub(2 * MARGIN);
    let column_x: Vec<u16> = if columns == 1 {
        box_width = box_width.min(usable.max(NARROWEST_BOX));
        vec![MARGIN]
    } else {
        let step = usable.saturating_sub(box_width) / (columns as u16 - 1);
        let room = step.saturating_sub(COLUMN_GAP / 2);
        if room < box_width {
            box_width = room.max(NARROWEST_BOX);
        }
        let step = usable.saturating_sub(box_width) / (columns as u16 - 1);
        (0..columns as u16)
            .map(|column| MARGIN + column * step)
            .collect()
    };

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); graph.nodes.len()];
    let mut roots = Vec::new();
    for (index, node) in graph.nodes.iter().enumerate() {
        match node.parent {
            Some(parent) => children[parent].push(index),
            None => roots.push(index),
        }
    }

    let slot = box_height + ROW_GAP;
    let mut column_bottom = vec![0u16; columns];
    let mut rows = vec![0u16; graph.nodes.len()];
    for root in roots {
        let top = column_bottom.iter().copied().max().unwrap_or(0);
        // Depth-first, with an explicit stack: (node, the row it would like).
        let mut stack = vec![(root, top)];
        while let Some((index, wanted)) = stack.pop() {
            let depth = graph.nodes[index].depth;
            let row = wanted.max(column_bottom[depth]);
            rows[index] = row;
            column_bottom[depth] = row + slot;
            for child in children[index].iter().rev() {
                stack.push((*child, row));
            }
        }
    }

    let boxes = graph
        .nodes
        .iter()
        .zip(&rows)
        .map(|(node, row)| Rect::new(column_x[node.depth], *row, box_width, box_height))
        .collect();
    Placement {
        boxes,
        compact,
        height: column_bottom
            .iter()
            .copied()
            .max()
            .unwrap_or(0)
            .saturating_sub(ROW_GAP),
        column_x,
    }
}
