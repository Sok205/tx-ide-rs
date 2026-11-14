//! The view's state and input: the graph built from its source, the selected node, the
//! vertical scroll, the group filter `g` cycles, and the pulses of messages in flight.

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use tx_core::{Agent, AgentId};

use crate::layout::{self, Placement};
use crate::model::{self, Context, Filter, Graph, Message, NodeState};

const WHEEL_ROWS: u16 = 3;
/// The header's brand when `TX_APPNAME` is not set, as `tx` itself defaults.
pub const DEFAULT_BRAND: &str = "tx-ide";
/// Seconds a pulse takes to cross its edge.
pub const PULSE_TRAVEL: f64 = 1.1;
/// Seconds between two pulses that start in the same refresh on the same pair.
const PULSE_STAGGER: f64 = 0.3;

/// What the operator asked for; the event loop carries it out through `tx`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Show the agent's pane.
    Focus {
        name: String,
    },
    Send {
        id: AgentId,
        name: String,
        text: String,
    },
    /// `fork_name` blank: the `<source>-fork` default.
    Fork {
        id: AgentId,
        name: String,
        fork_name: Option<String>,
    },
    Stop {
        id: AgentId,
        name: String,
    },
    Respawn {
        id: AgentId,
        name: String,
    },
    Artifacts {
        id: AgentId,
        name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// A message to the selected agent.
    Send,
    /// The fork's name.
    Fork,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode {
    Browse,
    /// A one-line prompt in the footer.
    Prompt {
        purpose: Purpose,
        text: String,
    },
    /// `x` asked to stop the selected agent; `y` confirms.
    ConfirmStop,
    /// The key help page; any key closes it.
    Help,
}

/// A message in flight: drawn along its pair's edge from `started` (on [`App::clock`]) for
/// [`PULSE_TRAVEL`] seconds. `from` is `None` for the operator.
#[derive(Clone, Debug, PartialEq)]
pub struct Pulse {
    pub from: Option<AgentId>,
    pub to: AgentId,
    pub started: f64,
}

/// What the graph is built from: the records, the window's messages and each agent's context.
#[derive(Clone, Debug, Default)]
pub struct Source {
    pub agents: Vec<Agent>,
    pub messages: Vec<Message>,
    pub contexts: HashMap<AgentId, Context>,
}

pub struct App {
    pub source: Source,
    /// The flags' filter; `g` replaces its group.
    pub filter: Filter,
    pub now: f64,
    /// Seconds.
    pub window: f64,
    pub graph: Graph,
    /// Laid out for the canvas the last frame drew.
    pub placement: Placement,
    pub selected: usize,
    /// The canvas row at the top of the view.
    pub scroll: u16,
    /// The canvas area the last frame drew.
    pub canvas: Rect,
    /// Seconds since the view started: drives the breathing, the spinner and the pulses. Stays
    /// 0 in a snapshot.
    pub clock: f64,
    pub pulses: Vec<Pulse>,
    pub mode: Mode,
    /// What a key asked for, for the event loop to carry out.
    pub request: Option<Request>,
    /// One line for the operator, shown until the next key.
    pub notice: Option<String>,
    /// The application's name, at the start of the header.
    pub brand: String,
    pub quit: bool,
}

impl App {
    pub fn new(source: Source, filter: Filter, now: f64, window: f64) -> Self {
        let graph = build(&source, &filter, now, window);
        Self {
            source,
            filter,
            now,
            window,
            graph,
            placement: Placement::default(),
            selected: 0,
            scroll: 0,
            canvas: Rect::default(),
            clock: 0.0,
            pulses: Vec::new(),
            mode: Mode::Browse,
            request: None,
            notice: None,
            brand: DEFAULT_BRAND.to_owned(),
            quit: false,
        }
    }

    /// Rebuilds the graph, keeping the selected agent when it is still shown.
    pub fn rebuild(&mut self) {
        let selected = self
            .graph
            .nodes
            .get(self.selected)
            .map(|node| node.id.clone());
        self.graph = build(&self.source, &self.filter, self.now, self.window);
        self.selected = selected
            .and_then(|id| self.graph.nodes.iter().position(|node| node.id == id))
            .unwrap_or(0);
        self.placement = layout::place(&self.graph, self.canvas.width);
        let most = self.placement.height.saturating_sub(self.canvas.height);
        self.scroll = self.scroll.min(most);
        self.reveal_selected();
    }

    /// New messages from the log: added to the source and started as pulses. Messages that left
    /// the window are dropped.
    pub fn receive(&mut self, messages: Vec<Message>) {
        let mut started: HashMap<(Option<AgentId>, AgentId), f64> = HashMap::new();
        for message in &messages {
            let pair = (message.from.clone(), message.to.clone());
            let delay = started.entry(pair).or_insert(-PULSE_STAGGER);
            *delay += PULSE_STAGGER;
            self.pulses.push(Pulse {
                from: message.from.clone(),
                to: message.to.clone(),
                started: self.clock + *delay,
            });
        }
        self.source.messages.extend(messages);
        let since = self.now - self.window;
        self.source.messages.retain(|message| message.at >= since);
    }

    /// Drops the pulses that arrived; whether one is still in flight.
    pub fn animating(&mut self) -> bool {
        let clock = self.clock;
        self.pulses
            .retain(|pulse| clock - pulse.started <= PULSE_TRAVEL);
        !self.pulses.is_empty()
    }

    /// Lays the graph out for `canvas`; the renderer calls it before drawing.
    pub fn arrange(&mut self, canvas: Rect) {
        if canvas != self.canvas || self.placement.boxes.len() != self.graph.nodes.len() {
            self.canvas = canvas;
            self.placement = layout::place(&self.graph, canvas.width);
            self.reveal_selected();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        self.notice = None;
        match std::mem::replace(&mut self.mode, Mode::Browse) {
            Mode::Browse => self.browse(key.code),
            Mode::Prompt { purpose, text } => self.prompt(purpose, text, key.code),
            Mode::ConfirmStop => {
                if key.code == KeyCode::Char('y') {
                    self.request = self.agent_request(|id, name| Request::Stop { id, name });
                }
            }
            Mode::Help => {}
        }
    }

    fn browse(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Tab => self.cycle(1),
            KeyCode::BackTab => self.cycle(-1),
            KeyCode::Char('j') | KeyCode::Down => self.move_vertically(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_vertically(-1),
            KeyCode::Char('l') | KeyCode::Right => self.move_horizontally(1),
            KeyCode::Char('h') | KeyCode::Left => self.move_horizontally(-1),
            KeyCode::Char('g') => self.next_group(),
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Enter if self.running("jump to") => {
                let name = self.graph.nodes[self.selected].name.clone();
                self.request = Some(Request::Focus { name });
            }
            KeyCode::Char('s') if self.running("send to") => {
                self.mode = Mode::Prompt {
                    purpose: Purpose::Send,
                    text: String::new(),
                };
            }
            KeyCode::Char('x') if self.running("stop") => self.mode = Mode::ConfirmStop,
            KeyCode::Char('f') if self.agent("fork") => {
                self.mode = Mode::Prompt {
                    purpose: Purpose::Fork,
                    text: String::new(),
                };
            }
            KeyCode::Char('a') if self.agent("show artifacts of") => {
                self.request = self.agent_request(|id, name| Request::Artifacts { id, name });
            }
            KeyCode::Char('r') if self.agent("respawn") => {
                let node = &self.graph.nodes[self.selected];
                if node.state == NodeState::Ended {
                    self.request = self.agent_request(|id, name| Request::Respawn { id, name });
                } else {
                    self.notice = Some(format!("{} is running", node.name));
                }
            }
            _ => {}
        }
    }

    fn prompt(&mut self, purpose: Purpose, mut text: String, code: KeyCode) {
        match code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                let line = text.trim().to_owned();
                self.request = match purpose {
                    Purpose::Send if line.is_empty() => None,
                    Purpose::Send => self.agent_request(|id, name| Request::Send {
                        id,
                        name,
                        text: line,
                    }),
                    Purpose::Fork => self.agent_request(|id, name| Request::Fork {
                        id,
                        name,
                        fork_name: (!line.is_empty()).then_some(line),
                    }),
                };
            }
            KeyCode::Backspace => {
                text.pop();
                self.mode = Mode::Prompt { purpose, text };
            }
            KeyCode::Char(character) => {
                text.push(character);
                self.mode = Mode::Prompt { purpose, text };
            }
            _ => self.mode = Mode::Prompt { purpose, text },
        }
    }

    /// Whether the selection is an agent; otherwise says why `verb` cannot apply.
    fn agent(&mut self, verb: &str) -> bool {
        let node = &self.graph.nodes[self.selected];
        if node.id.is_none() {
            self.notice = Some(format!("cannot {verb} the operator"));
        }
        node.id.is_some()
    }

    /// Whether the selection is an agent that has not ended; otherwise says why.
    fn running(&mut self, verb: &str) -> bool {
        if !self.agent(verb) {
            return false;
        }
        let node = &self.graph.nodes[self.selected];
        if node.state == NodeState::Ended {
            self.notice = Some(format!("{} has ended · r respawns it", node.name));
            return false;
        }
        true
    }

    /// `make` applied to the selected agent; `None` for the operator.
    fn agent_request(&self, make: impl FnOnce(AgentId, String) -> Request) -> Option<Request> {
        let node = &self.graph.nodes[self.selected];
        Some(make(node.id.clone()?, node.name.clone()))
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollDown => self.scroll_by(i32::from(WHEEL_ROWS)),
            MouseEventKind::ScrollUp => self.scroll_by(-i32::from(WHEEL_ROWS)),
            _ => {}
        }
    }

    /// The group the filter shows, `None` for all.
    pub fn group(&self) -> Option<&str> {
        self.filter.group.as_deref()
    }

    /// all → each group → all.
    fn next_group(&mut self) {
        let groups = &self.graph.groups;
        let next = match &self.filter.group {
            None => groups.first().cloned(),
            Some(current) => groups
                .iter()
                .position(|group| group == current)
                .and_then(|position| groups.get(position + 1))
                .cloned(),
        };
        self.filter.group = next;
        self.scroll = 0;
        self.rebuild();
    }

    /// Nodes in reading order: column by column, top to bottom.
    fn reading_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.placement.boxes.len()).collect();
        order.sort_by_key(|index| {
            let rect = self.placement.boxes[*index];
            (rect.x, rect.y)
        });
        order
    }

    fn cycle(&mut self, step: isize) {
        let order = self.reading_order();
        let Some(position) = order.iter().position(|index| *index == self.selected) else {
            return;
        };
        let next = (position as isize + step).rem_euclid(order.len() as isize) as usize;
        self.select(order[next]);
    }

    /// The nearest node above or below in the same column.
    fn move_vertically(&mut self, step: i32) {
        let Some(current) = self.placement.boxes.get(self.selected).copied() else {
            return;
        };
        let candidates = self
            .placement
            .boxes
            .iter()
            .enumerate()
            .filter(|(_, rect)| rect.x == current.x)
            .filter(|(_, rect)| {
                if step > 0 {
                    rect.y > current.y
                } else {
                    rect.y < current.y
                }
            });
        let next = candidates.min_by_key(|(_, rect)| rect.y.abs_diff(current.y));
        match next {
            Some((index, _)) => self.select(index),
            // Past the column's last box the canvas still scrolls, but only while the
            // selected box stays in view: j and k never leave the column.
            None => {
                let rows = step * i32::from(current.height + layout::ROW_GAP);
                let most = self.placement.height.saturating_sub(self.canvas.height);
                let lowest = current.bottom().saturating_sub(self.canvas.height);
                let highest = current.y.min(most);
                let wanted = (i32::from(self.scroll) + rows).clamp(0, i32::from(most)) as u16;
                self.scroll = wanted.clamp(lowest.min(highest), highest);
            }
        }
    }

    /// The node in the next column whose row is nearest.
    fn move_horizontally(&mut self, step: isize) {
        let Some(current) = self.placement.boxes.get(self.selected).copied() else {
            return;
        };
        let depth = self.graph.nodes[self.selected].depth as isize + step;
        let next = self
            .graph
            .nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.depth as isize == depth)
            .min_by_key(|(index, _)| self.placement.boxes[*index].y.abs_diff(current.y));
        if let Some((index, _)) = next {
            self.select(index);
        }
    }

    fn select(&mut self, index: usize) {
        self.selected = index;
        self.reveal_selected();
    }

    fn reveal_selected(&mut self) {
        let Some(rect) = self.placement.boxes.get(self.selected) else {
            return;
        };
        let height = self.canvas.height;
        if rect.y < self.scroll {
            self.scroll = rect.y;
        } else if rect.bottom() > self.scroll + height {
            self.scroll = rect.bottom().saturating_sub(height);
        }
    }

    /// Scrolls the canvas, then moves the selection to the nearest box in view when it left it.
    fn scroll_by(&mut self, rows: i32) {
        let most = self.placement.height.saturating_sub(self.canvas.height);
        self.scroll = (i32::from(self.scroll) + rows).clamp(0, i32::from(most)) as u16;
        let (top, bottom) = (self.scroll, self.scroll + self.canvas.height);
        let visible = |rect: &Rect| rect.y >= top && rect.bottom() <= bottom;
        let Some(current) = self.placement.boxes.get(self.selected).copied() else {
            return;
        };
        if visible(&current) {
            return;
        }
        let nearest = self
            .placement
            .boxes
            .iter()
            .enumerate()
            .filter(|(_, rect)| visible(rect))
            .min_by_key(|(_, rect)| (rect.x.abs_diff(current.x), rect.y.abs_diff(current.y)));
        if let Some((index, _)) = nearest {
            self.selected = index;
        }
    }
}

fn build(source: &Source, filter: &Filter, now: f64, window: f64) -> Graph {
    model::build(
        &source.agents,
        &source.messages,
        filter,
        now,
        window,
        |agent| source.contexts.get(&agent.id).copied(),
    )
}
