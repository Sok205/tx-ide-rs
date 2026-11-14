//! `tx layout save`: write a view's current window as a layout file, so it can be opened again
//! in any project at any terminal size.
//!
//! A tmux window is a guillotine arrangement: a region of more than one pane can always be cut
//! in two along a full-length border. [`walk`] cuts the window recursively and writes one record
//! per cut, for the far side's top-left pane, splitting the region's top-left pane by the far
//! side's share of the region, measured to the middle of its last cell (see [`divide`]). The file order is that pre-order walk, not tmux's pane indices,
//! so every `of` names an earlier pane. Shares are measured on the panes' own rectangles, which
//! is what `split-window -l N%` measures when `open` rebuilds them.
//!
//! A pane nesting an agent becomes `agent: <its definition>`, never `attach:`: a saved layout
//! starts its own agents wherever it is opened. No model or effort is written: a record's
//! options hold config.yaml's values too, and cannot tell them from a chosen one.

use std::path::Path;

use serde::Serialize;
use tx_client::tmux::Ui;
use tx_client::views::{self, WindowPane, default_shell};
use tx_core::tmux::TX_ID_OPTION;
use tx_core::{AgentId, TxCore, VerbError, VerbIo};

use crate::pane::{Placement, Split};

/// What a pane's foreground program is called when the pane is a shell, besides `$SHELL`.
const SHELLS: [&str; 8] = ["sh", "bash", "zsh", "fish", "dash", "ksh", "tcsh", "csh"];

pub struct SaveRequest<'a> {
    /// The view whose current window is saved; `None`: the caller's own session.
    pub view: Option<&'a str>,
    pub path: &'a Path,
    /// Overwrite an existing file.
    pub force: bool,
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

pub fn save(
    core: &TxCore,
    io: &mut VerbIo<'_>,
    request: &SaveRequest<'_>,
) -> Result<i32, VerbError> {
    let ui = Ui::from_core(core);
    let view = match request.view {
        Some(view) => view.to_owned(),
        None => ui.current_session_name().ok_or_else(|| {
            failed("not inside tx's tmux server: name the view to save with --view")
        })?,
    };
    if !ui.is_view(&view) {
        return Err(failed(format!(
            "'{view}' is not a view: name the view to save with --view"
        )));
    }
    if !request.force && request.path.exists() {
        return Err(failed(format!(
            "{} exists; --force overwrites it",
            request.path.display()
        )));
    }
    let panes = views::window_panes(&ui, &view).map_err(failed)?;
    let rectangles: Vec<Rectangle> = panes.iter().map(Rectangle::of).collect();
    let order = walk(&rectangles).map_err(|error| failed(format!("view {view}: {error}")))?;
    let mut records = Vec::with_capacity(order.len());
    for (index, placement) in order {
        let content = content(core, &ui, io, &panes[index])?;
        records.push(Record::new(content, placement));
    }
    let text = serde_norway::to_string(&File { panes: records }).map_err(failed)?;
    if let Some(directory) = request.path.parent() {
        std::fs::create_dir_all(directory)
            .map_err(|error| failed(format!("{}: {error}", directory.display())))?;
    }
    std::fs::write(request.path, text)
        .map_err(|error| failed(format!("{}: {error}", request.path.display())))?;
    writeln!(io.stdout, "{view} saved to {}", request.path.display()).map_err(failed)?;
    Ok(0)
}

/// What a saved pane shows.
#[derive(Debug, PartialEq, Eq)]
enum Content {
    /// The definition of the agent nested in it.
    Agent(String),
    Command(String),
    Shell,
}

fn content(
    core: &TxCore,
    ui: &Ui,
    io: &mut VerbIo<'_>,
    pane: &WindowPane,
) -> Result<Content, VerbError> {
    if let Some(inner) = ui.inner_for_pane(&pane.id) {
        let agent = ui
            .session_option(&inner, TX_ID_OPTION)
            .and_then(|id| id.parse::<AgentId>().ok())
            .map(|id| core.get_agent(&id))
            .transpose()?;
        let Some(definition) = agent.and_then(|agent| agent.definition) else {
            let _ = writeln!(
                io.stderr,
                "tx layout: pane {} nests {inner}, which runs no agent definition; saved as a shell",
                pane.id
            );
            return Ok(Content::Shell);
        };
        return Ok(Content::Agent(definition.agent));
    }
    if is_shell(&pane.command) {
        return Ok(Content::Shell);
    }
    Ok(Content::Command(pane.command.clone()))
}

