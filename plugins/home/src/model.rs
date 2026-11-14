//! The home's state machine: the session manager's rows (projects → agents → views, unfolded
//! details, folded ended agents), the cursor, the find / ask inputs, the stop confirm, and what
//! a key does. Pure: [`Context`] is gathered by the data layer, [`Home::key`] turns a key into
//! an [`Outcome`], nothing here touches tmux or the store.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use tx_core::LlmEngineState;

/// A repository's main checkout: its directory name and path.
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentRow {
    pub id: String,
    pub name: String,
    /// The first tag, or empty.
    pub tag: String,
    pub state: LlmEngineState,
    /// Last activity, else creation, epoch seconds.
    pub age_at: f64,
    pub branch: Option<String>,
    pub chats: usize,
    /// Index into [`Context::projects`].
    pub project: usize,
    pub is_assistant: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ViewRow {
    pub name: String,
    /// The id of the agent the view belongs to.
    pub parent: Option<String>,
    /// Index into [`Context::projects`].
    pub project: Option<usize>,
}

/// What an unfolded agent row shows; gathered lazily, see [`Home::needs_detail`].
#[derive(Clone, Debug, PartialEq)]
pub struct Detail {
    pub roles: Vec<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub parent_name: Option<String>,
    pub last_user: Option<String>,
    pub last_agent: Option<String>,
    /// Artifact counts by kind.
    pub artifacts: Vec<(String, usize)>,
    pub started_at: f64,
    pub ended_at: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AssistantState {
    #[default]
    Absent,
    Creating,
    Starting,
    Ready,
    Stopped {
        id: String,
    },
    Failed(String),
}

/// One line of the cheat sheet: a fragment's `# keys:` and its first prose comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyHint {
    pub plugin: String,
    pub keys: Vec<String>,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Context {
    pub projects: Vec<Project>,
    pub agents: Vec<AgentRow>,
    pub views: Vec<ViewRow>,
    /// By agent id, filled lazily.
    pub details: BTreeMap<String, Detail>,
    pub assistant: AssistantState,
    /// Every plugin with whether it is active, manifest order.
    pub plugins: Vec<(String, bool)>,
    pub keys: Vec<KeyHint>,
    /// Ages are drawn against this, epoch seconds.
    pub now: f64,
    /// Where `n` starts an agent when the cursor is under no project.
    pub home_directory: PathBuf,
    /// The names `tx layout ls` lists; `None` when the `layout` verb is absent, which hides `w`.
    pub layouts: Option<Vec<String>>,
}

/// One line of the main list.
#[derive(Clone, Debug, PartialEq)]
pub enum Row<'a> {
    Project(&'a Project),
    Agent(&'a AgentRow),
    Detail(&'a AgentRow, Option<&'a Detail>),
    Views(Vec<&'a ViewRow>),
    EndedFold {
        project: usize,
        hidden: usize,
    },
    FirstRun,
    /// The only row until the first context arrived.
    Loading,
}

impl Row<'_> {
    fn is_selectable(&self) -> bool {
        matches!(self, Row::Agent(_) | Row::Views(_))
    }

    /// The session Enter shows: the agent, or a views row's first view.
    fn session_name(&self) -> Option<&str> {
        match self {
            Row::Agent(agent) => Some(&agent.name),
            Row::Views(views) => Some(&views[0].name),
            _ => None,
        }
    }

    fn holds(&self, name: &str) -> bool {
        match self {
            Row::Agent(agent) => agent.name == name,
            Row::Views(views) => views.iter().any(|view| view.name == name),
            _ => false,
        }
    }
}

/// What the home runs once the TUI has exited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Show the session with this name.
    Attach(String),
    /// Respawn the ended agent with this id.
    Resume(String),
    /// The new-agent form, its directory preset.
    New(PathBuf),
    /// Stop the agent with this id.
    Stop(String),
    /// Send the line to the assistant.
    Ask(String),
    /// The artifacts of the agent with this id.
    Artifacts(String),
    Timeline,
    Tutor,
    /// Start the assistant again after a failed warm.
    Warm,
    /// Open the layout `name` in `project`, around the agent with this id when there is one.
    Layout {
        name: String,
        agent: Option<String>,
        project: PathBuf,
    },
}

impl Action {
    /// One line, the choice file's format.
    pub fn encode(&self) -> String {
        match self {
            Action::Attach(name) => format!("attach {name}"),
            Action::Resume(id) => format!("resume {id}"),
            Action::New(path) => format!("new {}", path.display()),
            Action::Stop(id) => format!("stop {id}"),
            Action::Ask(text) => format!("ask {text}"),
            Action::Artifacts(id) => format!("artifacts {id}"),
            Action::Timeline => "timeline".into(),
            Action::Tutor => "tutor".into(),
            Action::Warm => "warm".into(),
            // The path last: it may hold spaces, a layout name and an agent id never do.
            Action::Layout {
                name,
                agent,
                project,
            } => format!(
                "layout {name} {} {}",
                agent.as_deref().unwrap_or("-"),
                project.display()
            ),
        }
    }

    pub fn decode(text: &str) -> Option<Self> {
        let line = text.trim();
        let (verb, argument) = line.split_once(' ').unwrap_or((line, ""));
        let argument = argument.trim().to_owned();
        Some(match (verb, argument.is_empty()) {
            ("timeline", true) => Action::Timeline,
            ("tutor", true) => Action::Tutor,
            ("warm", true) => Action::Warm,
            ("attach", false) => Action::Attach(argument),
            ("resume", false) => Action::Resume(argument),
            ("new", false) => Action::New(PathBuf::from(argument)),
            ("stop", false) => Action::Stop(argument),
            ("ask", false) => Action::Ask(argument),
            ("artifacts", false) => Action::Artifacts(argument),
            ("layout", false) => {
                let (name, rest) = argument.split_once(' ')?;
                let (agent, project) = rest.split_once(' ')?;
                Action::Layout {
                    name: name.to_owned(),
                    agent: (agent != "-").then(|| agent.to_owned()),
                    project: PathBuf::from(project),
                }
            }
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Main,
    /// The read-only plugin list, scrolled to this row.
    Plugins(usize),
    /// The cheat sheet, scrolled to this row.
    Keys(usize),
    /// The layouts `tx layout ls` lists, this row selected.
    Layouts(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    Run(Action),
    NextTheme,
}

/// A question on the footer line, answered by the next key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Confirm {
    /// Stop the agent with this id.
    Stop(String),
}

/// The one-line input in the footer's place.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    /// Typing edits [`Home`]'s filter.
    Find,
    Ask(String),
}

/// How many ended agents a project shows before the rest fold into one row.
pub const ENDED_SHOWN: usize = 3;

const MAIN_FOOTER: &str = "enter attach · →/← unfold · n new here · / ask · f find · ? keys";
const ENDED_FOOTER: &str = "enter resume · →/← unfold · n new here · / ask · f find · ? keys";
const FIRST_RUN_FOOTER: &str = "enter attach · n new · / ask · L tour · ? keys";
const SYSTEM_TAG: &str = "tx-system";

pub struct Home {
    pub context: Context,
    /// A context arrived: until then the list says it is loading, and is no first run.
    loaded: bool,
    /// Index into [`Home::rows`].
    cursor: usize,
    /// Agent ids whose detail row shows.
    unfolded: BTreeSet<String>,
    ended_shown: bool,
    /// The find text; empty keeps every row.
    filter: String,
    view: View,
    confirm: Option<Confirm>,
    input: Option<Input>,
    /// Opened over the IDE (prefix+m), so `q` and Esc close it.
    popup: bool,
    /// The trailing pseudo-project of the views under no project.
    loose_views: Project,
    /// A backdrop catalogue is installed: `b` cycles themes.
    themed: bool,
    notice: Option<(String, Instant)>,
}

/// How long a notice replaces the footer.
const NOTICE: Duration = Duration::from_secs(2);

impl Home {
    /// The floor: the home pane's own TUI, which never closes on `q`.
    pub fn new(context: Context) -> Self {
        let mut home = Home {
            context,
            loaded: true,
            cursor: 0,
            unfolded: BTreeSet::new(),
            ended_shown: false,
            filter: String::new(),
            view: View::Main,
            confirm: None,
            input: None,
            popup: false,
            loose_views: Project {
                name: "views".into(),
                path: PathBuf::new(),
            },
            themed: false,
            notice: None,
        };
        home.cursor = home.anchor(None);
        home
    }

    pub fn new_popup(context: Context) -> Self {
        Home {
            popup: true,
            ..Home::new(context)
        }
    }

    /// The floor (or the popup) before its first context.
    pub fn loading(popup: bool) -> Self {
        Home {
            loaded: false,
            popup,
            ..Home::new(Context::default())
        }
    }

    pub fn view(&self) -> View {
        self.view
    }

    pub fn rows(&self) -> Vec<Row<'_>> {
        if !self.loaded {
            return vec![Row::Loading];
        }
        let mut rows = Vec::new();
        for project in self.project_order() {
            let mut agents: Vec<&AgentRow> = self
                .context
                .agents
                .iter()
                .filter(|agent| agent.project == project && self.keeps_agent(agent))
                .collect();
            agents.sort_by(|a, b| {
                is_ended(a)
                    .cmp(&is_ended(b))
                    .then(b.age_at.total_cmp(&a.age_at))
            });
            let loose = self.loose_views(Some(project));
            if agents.is_empty() && loose.is_empty() {
                continue;
            }
            rows.push(Row::Project(&self.context.projects[project]));
            let mut ended = 0;
            for agent in agents {
                if is_ended(agent) {
                    ended += 1;
                    if ended > ENDED_SHOWN && !self.ended_shown {
                        continue;
                    }
                }
                rows.push(Row::Agent(agent));
                if self.unfolded.contains(&agent.id) {
                    rows.push(Row::Detail(agent, self.context.details.get(&agent.id)));
                }
                let views: Vec<&ViewRow> = self
                    .context
                    .views
                    .iter()
                    .filter(|view| view.parent.as_ref() == Some(&agent.id))
                    .collect();
                if !views.is_empty() {
                    rows.push(Row::Views(views));
                }
            }
            if ended > ENDED_SHOWN && !self.ended_shown {
                rows.push(Row::EndedFold {
                    project,
                    hidden: ended - ENDED_SHOWN,
                });
            }
            if !loose.is_empty() {
                rows.push(Row::Views(loose));
            }
        }
        let loose = self.loose_views(None);
        if !loose.is_empty() {
            rows.push(Row::Project(&self.loose_views));
            rows.push(Row::Views(loose));
        }
        if self.is_first_run() {
            rows.push(Row::FirstRun);
        }
        rows
    }

    pub fn cursor(&self) -> Option<usize> {
        self.rows()
            .get(self.cursor)
            .is_some_and(Row::is_selectable)
            .then_some(self.cursor)
    }

    /// Moves the cursor to the row of session `name`, when it is listed.
    pub fn select_session(&mut self, name: &str) {
        if let Some(index) = self
            .rows()
            .iter()
            .position(|row| row.is_selectable() && row.holds(name))
        {
            self.cursor = index;
        }
    }

    pub fn is_first_run(&self) -> bool {
        self.loaded
            && self.context.agents.iter().all(|agent| agent.is_assistant)
            && self.context.views.is_empty()
    }

    /// A backdrop catalogue is installed: `b` cycles themes.
    pub fn set_themed(&mut self, themed: bool) {
        self.themed = themed;
    }

    /// Shown in place of the footer for [`NOTICE`] from `now`.
    pub fn notify(&mut self, text: String, now: Instant) {
        self.notice = Some((text, now));
    }

    /// Drops a notice that has run its time; whether one was dropped.
    pub fn expire_notice(&mut self, now: Instant) -> bool {
        let expired = self
            .notice
            .as_ref()
            .is_some_and(|(_, shown)| now.duration_since(*shown) >= NOTICE);
        if expired {
            self.notice = None;
        }
        expired
    }

    pub fn footer(&self) -> String {
        if let Some(Confirm::Stop(id)) = &self.confirm {
            let name = self
                .context
                .agents
                .iter()
                .find(|agent| &agent.id == id)
                .map_or(id.as_str(), |agent| agent.name.as_str());
            return format!("stop {name}? y/n");
        }
        match &self.input {
            Some(Input::Find) => return format!("find › {}", self.filter),
            Some(Input::Ask(text)) => return format!("ask › {text}"),
            None => {}
        }
        if let Some((text, _)) = &self.notice {
            return text.clone();
        }
        let hint = if self.is_first_run() {
            FIRST_RUN_FOOTER
        } else if self.cursor_agent().is_some_and(is_ended) {
            ENDED_FOOTER
        } else {
            MAIN_FOOTER
        };
        let close = if self.popup { " · q close" } else { "" };
        // Last, so a narrow popup cuts it rather than `q close`.
        let theme = if self.themed { " · b theme" } else { "" };
        let layouts = if self.context.layouts.is_some() {
            " · w layouts"
        } else {
            ""
        };
        format!("{hint}{close}{theme}{layouts}")
    }

    /// The first unfolded agent whose [`Detail`] the context lacks.
    pub fn needs_detail(&self) -> Option<String> {
        self.unfolded
            .iter()
            .find(|id| {
                !self.context.details.contains_key(*id)
                    && self.context.agents.iter().any(|agent| &agent.id == *id)
            })
            .cloned()
    }

    /// New data from the poll; the cursor stays on its session, else the nearest row.
    pub fn replace_context(&mut self, context: Context) {
        self.reshape(|home| {
            home.context = context;
            home.loaded = true;
        });
    }

    pub fn key(&mut self, code: KeyCode) -> Outcome {
        match self.view {
            View::Plugins(row) => {
                self.view = View::Plugins(scroll(row, self.context.plugins.len(), code));
                return self.back_on(code);
            }
            View::Keys(row) => {
                self.view = View::Keys(scroll(row, self.context.keys.len(), code));
                return self.back_on(code);
            }
            View::Layouts(row) => {
                if code == KeyCode::Enter {
                    let outcome = self.open_layout(row);
                    if outcome != Outcome::Stay {
                        self.view = View::Main;
                    }
                    return outcome;
                }
                let count = self.context.layouts.as_ref().map_or(0, Vec::len);
                self.view = View::Layouts(scroll(row, count, code));
                return self.back_on(code);
            }
            View::Main => {}
        }
        if let Some(Confirm::Stop(id)) = self.confirm.take() {
            return match code {
                KeyCode::Char('y') => Outcome::Run(Action::Stop(id)),
                _ => Outcome::Stay,
            };
        }
        match self.input.take() {
            Some(Input::Find) => self.find_key(code),
            Some(Input::Ask(text)) => self.ask_key(text, code),
            None => self.main_key(code),
        }
    }

    fn main_key(&mut self, code: KeyCode) -> Outcome {
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.step(true),
            KeyCode::Up | KeyCode::Char('k') => self.step(false),
            KeyCode::Enter => return self.enter(),
            KeyCode::Right | KeyCode::Char('l') => self.unfold(true),
            KeyCode::Left | KeyCode::Char('h') => self.unfold(false),
            KeyCode::Tab => self.reshape(|home| home.ended_shown = !home.ended_shown),
            KeyCode::Char('n') => return Outcome::Run(Action::New(self.cursor_project_path())),
            KeyCode::Char('x') => {
                if let Some(agent) = self.cursor_agent().filter(|agent| !is_ended(agent)) {
                    self.confirm = Some(Confirm::Stop(agent.id.clone()));
                }
            }
            KeyCode::Char('r') => return self.resume(),
            KeyCode::Char('a') => {
                if let Some(agent) = self.cursor_agent() {
                    return Outcome::Run(Action::Artifacts(agent.id.clone()));
                }
            }
            KeyCode::Char('b') if self.themed => return Outcome::NextTheme,
            KeyCode::Char('w') if self.context.layouts.is_some() => self.view = View::Layouts(0),
            KeyCode::Char('t' | 'T') => return Outcome::Run(Action::Timeline),
            KeyCode::Char('L') => return Outcome::Run(Action::Tutor),
            KeyCode::Char('?' | 'K') => self.view = View::Keys(0),
            KeyCode::Char('p') => self.view = View::Plugins(0),
            KeyCode::Char('f') => self.input = Some(Input::Find),
            KeyCode::Char('/') => self.input = Some(Input::Ask(String::new())),
            KeyCode::Esc | KeyCode::Char('q') if self.popup => return Outcome::Close,
            _ => {}
        }
        Outcome::Stay
    }

    /// Called with the input taken; puts it back unless the key leaves it.
    fn find_key(&mut self, code: KeyCode) -> Outcome {
        match code {
            KeyCode::Enter => return Outcome::Stay,
            KeyCode::Esc => {
                self.reshape(|home| home.filter.clear());
                return Outcome::Stay;
            }
            KeyCode::Char(character) => self.reshape(|home| home.filter.push(character)),
            KeyCode::Backspace => self.reshape(|home| {
                home.filter.pop();
            }),
            _ => {}
        }
        self.input = Some(Input::Find);
        Outcome::Stay
    }

    /// Called with the input taken; puts it back unless the key leaves it.
    fn ask_key(&mut self, mut text: String, code: KeyCode) -> Outcome {
        match code {
            KeyCode::Char(character) => text.push(character),
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Enter if !text.trim().is_empty() => {
                return Outcome::Run(Action::Ask(text.trim().to_owned()));
            }
            KeyCode::Enter | KeyCode::Esc => return Outcome::Stay,
            _ => {}
        }
        self.input = Some(Input::Ask(text));
        Outcome::Stay
    }

    fn enter(&self) -> Outcome {
        match self.cursor_row() {
            Some(Row::Agent(agent)) if is_ended(agent) => {
                Outcome::Run(Action::Resume(agent.id.clone()))
            }
            Some(Row::Agent(agent)) => Outcome::Run(Action::Attach(agent.name.clone())),
            Some(Row::Views(views)) => Outcome::Run(Action::Attach(views[0].name.clone())),
            _ => Outcome::Stay,
        }
    }

    /// Enter on the layout list: the layout at `row` in the cursor's project, around the
    /// cursor's agent unless it ended (an ended agent's pane would be a shell). Nothing on an
    /// empty list.
    fn open_layout(&self, row: usize) -> Outcome {
        let Some(name) = self
            .context
            .layouts
            .as_ref()
            .and_then(|layouts| layouts.get(row))
        else {
            return Outcome::Stay;
        };
        Outcome::Run(Action::Layout {
            name: name.clone(),
            agent: self
                .cursor_agent()
                .filter(|agent| !is_ended(agent))
                .map(|agent| agent.id.clone()),
            project: self.cursor_project_path(),
        })
    }

    /// `r`: an ended agent, the assistant when it is stopped, or a warm again when the last one
    /// failed (on the assistant's row, or when no agent is listed to select).
    fn resume(&self) -> Outcome {
        let agent = self.cursor_agent();
        if matches!(self.context.assistant, AssistantState::Failed(_))
            && agent.is_none_or(|agent| agent.is_assistant)
        {
            return Outcome::Run(Action::Warm);
        }
        let Some(agent) = agent else {
            return Outcome::Stay;
        };
        if is_ended(agent) {
            return Outcome::Run(Action::Resume(agent.id.clone()));
        }
        match &self.context.assistant {
            AssistantState::Stopped { id } if agent.is_assistant => {
                Outcome::Run(Action::Resume(id.clone()))
            }
            _ => Outcome::Stay,
        }
    }

    fn unfold(&mut self, open: bool) {
        let Some(id) = self.cursor_agent().map(|agent| agent.id.clone()) else {
            return;
        };
        self.reshape(|home| {
            if open {
                home.unfolded.insert(id);
            } else {
                home.unfolded.remove(&id);
            }
        });
    }

    /// The next (or previous) selectable row, wrapping.
    fn step(&mut self, forward: bool) {
        let selectable: Vec<usize> = self
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is_selectable())
            .map(|(index, _)| index)
            .collect();
        if selectable.is_empty() {
            return;
        }
        let count = selectable.len();
        let next = match selectable.iter().position(|index| *index == self.cursor) {
            Some(position) if forward => (position + 1) % count,
            Some(position) => (position + count - 1) % count,
            None => 0,
        };
        self.cursor = selectable[next];
    }

    fn cursor_row(&self) -> Option<Row<'_>> {
        self.rows()
            .into_iter()
            .nth(self.cursor)
            .filter(Row::is_selectable)
    }

    fn cursor_agent(&self) -> Option<&AgentRow> {
        match self.cursor_row()? {
            Row::Agent(agent) => Some(agent),
            _ => None,
        }
    }

    /// The checkout of the cursor's project, else the home directory.
    fn cursor_project_path(&self) -> PathBuf {
        let project = match self.cursor_row() {
            Some(Row::Agent(agent)) => Some(agent.project),
            Some(Row::Views(views)) => self.view_project(views[0]),
            _ => None,
        };
        project.map_or_else(
            || self.context.home_directory.clone(),
            |project| self.context.projects[project].path.clone(),
        )
    }

    /// Runs a change that reshapes the rows, keeping the cursor on its session.
    fn reshape(&mut self, change: impl FnOnce(&mut Self)) {
        let name = self
            .cursor_row()
            .and_then(|row| row.session_name().map(str::to_owned));
        change(self);
        self.cursor = self.anchor(name.as_deref());
    }

    /// The row of session `name`, else the selectable row nearest the cursor's index.
    fn anchor(&self, name: Option<&str>) -> usize {
        let rows = self.rows();
        let named = name.and_then(|name| {
            rows.iter()
                .position(|row| row.is_selectable() && row.holds(name))
        });
        named
            .or_else(|| {
                rows.iter()
                    .enumerate()
                    .filter(|(_, row)| row.is_selectable())
                    .map(|(index, _)| index)
                    .min_by_key(|index| index.abs_diff(self.cursor))
            })
            .unwrap_or(self.cursor)
    }

    /// Projects by their newest agent, the assistant's project last.
    fn project_order(&self) -> Vec<usize> {
        let newest = |project: usize| {
            self.context
                .agents
                .iter()
                .filter(|agent| agent.project == project)
                .map(|agent| agent.age_at)
                .fold(f64::NEG_INFINITY, f64::max)
        };
        let is_system = |project: usize| {
            self.context.agents.iter().any(|agent| {
                agent.project == project && (agent.is_assistant || agent.tag == SYSTEM_TAG)
            })
        };
        let mut order: Vec<usize> = (0..self.context.projects.len()).collect();
        order.sort_by(|a, b| {
            is_system(*a)
                .cmp(&is_system(*b))
                .then(newest(*b).total_cmp(&newest(*a)))
        });
        order
    }

    fn parent_agent(&self, view: &ViewRow) -> Option<&AgentRow> {
        let parent = view.parent.as_ref()?;
        self.context.agents.iter().find(|agent| &agent.id == parent)
    }

    fn view_project(&self, view: &ViewRow) -> Option<usize> {
        self.parent_agent(view)
            .map(|agent| agent.project)
            .or(view.project)
    }

    /// The views of `project` with no listed parent agent, that the filter keeps.
    fn loose_views(&self, project: Option<usize>) -> Vec<&ViewRow> {
        self.context
            .views
            .iter()
            .filter(|view| {
                view.project == project
                    && self.parent_agent(view).is_none()
                    && (self.matches(&view.name)
                        || project.is_some_and(|project| {
                            self.matches(&self.context.projects[project].name)
                        }))
            })
            .collect()
    }

    /// An agent stays when its name, tag, project, branch or one of its views' names
    /// contains the find text.
    fn keeps_agent(&self, agent: &AgentRow) -> bool {
        self.matches(&agent.name)
            || self.matches(&agent.tag)
            || self.matches(&self.context.projects[agent.project].name)
            || agent
                .branch
                .as_deref()
                .is_some_and(|branch| self.matches(branch))
            || self
                .context
                .views
                .iter()
                .any(|view| view.parent.as_ref() == Some(&agent.id) && self.matches(&view.name))
    }

    fn matches(&self, text: &str) -> bool {
        text.to_lowercase().contains(&self.filter.to_lowercase())
    }

    /// Esc, `q`, Backspace and `h` leave a sub-list for the main list.
    fn back_on(&mut self, code: KeyCode) -> Outcome {
        if matches!(
            code,
            KeyCode::Esc | KeyCode::Backspace | KeyCode::Char('q' | 'h')
        ) {
            self.view = View::Main;
        }
        Outcome::Stay
    }
}

