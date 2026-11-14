//! `tx layout open`: build a layout's panes as one view, or show the view already built for the
//! same layout and directory (its binding `<layout>:<cwd>`).
//!
//! The agents come first, in file order, so every pane's program is known before the view
//! exists. Then pane 0 opens the view, the window is sized to the client that will show it, and
//! every later pane splits the pane its `of` names, by pane id. A failure after the first
//! agent closes the view and stops every agent this run created.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tx_client::editor::Terminal;
use tx_client::tmux::{Ui, shell_quote};
use tx_client::views::{self, SplitDirection, SplitRequest, ViewKind, ViewRequest, default_shell};
use tx_core::{
    Agent, AgentId, CreateOptions, DefinitionChoice, LlmEngineState, TxCore, VerbError, VerbIo,
};

use crate::pane::{AgentPane, Content, Pane, Split};

/// Spawn's default tag, copied: an agent a layout starts without `--tag` is tagged like one
/// started with prefix+n.
pub const DEFAULT_TAG: &str = "agent";
/// The first message of an agent pane without `prompt:`.
pub const DEFAULT_PROMPT: &str = "Introduce yourself, then wait for instructions.";

pub struct OpenRequest<'a> {
    /// The layout's name, valid by [`crate::pane::is_valid_name`].
    pub layout: &'a str,
    pub panes: &'a [Pane],
    /// Canonical: the binding compares it as text.
    pub cwd: &'a Path,
    /// `--agent`: the agent the first `agent:` pane shows instead of a new one.
    pub agent: Option<&'a str>,
    /// `--tag`s; empty: the `--agent` agent's tags, else [`DEFAULT_TAG`].
    pub tags: &'a [String],
    pub detach: bool,
}

fn failed(error: impl std::fmt::Display) -> VerbError {
    VerbError::Failed(error.to_string())
}

pub fn open(
    core: &TxCore,
    io: &mut VerbIo<'_>,
    request: &OpenRequest<'_>,
) -> Result<i32, VerbError> {
    let ui = Ui::from_core(core);
    let show = Show::choose(&ui, request.detach);
    let bound = format!("{}:{}", request.layout, request.cwd.display());
    let existing = views::list(&ui)
        .map_err(failed)?
        .into_iter()
        .find(|view| view.bound.as_deref() == Some(bound.as_str()));
    let view = match existing {
        Some(view) => view.name,
        None => {
            let given = request
                .agent
                .map(|token| core.resolve(token).and_then(|id| core.get_agent(&id)))
                .transpose()?;
            let tags = layout_tags(
                request.tags,
                given.as_ref().map(|agent| agent.tags.as_slice()),
            );
            let mut build = Build {
                core,
                ui: &ui,
                request,
                tags,
                parent: agent_from_env(core),
                created: Vec::new(),
                view: None,
            };
            let view = match build.run(given.as_ref(), &bound, &show) {
                Ok(view) => view,
                Err(error) => {
                    build.undo(io);
                    return Err(error);
                }
            };
            writeln!(io.stdout, "{view} view created.").map_err(failed)?;
            view
        }
    };
    show.show(core, &ui, io, &view)
}

/// `--tag`s, else the tags of the agent given with `--agent`, so a layout opened on an agent
/// stays in its scope, else [`DEFAULT_TAG`].
fn layout_tags(tags: &[String], given: Option<&[String]>) -> Vec<String> {
    match given {
        _ if !tags.is_empty() => tags.to_vec(),
        Some(given) => given.to_vec(),
        None => vec![DEFAULT_TAG.to_owned()],
    }
}

/// The caller's own agent, the parent of what a layout starts: `$TX_SESSION_ID`, only when
/// `$TMUX` names the core's own server (the rule `tx send` uses for its sender).
fn agent_from_env(core: &TxCore) -> Option<AgentId> {
    let tmux = std::env::var("TMUX").ok()?;
    let socket = tmux.split(',').next()?;
    if Path::new(socket) != core.socket_path() {
        return None;
    }
    std::env::var("TX_SESSION_ID").ok()?.parse().ok()
}

/// `<layout>-<project>`, where the project is the main checkout `cwd` belongs to, so a
/// layout opened in any worktree of a repository is named after the repository.
fn view_base(layout: &str, project: &Path) -> String {
    let project = project
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    views::sanitize(&format!("{layout}-{project}"))
}

/// The repository's main checkout, the parent of its common git directory (the home's rule for
/// its project rows); `None` outside a git checkout.
fn main_checkout(directory: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let common = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    common.parent().map(Path::to_owned)
}