/// A login shell shows as `-zsh`.
fn is_shell(command: &str) -> bool {
    let command = command.trim_start_matches('-');
    SHELLS.contains(&command)
        || Path::new(&default_shell())
            .file_name()
            .is_some_and(|name| name == command)
}

#[derive(Serialize)]
struct File {
    panes: Vec<Record>,
}

/// One pane as written: a shell is `{}`.
#[derive(Serialize)]
struct Record {
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    split: Option<Split>,
    #[serde(skip_serializing_if = "Option::is_none")]
    of: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<String>,
}

impl Record {
    fn new(content: Content, placement: Option<Placement>) -> Self {
        let (agent, command) = match content {
            Content::Agent(definition) => (Some(definition), None),
            Content::Command(command) => (None, Some(command)),
            Content::Shell => (None, None),
        };
        let (split, of, size) = match placement {
            Some(placement) => (
                Some(placement.split),
                Some(placement.of),
                Some(placement.size),
            ),
            None => (None, None, None),
        };
        Record {
            agent,
            command,
            split,
            of,
            size,
        }
    }
}

/// A pane's cells: `left` and `top` are its first column and row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rectangle {
    pub left: usize,
    pub top: usize,
    pub width: usize,
    pub height: usize,
}

impl Rectangle {
    fn of(pane: &WindowPane) -> Self {
        Rectangle {
            left: pane.left,
            top: pane.top,
            width: pane.width,
            height: pane.height,
        }
    }

    /// The first cell along the axis a `split` cuts: the column for `right`, the row for `below`.
    fn start(&self, split: Split) -> usize {
        match split {
            Split::Right => self.left,
            Split::Below => self.top,
        }
    }

    /// One past the last cell along that axis.
    fn end(&self, split: Split) -> usize {
        match split {
            Split::Right => self.left + self.width,
            Split::Below => self.top + self.height,
        }
    }
}

/// A region of panes no full-length border cuts in two; tmux does not make one.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("panes {0:?} are not cut in two by any full-length border")]
pub struct NoCut(pub Vec<usize>);

/// Every rectangle's index in file order, with its placement; the first has none.
pub fn walk(rectangles: &[Rectangle]) -> Result<Vec<(usize, Option<Placement>)>, NoCut> {
    let members: Vec<usize> = (0..rectangles.len()).collect();
    let mut order = vec![(top_left(rectangles, &members), None)];
    divide(rectangles, &members, 0, &mut order)?;
    Ok(order)
}

/// The region's pane at its top-left corner, which holds the region until it is split.
fn top_left(rectangles: &[Rectangle], members: &[usize]) -> usize {
    *members
        .iter()
        .min_by_key(|&&index| (rectangles[index].left, rectangles[index].top))
        .expect("a region holds at least one pane")
}

/// The region's extent along `split`'s axis, the borders between its panes included.
fn extent(rectangles: &[Rectangle], members: &[usize], split: Split) -> usize {
    let start = members
        .iter()
        .map(|&index| rectangles[index].start(split))
        .min()
        .expect("a region holds at least one pane");
    let end = members
        .iter()
        .map(|&index| rectangles[index].end(split))
        .max()
        .expect("a region holds at least one pane");
    end - start
}

/// Cuts the region `members`, whose top-left pane is file pane `anchor`, appending a record for
/// the far side and then everything inside each side.
fn divide(
    rectangles: &[Rectangle],
    members: &[usize],
    anchor: usize,
    order: &mut Vec<(usize, Option<Placement>)>,
) -> Result<(), NoCut> {
    if members.len() == 1 {
        return Ok(());
    }
    let (split, near, far) = cut(rectangles, members).ok_or_else(|| NoCut(members.to_vec()))?;
    let percent = percent(
        extent(rectangles, &far, split),
        extent(rectangles, members, split),
    );
    order.push((
        top_left(rectangles, &far),
        Some(Placement {
            split,
            of: anchor,
            size: format!("{percent}%"),
        }),
    ));
    let far_anchor = order.len() - 1;
    divide(rectangles, &near, anchor, order)?;
    divide(rectangles, &far, far_anchor, order)
}