fn is_ended(agent: &AgentRow) -> bool {
    agent.state == LlmEngineState::Ended
}

fn scroll(row: usize, len: usize, code: KeyCode) -> usize {
    let last = len.saturating_sub(1);
    match code {
        KeyCode::Down | KeyCode::Char('j') => (row + 1).min(last),
        KeyCode::Up | KeyCode::Char('k') => row.saturating_sub(1),
        KeyCode::Home | KeyCode::Char('g') => 0,
        KeyCode::End | KeyCode::Char('G') => last,
        _ => row,
    }
}

/// How long a home opened by an attach acts only on navigation, Enter and close: keys typed
/// for the IDE while the client attached must not pick anything.
pub const GRACE: Duration = Duration::from_millis(400);

/// The key the home acts on, if any. Ctrl and Alt chords (the prefix and the IDE's root keys)
/// never reach it; during the grace period only arrows, `j`/`k`, Enter, Esc and `q` do.
pub fn accept(code: KeyCode, modifiers: KeyModifiers, in_grace: bool) -> Option<KeyCode> {
    if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return None;
    }
    let navigation = matches!(
        code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::Enter
            | KeyCode::Esc
            | KeyCode::Char('j' | 'k' | 'q')
    );
    (!in_grace || navigation).then_some(code)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            path: PathBuf::from(format!("/w/{name}")),
        }
    }

    pub(crate) fn agent(
        id: &str,
        name: &str,
        state: LlmEngineState,
        age_at: f64,
        project: usize,
    ) -> AgentRow {
        AgentRow {
            id: id.into(),
            name: name.into(),
            tag: "agent".into(),
            state,
            age_at,
            branch: None,
            chats: 1,
            project,
            is_assistant: false,
        }
    }

    fn assistant(age_at: f64, project: usize) -> AgentRow {
        AgentRow {
            tag: "tx-system".into(),
            is_assistant: true,
            ..agent("s1", "tx-assistant", LlmEngineState::Idle, age_at, project)
        }
    }

    fn view(name: &str, parent: Option<&str>, project: Option<usize>) -> ViewRow {
        ViewRow {
            name: name.into(),
            parent: parent.map(str::to_owned),
            project,
        }
    }

    /// tx-ide-rs (newest 900), wrangler (newest 950), tx-system (the assistant, newest 990),
    /// and one view that belongs nowhere.
    pub(crate) fn context() -> Context {
        Context {
            projects: vec![
                project("tx-ide-rs"),
                project("wrangler"),
                project("tx-system"),
            ],
            agents: vec![
                AgentRow {
                    branch: Some("feat/startup-ux".into()),
                    ..agent("a1", "startup-ux", LlmEngineState::Working, 900.0, 0)
                },
                agent("a2", "docs-site", LlmEngineState::Ended, 500.0, 0),
                AgentRow {
                    branch: Some("fix/retry-backoff".into()),
                    tag: "fixing".into(),
                    ..agent("w1", "fixer", LlmEngineState::Idle, 950.0, 1)
                },
                assistant(990.0, 2),
            ],
            views: vec![
                view("wrangler-p1-nvim", Some("w1"), Some(1)),
                view("build-watch", Some("w1"), Some(1)),
                view("scratch", None, None),
            ],
            details: BTreeMap::new(),
            assistant: AssistantState::Ready,
            plugins: vec![("spawn".into(), true), ("kill".into(), false)],
            keys: vec![KeyHint {
                plugin: "kill".into(),
                keys: vec!["X".into()],
                text: "stop".into(),
            }],
            now: 1000.0,
            home_directory: PathBuf::from("/home/operator"),
            layouts: None,
        }
    }

    fn first_run_context() -> Context {
        Context {
            projects: vec![project("tx-system")],
            agents: vec![assistant(990.0, 0)],
            views: Vec::new(),
            ..context()
        }
    }

    fn label(row: &Row<'_>) -> String {
        match row {
            Row::Project(project) => format!("project {}", project.name),
            Row::Agent(agent) => format!("agent {}", agent.name),
            Row::Detail(agent, detail) => match detail {
                Some(_) => format!("detail {}", agent.name),
                None => format!("detail {} …", agent.name),
            },
            Row::Views(views) => {
                let names: Vec<&str> = views.iter().map(|view| view.name.as_str()).collect();
                format!("views {}", names.join(" "))
            }
            Row::EndedFold { hidden, .. } => format!("{hidden} more ended"),
            Row::FirstRun => "first run".into(),
            Row::Loading => "loading".into(),
        }
    }

    fn labels(home: &Home) -> Vec<String> {
        home.rows().iter().map(label).collect()
    }

    fn selected(home: &Home) -> Option<String> {
        home.cursor().map(|index| label(&home.rows()[index]))
    }

    fn typed(home: &mut Home, text: &str) {
        for character in text.chars() {
            home.key(KeyCode::Char(character));
        }
    }

    const MAIN_FOOTER: &str = "enter attach · →/← unfold · n new here · / ask · f find · ? keys";

    #[test]
    fn projects_are_ordered_by_newest_activity_with_tx_system_last() {
        let home = Home::new(context());
        assert_eq!(
            labels(&home),
            [
                "project wrangler",
                "agent fixer",
                "views wrangler-p1-nvim build-watch",
                "project tx-ide-rs",
                "agent startup-ux",
                "agent docs-site",
                "project tx-system",
                "agent tx-assistant",
                "project views",
                "views scratch",
            ]
        );
    }

    #[test]
    fn ended_agents_fold_past_three_per_project_and_tab_unfolds() {
        let mut context = context();
        for (index, age_at) in [400.0, 300.0, 200.0, 100.0].into_iter().enumerate() {
            let id = format!("old-{index}");
            context
                .agents
                .push(agent(&id, &id, LlmEngineState::Ended, age_at, 0));
        }
        context
            .agents
            .push(agent("a3", "late", LlmEngineState::Starting, 10.0, 0));
        let mut home = Home::new(context);
        let tx_ide_rs = |home: &Home| -> Vec<String> {
            let rows = labels(home);
            let start = rows
                .iter()
                .position(|row| row == "project tx-ide-rs")
                .unwrap();
            let end = rows
                .iter()
                .position(|row| row == "project tx-system")
                .unwrap();
            rows[start + 1..end].to_vec()
        };
        assert_eq!(
            tx_ide_rs(&home),
            [
                "agent startup-ux",
                "agent late",
                "agent docs-site",
                "agent old-0",
                "agent old-1",
                "2 more ended",
            ],
            "running first, then the three newest ended"
        );
        assert_eq!(home.key(KeyCode::Tab), Outcome::Stay);
        assert_eq!(
            tx_ide_rs(&home),
            [
                "agent startup-ux",
                "agent late",
                "agent docs-site",
                "agent old-0",
                "agent old-1",
                "agent old-2",
                "agent old-3",
            ]
        );
        home.key(KeyCode::Tab);
        assert!(tx_ide_rs(&home).contains(&"2 more ended".to_owned()));
    }

    #[test]
    fn the_cursor_skips_project_rows_and_wraps() {
        let mut home = Home::new(context());
        assert_eq!(home.cursor(), Some(1));
        assert_eq!(selected(&home).unwrap(), "agent fixer");
        let mut visited = Vec::new();
        for _ in 0..6 {
            home.key(KeyCode::Char('j'));
            visited.push(selected(&home).unwrap());
        }
        assert_eq!(
            visited,
            [
                "views wrangler-p1-nvim build-watch",
                "agent startup-ux",
                "agent docs-site",
                "agent tx-assistant",
                "views scratch",
                "agent fixer",
            ],
            "j wraps from the last row to the first"
        );
        home.key(KeyCode::Up);
        assert_eq!(selected(&home).unwrap(), "views scratch", "k wraps back");
        home.key(KeyCode::Char('k'));
        home.key(KeyCode::Down);
        assert_eq!(selected(&home).unwrap(), "views scratch");
    }

    #[test]
    fn enter_attaches_running_and_resumes_ended() {
        let mut home = Home::new(context());
        home.select_session("startup-ux");
        assert_eq!(home.footer(), MAIN_FOOTER);
        assert_eq!(
            home.key(KeyCode::Enter),
            Outcome::Run(Action::Attach("startup-ux".into()))
        );
        home.select_session("docs-site");
        assert_eq!(
            home.footer(),
            "enter resume · →/← unfold · n new here · / ask · f find · ? keys"
        );
        assert_eq!(
            home.key(KeyCode::Enter),
            Outcome::Run(Action::Resume("a2".into()))
        );
        home.select_session("build-watch");
        assert_eq!(
            home.key(KeyCode::Enter),
            Outcome::Run(Action::Attach("wrangler-p1-nvim".into())),
            "a views row attaches its first view"
        );
    }

    #[test]
    fn unfold_adds_a_detail_row_under_the_agent_and_needs_detail_reports_it() {
        let mut home = Home::new(context());
        assert_eq!(home.needs_detail(), None);
        assert_eq!(home.key(KeyCode::Char('l')), Outcome::Stay);
        assert_eq!(
            labels(&home)[..4],
            [
                "project wrangler",
                "agent fixer",
                "detail fixer …",
                "views wrangler-p1-nvim build-watch",
            ]
        );
        assert_eq!(selected(&home).unwrap(), "agent fixer");
        assert_eq!(home.needs_detail(), Some("w1".into()));

        let mut context = context();
        context.details.insert(
            "w1".into(),
            Detail {
                roles: vec!["COMMON".into()],
                model: Some("opus".into()),
                effort: None,
                parent_name: None,
                last_user: Some("fix the retry".into()),
                last_agent: None,
                artifacts: vec![("doc".into(), 1)],
                started_at: 100.0,
                ended_at: None,
            },
        );
        home.replace_context(context);
        assert_eq!(labels(&home)[2], "detail fixer");
        assert_eq!(home.needs_detail(), None);

        home.key(KeyCode::Char('j'));
        home.key(KeyCode::Char('j'));
        home.key(KeyCode::Right);
        assert_eq!(selected(&home).unwrap(), "agent startup-ux");
        assert_eq!(home.needs_detail(), Some("a1".into()));
        home.key(KeyCode::Left);
        assert_eq!(home.needs_detail(), None);
        home.select_session("fixer");
        home.key(KeyCode::Char('h'));
        assert!(!labels(&home).iter().any(|row| row.starts_with("detail")));
    }

    #[test]
    fn n_uses_the_cursors_project_path() {
        let mut home = Home::new(context());
        home.select_session("startup-ux");
        assert_eq!(
            home.key(KeyCode::Char('n')),
            Outcome::Run(Action::New(PathBuf::from("/w/tx-ide-rs")))
        );
        home.select_session("build-watch");
        assert_eq!(
            home.key(KeyCode::Char('n')),
            Outcome::Run(Action::New(PathBuf::from("/w/wrangler"))),
            "a views row: its agent's project"
        );
        home.select_session("scratch");
        assert_eq!(
            home.key(KeyCode::Char('n')),
            Outcome::Run(Action::New(PathBuf::from("/home/operator"))),
            "a view under no project: the home directory"
        );
        let mut empty = Home::new(Context {
            projects: Vec::new(),
            agents: Vec::new(),
            ..first_run_context()
        });
        assert_eq!(empty.cursor(), None);
        assert_eq!(
            empty.key(KeyCode::Char('n')),
            Outcome::Run(Action::New(PathBuf::from("/home/operator")))
        );
    }

    #[test]
    fn x_then_y_stops_x_then_other_does_not() {
        let mut home = Home::new(context());
        home.select_session("startup-ux");
        assert_eq!(home.key(KeyCode::Char('x')), Outcome::Stay);
        assert_eq!(home.footer(), "stop startup-ux? y/n");
        assert_eq!(
            home.key(KeyCode::Char('y')),
            Outcome::Run(Action::Stop("a1".into()))
        );
        assert_eq!(home.footer(), MAIN_FOOTER);

        home.key(KeyCode::Char('x'));
        assert_eq!(home.key(KeyCode::Char('j')), Outcome::Stay);
        assert_eq!(home.footer(), MAIN_FOOTER, "the confirm is cleared");
        assert_eq!(
            selected(&home).unwrap(),
            "agent startup-ux",
            "and the key is otherwise ignored"
        );
        assert_eq!(home.key(KeyCode::Char('y')), Outcome::Stay);

        home.select_session("docs-site");
        home.key(KeyCode::Char('x'));
        assert_ne!(home.footer(), "stop docs-site? y/n", "an ended agent");
        assert_eq!(home.key(KeyCode::Char('y')), Outcome::Stay);

        home.select_session("tx-assistant");
        home.key(KeyCode::Char('x'));
        assert_eq!(
            home.key(KeyCode::Char('y')),
            Outcome::Run(Action::Stop("s1".into())),
            "the assistant is an agent too"
        );
    }

    #[test]
    fn find_keeps_rows_matching_name_tag_project_or_branch() {
        let mut home = Home::new(context());
        home.key(KeyCode::Char('f'));
        assert_eq!(home.footer(), "find › ");
        typed(&mut home, "retry");
        assert_eq!(home.footer(), "find › retry");
        assert_eq!(
            labels(&home),
            [
                "project wrangler",
                "agent fixer",
                "views wrangler-p1-nvim build-watch"
            ],
            "branch"
        );
        for _ in 0.."retry".len() {
            home.key(KeyCode::Backspace);
        }
        typed(&mut home, "tx-ide");
        assert_eq!(
            labels(&home),
            ["project tx-ide-rs", "agent startup-ux", "agent docs-site"],
            "project"
        );
        home.key(KeyCode::Esc);
        assert_eq!(home.footer(), MAIN_FOOTER, "Esc clears and leaves");
        assert_eq!(labels(&home).len(), 10);

        home.key(KeyCode::Char('f'));
        typed(&mut home, "FIXING");
        home.key(KeyCode::Enter);
        assert_eq!(home.footer(), MAIN_FOOTER, "Enter leaves");
        assert_eq!(labels(&home)[1], "agent fixer", "tag, any case");
        assert_eq!(labels(&home).len(), 3, "keeping the filter");

        home.key(KeyCode::Char('f'));
        home.key(KeyCode::Esc);
        typed(&mut home, "f");
        typed(&mut home, "docs");
        assert_eq!(
            labels(&home),
            ["project tx-ide-rs", "agent docs-site"],
            "name"
        );
        typed(&mut home, "zzz");
        assert!(labels(&home).is_empty());
        assert_eq!(home.cursor(), None);
        home.key(KeyCode::Enter);
        assert_eq!(home.key(KeyCode::Enter), Outcome::Stay);
    }

    #[test]
    fn slash_then_enter_asks_and_esc_cancels() {
        let mut home = Home::new_popup(context());
        assert_eq!(home.key(KeyCode::Char('/')), Outcome::Stay);
        assert_eq!(home.footer(), "ask › ");
        typed(&mut home, "why q?");
        home.key(KeyCode::Backspace);
        assert_eq!(home.footer(), "ask › why q");
        assert_eq!(
            home.key(KeyCode::Enter),
            Outcome::Run(Action::Ask("why q".into()))
        );

        let mut home = Home::new_popup(context());
        home.key(KeyCode::Char('/'));
        typed(&mut home, "never mind");
        assert_eq!(home.key(KeyCode::Esc), Outcome::Stay, "cancels, not closes");
        assert_eq!(home.footer(), format!("{MAIN_FOOTER} · q close"));
        home.key(KeyCode::Char('/'));
        assert_eq!(home.footer(), "ask › ", "the old text is gone");
        assert_eq!(
            home.key(KeyCode::Enter),
            Outcome::Stay,
            "empty: just leaves"
        );
        assert_eq!(home.footer(), format!("{MAIN_FOOTER} · q close"));
    }

    #[test]
    fn first_run_shows_the_three_verbs_and_the_assistant_row() {
        let home = Home::new(first_run_context());
        assert!(home.is_first_run());
        assert_eq!(
            labels(&home),
            ["project tx-system", "agent tx-assistant", "first run"]
        );
        assert_eq!(selected(&home).unwrap(), "agent tx-assistant");
        assert_eq!(
            home.footer(),
            "enter attach · n new · / ask · L tour · ? keys"
        );
        assert_eq!(
            Home::new_popup(first_run_context()).footer(),
            "enter attach · n new · / ask · L tour · ? keys · q close"
        );
        assert!(!Home::new(context()).is_first_run());
        let only_a_view = Context {
            views: vec![view("scratch", None, None)],
            ..first_run_context()
        };
        assert!(!Home::new(only_a_view).is_first_run());
    }

    #[test]
    fn rows_say_loading_until_the_first_context() {
        let mut home = Home::loading(false);
        assert_eq!(labels(&home), ["loading"]);
        assert!(!home.is_first_run());
        assert_eq!(home.cursor(), None);
        assert_eq!(home.footer(), MAIN_FOOTER);
        assert_eq!(home.key(KeyCode::Enter), Outcome::Stay);
        home.replace_context(context());
        assert!(labels(&home).contains(&"agent startup-ux".to_owned()));
        assert!(selected(&home).is_some());
        home.replace_context(first_run_context());
        assert!(home.is_first_run());
        assert_eq!(
            Home::loading(true).footer(),
            format!("{MAIN_FOOTER} · q close")
        );
    }

    #[test]
    fn returned_from_selects_that_session() {
        let mut home = Home::new(context());
        home.select_session("docs-site");
        assert_eq!(selected(&home).unwrap(), "agent docs-site");
        home.select_session("build-watch");
        assert_eq!(
            selected(&home).unwrap(),
            "views wrangler-p1-nvim build-watch"
        );
        home.select_session("gone");
        assert_eq!(
            selected(&home).unwrap(),
            "views wrangler-p1-nvim build-watch",
            "an unknown name leaves the cursor"
        );
    }

    #[test]
    fn replace_context_keeps_cursor_unfold_and_filter() {
        let mut home = Home::new(context());
        home.key(KeyCode::Char('f'));
        typed(&mut home, "s");
        home.key(KeyCode::Enter);
        home.select_session("startup-ux");
        home.key(KeyCode::Char('l'));
        let mut newer = context();
        newer.projects.push(project("fresh"));
        newer
            .agents
            .push(agent("f1", "fresher", LlmEngineState::Working, 999.0, 3));
        home.replace_context(newer.clone());
        assert_eq!(
            labels(&home),
            [
                "project fresh",
                "agent fresher",
                "project tx-ide-rs",
                "agent startup-ux",
                "detail startup-ux …",
                "agent docs-site",
                "project tx-system",
                "agent tx-assistant",
                "project views",
                "views scratch",
            ],
            "filter `s` still applied (fixer gone), the unfold kept"
        );
        assert_eq!(selected(&home).unwrap(), "agent startup-ux");

        newer.agents.retain(|agent| agent.id != "a1");
        home.replace_context(newer);
        assert_eq!(
            selected(&home).unwrap(),
            "agent docs-site",
            "a vanished row: the nearest one"
        );

        let mut asking = Home::new(context());
        asking.key(KeyCode::Char('/'));
        typed(&mut asking, "hi");
        asking.replace_context(context());
        assert_eq!(asking.footer(), "ask › hi", "the mode is kept");
    }

    #[test]
    fn q_closes_only_the_popup_variant() {
        let mut floor = Home::new(context());
        assert_eq!(floor.key(KeyCode::Char('q')), Outcome::Stay);
        assert_eq!(floor.key(KeyCode::Esc), Outcome::Stay);
        assert_eq!(floor.footer(), MAIN_FOOTER);
        let mut popup = Home::new_popup(context());
        assert_eq!(popup.footer(), format!("{MAIN_FOOTER} · q close"));
        assert_eq!(popup.key(KeyCode::Esc), Outcome::Close);
        assert_eq!(popup.key(KeyCode::Char('q')), Outcome::Close);
        popup.key(KeyCode::Char('p'));
        assert_eq!(
            popup.key(KeyCode::Char('q')),
            Outcome::Stay,
            "q leaves a sub-view first"
        );
        assert_eq!(popup.view(), View::Main);
    }

    #[test]
    fn b_asks_for_the_next_theme_in_normal_mode() {
        let mut home = Home::new(context());
        home.set_themed(true);
        assert_eq!(home.key(KeyCode::Char('b')), Outcome::NextTheme);
        assert_eq!(home.key(KeyCode::Char('t')), Outcome::Run(Action::Timeline));
        assert_eq!(home.footer(), format!("{MAIN_FOOTER} · b theme"));
        home.set_themed(false);
        assert_eq!(home.key(KeyCode::Char('b')), Outcome::Stay);
        assert_eq!(home.footer(), MAIN_FOOTER);
    }

    #[test]
    fn b_theme_comes_after_q_close() {
        let mut popup = Home::new_popup(context());
        popup.set_themed(true);
        assert_eq!(popup.footer(), format!("{MAIN_FOOTER} · q close · b theme"));
    }

    #[test]
    fn b_types_in_find_and_ask_modes() {
        for opener in ['f', '/'] {
            let mut home = Home::new(context());
            home.set_themed(true);
            home.key(KeyCode::Char(opener));
            assert_eq!(home.key(KeyCode::Char('b')), Outcome::Stay);
            let prompt = if opener == 'f' {
                "find › b"
            } else {
                "ask › b"
            };
            assert_eq!(home.footer(), prompt);
        }
    }

    #[test]
    fn a_notice_replaces_the_footer_for_two_seconds() {
        let mut home = Home::new(context());
        let now = Instant::now();
        home.notify("theme: koi".into(), now);
        assert_eq!(home.footer(), "theme: koi");
        assert!(!home.expire_notice(now + Duration::from_millis(1900)));
        assert_eq!(home.footer(), "theme: koi");
        assert!(home.expire_notice(now + Duration::from_secs(3)));
        assert_eq!(home.footer(), MAIN_FOOTER);
        assert!(!home.expire_notice(now + Duration::from_secs(4)));
    }

    #[test]
    fn the_verb_keys_run_their_actions() {
        let mut home = Home::new(context());
        home.select_session("fixer");
        assert_eq!(
            home.key(KeyCode::Char('a')),
            Outcome::Run(Action::Artifacts("w1".into()))
        );
        assert_eq!(home.key(KeyCode::Char('r')), Outcome::Stay, "running");
        for (key, action) in [
            ('t', Action::Timeline),
            ('T', Action::Timeline),
            ('L', Action::Tutor),
        ] {
            assert_eq!(home.key(KeyCode::Char(key)), Outcome::Run(action), "{key}");
        }
        home.select_session("docs-site");
        assert_eq!(
            home.key(KeyCode::Char('r')),
            Outcome::Run(Action::Resume("a2".into()))
        );
        home.select_session("build-watch");
        assert_eq!(home.key(KeyCode::Char('a')), Outcome::Stay, "views row");

        let mut stopped = Home::new(Context {
            assistant: AssistantState::Stopped { id: "s1".into() },
            ..first_run_context()
        });
        assert_eq!(
            stopped.key(KeyCode::Char('r')),
            Outcome::Run(Action::Resume("s1".into())),
            "the stopped assistant"
        );
    }

    #[test]
    fn r_warms_a_failed_assistant_from_its_row_or_an_empty_list() {
        let failed = AssistantState::Failed("no roles".into());
        let mut on_its_row = Home::new(Context {
            assistant: failed.clone(),
            ..first_run_context()
        });
        assert_eq!(selected(&on_its_row).unwrap(), "agent tx-assistant");
        assert_eq!(
            on_its_row.key(KeyCode::Char('r')),
            Outcome::Run(Action::Warm)
        );
        let mut no_record = Home::new(Context {
            assistant: failed.clone(),
            projects: Vec::new(),
            agents: Vec::new(),
            ..first_run_context()
        });
        assert_eq!(no_record.cursor(), None);
        assert_eq!(
            no_record.key(KeyCode::Char('r')),
            Outcome::Run(Action::Warm)
        );
        let mut elsewhere = Home::new(Context {
            assistant: failed,
            ..context()
        });
        elsewhere.select_session("fixer");
        assert_eq!(
            elsewhere.key(KeyCode::Char('r')),
            Outcome::Stay,
            "a running agent"
        );
    }

    #[test]
    fn plugins_and_keys_are_read_only_lists() {
        let mut home = Home::new(context());
        home.key(KeyCode::Char('p'));
        assert_eq!(home.view(), View::Plugins(0));
        assert_eq!(home.key(KeyCode::Enter), Outcome::Stay);
        home.key(KeyCode::Char('j'));
        assert_eq!(home.view(), View::Plugins(1));
        home.key(KeyCode::Char('q'));
        assert_eq!(home.view(), View::Main, "q leaves the list");
        home.key(KeyCode::Char('K'));
        assert_eq!(home.view(), View::Keys(0));
        home.key(KeyCode::Backspace);
        assert_eq!(home.view(), View::Main);
        home.key(KeyCode::Char('?'));
        assert_eq!(home.view(), View::Keys(0));
        home.key(KeyCode::Char('h'));
        assert_eq!(home.view(), View::Main);
        assert_eq!(selected(&home).unwrap(), "agent fixer", "the cursor stayed");
    }

    #[test]
    fn ctrl_and_alt_chords_never_reach_the_menu() {
        let none = KeyModifiers::NONE;
        assert_eq!(
            accept(KeyCode::Char('a'), KeyModifiers::CONTROL, false),
            None
        );
        assert_eq!(accept(KeyCode::Char('n'), KeyModifiers::ALT, false), None);
        assert_eq!(accept(KeyCode::Down, KeyModifiers::CONTROL, false), None);
        assert_eq!(
            accept(KeyCode::Char('n'), none, false),
            Some(KeyCode::Char('n'))
        );
        assert_eq!(
            accept(KeyCode::Char('K'), KeyModifiers::SHIFT, false),
            Some(KeyCode::Char('K'))
        );
    }

    #[test]
    fn the_grace_period_takes_only_navigation_enter_and_close() {
        let none = KeyModifiers::NONE;
        for code in [
            KeyCode::Char('n'),
            KeyCode::Char('T'),
            KeyCode::Char('1'),
            KeyCode::Char('h'),
            KeyCode::Char('/'),
        ] {
            assert_eq!(accept(code, none, true), None, "{code:?}");
        }
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Char('q'),
        ] {
            assert_eq!(accept(code, none, true), Some(code), "{code:?}");
        }
    }

    fn with_layouts() -> Context {
        Context {
            layouts: Some(vec!["pair".into(), "solo".into()]),
            ..context()
        }
    }

    #[test]
    fn w_does_nothing_without_the_layout_verb() {
        let mut home = Home::new(context());
        assert_eq!(home.key(KeyCode::Char('w')), Outcome::Stay);
        assert_eq!(home.view(), View::Main);
        assert_eq!(home.footer(), MAIN_FOOTER);
    }

    #[test]
    fn w_opens_the_layout_list_which_scrolls_and_backs_out() {
        let mut home = Home::new(with_layouts());
        assert_eq!(home.footer(), format!("{MAIN_FOOTER} · w layouts"));
        home.key(KeyCode::Char('w'));
        assert_eq!(home.view(), View::Layouts(0));
        home.key(KeyCode::Char('j'));
        assert_eq!(home.view(), View::Layouts(1));
        home.key(KeyCode::Char('j'));
        assert_eq!(home.view(), View::Layouts(1), "stops at the last");
        home.key(KeyCode::Char('k'));
        assert_eq!(home.view(), View::Layouts(0));
        assert_eq!(home.key(KeyCode::Esc), Outcome::Stay);
        assert_eq!(home.view(), View::Main);
        assert_eq!(selected(&home).unwrap(), "agent fixer", "the cursor stayed");
    }

    #[test]
    fn the_layout_footer_hint_follows_theme_and_close() {
        let mut home = Home::new_popup(with_layouts());
        home.set_themed(true);
        assert_eq!(
            home.footer(),
            format!("{MAIN_FOOTER} · q close · b theme · w layouts")
        );
    }

    #[test]
    fn enter_on_a_layout_opens_it_around_the_cursor() {
        let layout = |name: &str, agent: Option<&str>, project: &str| {
            Outcome::Run(Action::Layout {
                name: name.into(),
                agent: agent.map(str::to_owned),
                project: PathBuf::from(project),
            })
        };
        let mut home = Home::new(with_layouts());
        home.key(KeyCode::Char('w'));
        home.key(KeyCode::Char('j'));
        assert_eq!(
            home.key(KeyCode::Enter),
            layout("solo", Some("w1"), "/w/wrangler"),
            "a running agent's row: around that agent"
        );
        home.select_session("build-watch");
        home.key(KeyCode::Char('w'));
        assert_eq!(
            home.key(KeyCode::Enter),
            layout("pair", None, "/w/wrangler"),
            "a views row: fresh agents in its project"
        );
        home.select_session("docs-site");
        home.key(KeyCode::Char('w'));
        assert_eq!(
            home.key(KeyCode::Enter),
            layout("pair", None, "/w/tx-ide-rs"),
            "an ended agent's row: no agent"
        );
        home.select_session("scratch");
        home.key(KeyCode::Char('w'));
        assert_eq!(
            home.key(KeyCode::Enter),
            layout("pair", None, "/home/operator"),
            "a view under no project: the home directory"
        );
        let mut empty = Home::new(Context {
            layouts: Some(Vec::new()),
            ..context()
        });
        empty.key(KeyCode::Char('w'));
        assert_eq!(empty.key(KeyCode::Enter), Outcome::Stay, "an empty list");
        assert_eq!(empty.view(), View::Layouts(0));
    }

    #[test]
    fn action_round_trips_through_encode_decode() {
        for action in [
            Action::Attach("fixer".into()),
            Action::Resume("0f3a".into()),
            Action::New(PathBuf::from("/w/my project")),
            Action::Stop("0f3a".into()),
            Action::Ask("why is the build red?".into()),
            Action::Artifacts("0f3a".into()),
            Action::Timeline,
            Action::Tutor,
            Action::Warm,
            Action::Layout {
                name: "pair".into(),
                agent: Some("0f3a".into()),
                project: PathBuf::from("/w/my project"),
            },
            Action::Layout {
                name: "solo.v2".into(),
                agent: None,
                project: PathBuf::from("/w"),
            },
        ] {
            assert_eq!(Action::decode(&action.encode()), Some(action.clone()));
        }
        assert_eq!(
            Action::Layout {
                name: "pair".into(),
                agent: None,
                project: PathBuf::from("/w/my project"),
            }
            .encode(),
            "layout pair - /w/my project"
        );
        assert_eq!(Action::encode(&Action::New(PathBuf::from("/w"))), "new /w");
        assert_eq!(
            Action::decode("  ask hello there \n"),
            Some(Action::Ask("hello there".into()))
        );
        for bogus in [
            "",
            "resume ",
            "attach",
            "stop  ",
            "timeline now",
            "warm now",
            "layout",
            "layout pair",
            "layout pair -",
            "plugins",
            "keys",
            "bogus",
        ] {
            assert_eq!(Action::decode(bogus), None, "{bogus:?}");
        }
    }
}