/// The first of `base`, `base-2`, `base-3` … that no session on the server holds.
fn free_view_name(ui: &Ui, base: &str) -> String {
    let mut name = base.to_owned();
    let mut suffix = 1;
    while ui.has_session(&name) {
        suffix += 1;
        name = format!("{base}-{suffix}");
    }
    name
}

/// What one pane runs.
enum Program {
    /// A nested attach of a running agent's session.
    Nest {
        target: String,
        worktree: PathBuf,
    },
    Command(Vec<String>),
    Shell,
}

impl Program {
    /// The program and its arguments, as separate words for tmux.
    fn words(&self, ui: &Ui) -> (String, Vec<String>) {
        match self {
            Program::Nest { target, .. } => (
                "bash".to_owned(),
                vec!["-c".to_owned(), ui.nest_attach_script(target)],
            ),
            Program::Command(words) => (words[0].clone(), words[1..].to_vec()),
            Program::Shell => (default_shell(), Vec::new()),
        }
    }
}

struct Build<'a> {
    core: &'a TxCore,
    ui: &'a Ui,
    request: &'a OpenRequest<'a>,
    tags: Vec<String>,
    parent: Option<AgentId>,
    /// Every agent this run started, stopped again by [`Build::undo`].
    created: Vec<AgentId>,
    /// Set once the view exists, closed again by [`Build::undo`].
    view: Option<String>,
}

impl Build<'_> {
    fn run(
        &mut self,
        given: Option<&Agent>,
        bound: &str,
        show: &Show,
    ) -> Result<String, VerbError> {
        let programs = self.programs(given)?;
        // Command and shell panes run in the first shown agent's worktree.
        let directory = programs
            .iter()
            .find_map(|program| match program {
                Program::Nest { worktree, .. } => Some(worktree.as_path()),
                _ => None,
            })
            .unwrap_or(self.request.cwd);
        let project = main_checkout(self.request.cwd);
        let base = view_base(
            self.request.layout,
            project.as_deref().unwrap_or(self.request.cwd),
        );
        let name = free_view_name(self.ui, &base);
        let (program, arguments) = programs[0].words(self.ui);
        views::open(
            self.ui,
            &ViewRequest {
                name: &name,
                kind: ViewKind::Shell,
                program: &program,
                arguments: &arguments,
                working_directory: directory,
                tags: &self.tags,
                parent_id: self.parent.as_ref(),
                bound: Some(bound),
            },
        )
        .map_err(failed)?;
        self.view = Some(name.clone());
        let window = format!("={name}:");
        let first = self
            .ui
            .run(&["display-message", "-p", "-t", &window, "#{pane_id}"])
            .map_err(failed)?;
        let mut pane_ids = vec![first.trim().to_owned()];
        // The labels hook fires on new windows only; a view built from new-session and splits
        // gets its borders here. One call covers every pane, split before or after.
        self.ui
            .run(&[
                "set-window-option",
                "-t",
                &window,
                "pane-border-status",
                "top",
            ])
            .map_err(failed)?;
        // tmux spreads a later resize evenly over the panes instead of keeping their
        // proportions, so the splits are made at the size the view will be seen at. That pins
        // the window's `window-size` to manual; it is unset again once the panes exist.
        let size = show.window_size(self.ui)?;
        if let Some((columns, rows)) = size {
            self.ui
                .run(&[
                    "resize-window",
                    "-t",
                    &window,
                    "-x",
                    &columns.to_string(),
                    "-y",
                    &rows.to_string(),
                ])
                .map_err(failed)?;
        }
        for (pane, program) in self.request.panes.iter().zip(&programs).skip(1) {
            let placement = pane
                .placement
                .as_ref()
                .expect("pane.rs gives every pane after the first a placement");
            let (program, arguments) = program.words(self.ui);
            let id = views::split(
                self.ui,
                &SplitRequest {
                    target: &pane_ids[placement.of],
                    direction: match placement.split {
                        Split::Right => SplitDirection::Horizontal,
                        Split::Below => SplitDirection::Vertical,
                    },
                    size: Some(&placement.size),
                    program: &program,
                    arguments: &arguments,
                    working_directory: directory,
                },
            )
            .map_err(failed)?;
            pane_ids.push(id);
        }
        if size.is_some() {
            self.ui
                .run(&["set-window-option", "-u", "-t", &window, "window-size"])
                .map_err(failed)?;
        }
        self.ui.select_pane(&pane_ids[0]);
        Ok(name)
    }

    /// Every pane's program, in file order, starting the agents. The first `agent:` pane shows
    /// the `--agent` agent instead of a new one, or a shell when it is not running.
    fn programs(&mut self, given: Option<&Agent>) -> Result<Vec<Program>, VerbError> {
        let mut given = given.map(|agent| self.running(&agent.id)).transpose()?;
        let mut programs = Vec::with_capacity(self.request.panes.len());
        for pane in self.request.panes {
            let program = match &pane.content {
                Content::Agent(agent) => match given.take() {
                    Some(program) => program,
                    None => self.create(agent)?,
                },
                Content::Attach(token) => self.running(&self.core.resolve(token)?)?,
                Content::Command(words) => Program::Command(words.clone()),
                Content::Shell => Program::Shell,
            };
            programs.push(program);
        }
        Ok(programs)
    }

    /// A nested attach of `id`, or a shell when it is not running.
    fn running(&self, id: &AgentId) -> Result<Program, VerbError> {
        let agent = self.core.refresh_agent(id)?.agent().clone();
        if agent.state == LlmEngineState::Ended {
            return Ok(Program::Shell);
        }
        let location = self.core.get_execution_location(id)?;
        Ok(Program::Nest {
            target: location.session_target,
            worktree: agent.working_directory,
        })
    }

    /// Starts the agent an `agent:` pane names, as `tx create --agent` would with the pane's
    /// keys as flags: config.yaml's `agent:` block fills in engine, model and effort.
    fn create(&mut self, agent: &AgentPane) -> Result<Program, VerbError> {
        let options = [("model", &agent.model), ("effort", &agent.effort)]
            .into_iter()
            .filter_map(|(key, value)| value.clone().map(|value| (key.to_owned(), value)));
        let launch = self.core.config().agent_defaults.launch(
            agent.engine,
            Vec::new(),
            Some(DefinitionChoice::new(&agent.definition)),
            options,
            agent.prompt.as_deref().unwrap_or(DEFAULT_PROMPT),
        );
        let tags = agent.tags.clone().unwrap_or_else(|| self.tags.clone());
        let name = agent.name.as_deref().unwrap_or(&agent.definition);
        let mut create = CreateOptions::new(name, self.request.cwd, launch).tags(tags);
        if let Some(parent) = &self.parent {
            create = create.parent(parent.clone());
        }
        let id = self.core.create_agent(create)?;
        self.created.push(id.clone());
        self.running(&id)
    }

    /// Closes the half-built view and stops the agents this run started; what cannot be undone
    /// is reported on stderr, after which the original error is returned by the caller.
    fn undo(&self, io: &mut VerbIo<'_>) {
        if let Some(view) = &self.view
            && let Err(error) = views::close(self.ui, view)
        {
            let _ = writeln!(io.stderr, "tx layout: could not close {view}: {error}");
        }
        for id in &self.created {
            if let Err(error) = self.core.stop_agent(id) {
                let _ = writeln!(io.stderr, "tx layout: could not stop {id}: {error}");
            }
        }
    }
}