/// `share` cells of `whole` as the `N%` that rebuilds them. tmux makes `-l N%` of a pane
/// floor(N% of it) cells, so the share came from any N in a band: the roundest N in it, a
/// multiple of 10, else of 5, else the one nearest its middle. Any N in the band rebuilds the
/// same cells at this size; the round ones are what a hand-written or tmux-halved split used,
/// so they come back unchanged when a rebuilt view is saved again, and the band is never wider
/// than one point from 50 cells up.
fn percent(share: usize, whole: usize) -> usize {
    let low = (100 * share).div_ceil(whole);
    let high = (100 * (share + 1)).div_ceil(whole) - 1;
    // `share + ½` cells, rounded half down.
    let middle = (200 * share + 100 + whole - 1) / (2 * whole);
    let chosen = if low > high {
        // Over 100 cells one point can be more than a cell, and no N gives exactly `share`.
        middle
    } else {
        [10, 5]
            .into_iter()
            .map(|step| low.div_ceil(step) * step)
            .find(|round| *round <= high)
            .unwrap_or(middle.clamp(low, high))
    };
    chosen.clamp(1, 99)
}

/// The first full-length cut of the region: a vertical one at the smallest column that works,
/// else a horizontal one. Every pane lies wholly before the border or wholly past it.
fn cut(rectangles: &[Rectangle], members: &[usize]) -> Option<(Split, Vec<usize>, Vec<usize>)> {
    [Split::Right, Split::Below].into_iter().find_map(|split| {
        let mut edges: Vec<usize> = members
            .iter()
            .map(|&index| rectangles[index].end(split))
            .collect();
        edges.sort_unstable();
        edges.dedup();
        edges.into_iter().find_map(|edge| {
            let (near, far): (Vec<usize>, Vec<usize>) = members
                .iter()
                .partition(|&&index| rectangles[index].end(split) <= edge);
            let clean = !far.is_empty()
                && far
                    .iter()
                    .all(|&index| rectangles[index].start(split) > edge);
            clean.then_some((split, near, far))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::{self, Pane};

    /// `left,top widthxheight`, as 740fc13a's listings print a pane.
    fn rectangles(listing: &[&str]) -> Vec<Rectangle> {
        listing
            .iter()
            .map(|pane| {
                let (corner, size) = pane.split_once(' ').unwrap();
                let (left, top) = corner.split_once(',').unwrap();
                let (width, height) = size.split_once('x').unwrap();
                Rectangle {
                    left: left.parse().unwrap(),
                    top: top.parse().unwrap(),
                    width: width.parse().unwrap(),
                    height: height.parse().unwrap(),
                }
            })
            .collect()
    }

    fn placed(split: Split, of: usize, size: &str) -> Option<Placement> {
        Some(Placement {
            split,
            of,
            size: size.to_owned(),
        })
    }

    #[test]
    fn one_pane_is_the_window() {
        assert_eq!(walk(&rectangles(&["0,0 200x50"])).unwrap(), [(0, None)]);
    }

    #[test]
    fn two_columns_are_one_split() {
        assert_eq!(
            walk(&rectangles(&["0,0 60x30", "61,0 59x30"])).unwrap(),
            [(0, None), (1, placed(Split::Right, 0, "50%"))]
        );
    }

    /// 740fc13a's capture: tmux indices 0 1 2 3 are %0 %2 %1 %3, because %2 split the
    /// top-left pane after %1 existed and tmux inserted it at index 1.
    const FOUR: [&str; 4] = ["0,0 70x34", "0,35 70x15", "71,0 129x39", "71,40 129x10"];

    #[test]
    fn the_four_pane_window_is_walked_in_pre_order_not_index_order() {
        assert_eq!(
            walk(&rectangles(&FOUR)).unwrap(),
            [
                (0, None),
                (2, placed(Split::Right, 0, "65%")),
                (1, placed(Split::Below, 0, "30%")),
                (3, placed(Split::Below, 1, "20%")),
            ]
        );
    }

    #[test]
    fn the_border_labels_move_only_the_top_row() {
        // The same window with `pane-border-status top`: the top-row panes start a row lower.
        // Each share is then of the pane `open` splits, which has lost that row too.
        let labelled = ["0,1 70x33", "0,35 70x15", "71,1 129x38", "71,40 129x10"];
        assert_eq!(
            walk(&rectangles(&labelled)).unwrap(),
            [
                (0, None),
                (2, placed(Split::Right, 0, "65%")),
                (1, placed(Split::Below, 0, "32%")),
                (3, placed(Split::Below, 1, "21%")),
            ]
        );
    }

    #[test]
    fn three_columns_take_two_vertical_cuts() {
        // `split-window -h` three times from one pane: 100, 49, 24 and 24 columns.
        assert_eq!(
            walk(&rectangles(&[
                "0,0 100x50",
                "101,0 49x50",
                "151,0 24x50",
                "176,0 24x50"
            ]))
            .unwrap(),
            [
                (0, None),
                (1, placed(Split::Right, 0, "50%")),
                (2, placed(Split::Right, 1, "50%")),
                (3, placed(Split::Right, 2, "50%")),
            ]
        );
    }

    #[test]
    fn an_aligned_grid_is_cut_vertically_first() {
        assert_eq!(
            walk(&rectangles(&[
                "0,0 60x15",
                "61,0 59x15",
                "0,16 60x14",
                "61,16 59x14"
            ]))
            .unwrap(),
            [
                (0, None),
                (1, placed(Split::Right, 0, "50%")),
                (2, placed(Split::Below, 0, "48%")),
                (3, placed(Split::Below, 1, "48%")),
            ]
        );
    }

    #[test]
    fn a_percentage_survives_a_rebuild_at_any_size() {
        for size in 1..100 {
            for whole in 4..300 {
                // What `split-window -l size%` makes of a pane `whole` cells long.
                let share = whole * size / 100;
                if share == 0 || share + 1 >= whole {
                    continue;
                }
                let saved = percent(share, whole);
                assert_eq!(whole * saved / 100, share, "{size}% of {whole}");
                if whole >= 50 {
                    assert!(saved.abs_diff(size) <= 1, "{size}% of {whole}: {saved}%");
                }
                if whole >= 10 && size % 10 == 0 {
                    assert_eq!(saved, size, "{size}% of {whole}");
                }
            }
        }
    }

    #[test]
    fn a_pinwheel_has_no_cut() {
        // Four panes around a centre one: no border runs the whole window.
        let pinwheel = rectangles(&[
            "0,0 25x10",
            "26,0 14x20",
            "15,21 25x9",
            "0,11 14x19",
            "15,11 10x9",
        ]);
        assert_eq!(walk(&pinwheel), Err(NoCut(vec![0, 1, 2, 3, 4])));
    }

    #[test]
    fn a_saved_file_reads_back_as_the_same_panes() {
        let order = walk(&rectangles(&FOUR)).unwrap();
        let contents = [
            Content::Agent("developer".to_owned()),
            Content::Command("nvim".to_owned()),
            Content::Shell,
            Content::Command("htop".to_owned()),
        ];
        let records = order
            .into_iter()
            .zip(contents)
            .map(|((_, placement), content)| Record::new(content, placement))
            .collect();
        let text = serde_norway::to_string(&File { panes: records }).unwrap();
        assert_eq!(
            text,
            "panes:\n\
             - agent: developer\n\
             - command: nvim\n  split: right\n  of: 0\n  size: 65%\n\
             - split: below\n  of: 0\n  size: 30%\n\
             - command: htop\n  split: below\n  of: 1\n  size: 20%\n"
        );
        let panes = pane::parse(Path::new("/c/layouts/mine.yaml"), &text).unwrap();
        assert_eq!(
            panes,
            [
                Pane {
                    content: pane::Content::Agent(pane::AgentPane {
                        definition: "developer".to_owned(),
                        ..pane::AgentPane::default()
                    }),
                    placement: None,
                },
                Pane {
                    content: pane::Content::Command(vec!["nvim".to_owned()]),
                    placement: placed(Split::Right, 0, "65%"),
                },
                Pane {
                    content: pane::Content::Shell,
                    placement: placed(Split::Below, 0, "30%"),
                },
                Pane {
                    content: pane::Content::Command(vec!["htop".to_owned()]),
                    placement: placed(Split::Below, 1, "20%"),
                },
            ]
        );
    }

    #[test]
    fn shells_are_recognised_by_name() {
        for shell in ["zsh", "-zsh", "bash", "sh", "fish"] {
            assert!(is_shell(shell), "{shell}");
        }
        for program in ["nvim", "tmux", "htop", "zshell"] {
            assert!(!is_shell(program), "{program}");
        }
    }
}