/// How the view reaches the operator, the tutor's three ways.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Show {
    /// Inside the core's server: switch the terminal client the caller is shown on.
    Switch(String),
    /// `--detach`, no terminal, or inside the server with no terminal client showing the caller
    /// (an agent in a session nobody watches): print how to attach.
    Print,
    /// From a terminal outside the server: become the attached client.
    Attach,
}

impl Show {
    fn choose(ui: &Ui, detach: bool) -> Self {
        if ui.inside() {
            ui.terminal_client().map_or(Show::Print, Show::Switch)
        } else if detach || !std::io::stdin().is_terminal() {
            Show::Print
        } else {
            Show::Attach
        }
    }

    /// `(columns, rows)` of the window the view will be seen in: the caller's terminal client's
    /// when switching, this terminal's when attaching, and when printing the window of the
    /// terminal client the most recently active client walks up to (the operator's, as the
    /// home's `w` calls it).
    /// `None`: no client to measure, and tmux's default size stays.
    fn window_size(&self, ui: &Ui) -> Result<Option<(usize, usize)>, VerbError> {
        if *self == Show::Attach {
            let (rows, columns) = Terminal::size();
            return Ok(Some((columns, rows)));
        }
        // `list-clients` formats each line for its own client; `display-message -c` from
        // outside the server formats for the latest session instead.
        let listing = ui.quiet(&["list-clients", "-F", CLIENT_FORMAT]).1;
        let mut clients = parse_clients(&listing);
        let client = match self {
            Show::Switch(client) => client.clone(),
            _ => {
                // A nested attach in a view no terminal shows walks to nothing; the next
                // client then counts.
                clients.sort_by_key(|client| std::cmp::Reverse(client.activity));
                let terminal = clients
                    .iter()
                    .find_map(|client| ui.outer_client(&client.name));
                match terminal {
                    Some((client, _)) => client,
                    None => return Ok(None),
                }
            }
        };
        Ok(clients
            .into_iter()
            .find(|candidate| candidate.name == client)
            .map(|client| (client.columns, client.rows)))
    }

    fn show(
        self,
        core: &TxCore,
        ui: &Ui,
        io: &mut VerbIo<'_>,
        view: &str,
    ) -> Result<i32, VerbError> {
        match self {
            Show::Switch(client) => {
                ui.run(&["switch-client", "-c", &client, "-t", &format!("={view}")])
                    .map_err(failed)?;
                Ok(0)
            }
            Show::Print => {
                writeln!(
                    io.stdout,
                    "{view} is ready: tmux -S {} attach -t ={view}",
                    shell_quote(&core.socket_path().to_string_lossy())
                )
                .map_err(failed)?;
                Ok(0)
            }
            Show::Attach => {
                let _ = io.stdout.flush();
                Err(failed(ui.attach_exec(&format!("={view}"))))
            }
        }
    }
}

const CLIENT_FORMAT: &str = "#{client_name}\t#{client_activity}\t#{window_width}\t#{window_height}";

/// One client of the server, with the size of the window it shows.
#[derive(Debug, PartialEq, Eq)]
struct Client {
    name: String,
    activity: u64,
    columns: usize,
    rows: usize,
}

/// A `list-clients -F CLIENT_FORMAT` listing.
fn parse_clients(listing: &str) -> Vec<Client> {
    listing
        .lines()
        .filter_map(|line| {
            let [name, activity, columns, rows] = line.split('\t').collect::<Vec<_>>()[..] else {
                return None;
            };
            Some(Client {
                name: name.to_owned(),
                activity: activity.parse().ok()?,
                columns: columns.parse().ok()?,
                rows: rows.parse().ok()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_view_is_named_after_the_layout_and_the_project() {
        assert_eq!(
            view_base("pair", Path::new("/Users/me/src/tx-ide-rs")),
            "pair-tx-ide-rs"
        );
        assert_eq!(view_base("v1.2", Path::new("/w/proj #1")), "v1-2-proj--1");
    }

    #[test]
    fn the_main_checkout_names_a_worktree_view_and_a_plain_directory_names_itself() {
        let root = std::env::temp_dir().join(format!("txlayouts-{}", std::process::id()));
        let checkout = root.join("repo");
        std::fs::create_dir_all(&checkout).unwrap();
        let git = |arguments: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(arguments)
                .output()
                .is_ok_and(|output| output.status.success())
        };
        if !git(&["init", "-q"]) {
            eprintln!("git not available; skipping");
            return;
        }
        let checkout = checkout.canonicalize().unwrap();
        assert!(git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x"
        ]));
        let worktree = root.join("repo--dev");
        assert!(git(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap()
        ]));
        assert_eq!(main_checkout(&checkout).unwrap(), checkout);
        assert_eq!(main_checkout(&worktree).unwrap(), checkout);
        let plain = root.join("plain");
        std::fs::create_dir_all(&plain).unwrap();
        assert_eq!(main_checkout(&plain), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn tags_come_from_the_flags_then_the_given_agent_then_the_default() {
        let given = ["review".to_owned(), "x".to_owned()];
        let flags = ["t".to_owned()];
        assert_eq!(layout_tags(&flags, Some(&given)), ["t"]);
        assert_eq!(layout_tags(&[], Some(&given)), ["review", "x"]);
        assert_eq!(layout_tags(&[], None), [DEFAULT_TAG]);
        assert!(layout_tags(&[], Some(&[])).is_empty());
    }

    #[test]
    fn clients_are_read_with_their_window_size() {
        assert_eq!(
            parse_clients("/dev/ttys001\t100\t240\t60\nbogus\n/dev/ttys002\t300\t59\t20\n"),
            [
                Client {
                    name: "/dev/ttys001".to_owned(),
                    activity: 100,
                    columns: 240,
                    rows: 60,
                },
                Client {
                    name: "/dev/ttys002".to_owned(),
                    activity: 300,
                    columns: 59,
                    rows: 20,
                },
            ]
        );
        assert!(parse_clients("").is_empty());
    }
}
