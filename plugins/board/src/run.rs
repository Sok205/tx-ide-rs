//! The terminal and the event loop: raw mode + alternate screen (restored on exit and on
//! panic), keys multiplexed with two ticks: the agent records every [`RECORDS_TICK`], GitHub
//! every [`GITHUB_TICK`] on a thread so the view keeps drawing. Requests a key made are carried
//! out here: `gh` for the issue, `tx create` for an agent, tx-client's `show` for a pane.
//!
//! The resident (viewed through a popup) cannot reach the outer client, so a show leaves
//! `<mode> <name>` in [`GOTO_OPTION`] on its own tmux session and detaches; `tx _board-attach`,
//! which runs on the outer client, picks it up: the timeline's arrangement.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use tx_client::show::Show;
use tx_client::tmux::Ui;
use tx_core::{Agent, TxCore};

use super::app::{App, Request};
use super::github::{self, Issue, Repository};
use super::model::{self, Board};
use super::ui;

pub const RECORDS_TICK: Duration = Duration::from_secs(3);
pub const GITHUB_TICK: Duration = Duration::from_secs(60);
/// Session option on the resident's tmux session: `<mode> <session name>` a show asked for.
pub const GOTO_OPTION: &str = "@tx_board_goto";
/// The agent definition `s` starts.
pub const AGENT_DEFINITION: &str = "developer";

/// `detach-client -s`: every client viewing `session` (the popup's nested attach) lets go.
pub fn detach_session(tmux: &Ui, session: &str) -> io::Result<()> {
    tmux.run(&["detach-client", "-s", &format!("={session}")])
        .map(drop)
        .map_err(io::Error::other)
}

/// What the loop runs on.
pub struct Context<'a> {
    pub core: &'a TxCore,
    pub repository: Repository,
    /// The git checkout `s` creates agents from; `None` when the board was opened outside one.
    pub checkout: Option<PathBuf>,
    /// The tmux session name of a popup-hosted board: q detaches instead of quitting.
    pub resident: Option<&'a str>,
}

fn agents(core: &TxCore) -> io::Result<Vec<Agent>> {
    model::load_agents(core).map_err(io::Error::other)
}

fn board(repository: &Repository, issues: &[Issue], agents: &[Agent]) -> Board {
    Board::build(repository.clone(), issues.to_vec(), agents)
}

pub fn run(context: Context<'_>) -> io::Result<()> {
    let tmux = Ui::from_core(context.core);
    let now = tx_core::events::now();
    let mut app = App::new(Board::empty(context.repository.clone()), now);
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(&mut terminal, &mut app, &context, &tmux);
    ratatui::restore();
    result
}

/// One fetch at a time, on a thread; its result is read on the loop's next pass.
struct Fetcher {
    pending: Option<Receiver<Result<Vec<Issue>, String>>>,
}

impl Fetcher {
    fn start(&mut self, repository: &Repository) {
        if self.pending.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        let repository = repository.clone();
        std::thread::spawn(move || {
            let _ = sender.send(github::fetch(&repository));
        });
        self.pending = Some(receiver);
    }

    fn poll(&mut self) -> Option<Result<Vec<Issue>, String>> {
        let outcome = self.pending.as_ref()?.try_recv().ok()?;
        self.pending = None;
        Some(outcome)
    }
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    context: &Context<'_>,
    tmux: &Ui,
) -> io::Result<()> {
    let mut issues: Vec<Issue> = Vec::new();
    let mut fetcher = Fetcher { pending: None };
    fetcher.start(&context.repository);
    app.refreshing = true;
    let mut viewer_asked = false;
    let mut next_records = Instant::now() + RECORDS_TICK;
    let mut next_github = Instant::now() + GITHUB_TICK;
    while !app.quit {
        terminal.draw(|frame| ui::render(app, frame))?;
        let wait = next_records
            .saturating_duration_since(Instant::now())
            .min(if app.refreshing {
                Duration::from_millis(200)
            } else {
                RECORDS_TICK
            });
        if event::poll(wait)?
            && let Event::Key(key) = event::read()?
            && key.kind != KeyEventKind::Release
        {
            app.handle_key(key);
        }
        if let Some(request) = app.request.take() {
            let refetch = perform(app, request, context, tmux, terminal, &issues)?;
            if refetch {
                fetcher.start(&context.repository);
                app.refreshing = true;
            }
        }
        if let Some(outcome) = fetcher.poll() {
            app.refreshing = false;
            let now = tx_core::events::now();
            match outcome {
                Ok(fetched) => {
                    issues = fetched;
                    app.fetched_at = Some(now);
                    app.refresh(
                        board(&context.repository, &issues, &agents(context.core)?),
                        now,
                    );
                    if !viewer_asked {
                        viewer_asked = true;
                        app.viewer = github::viewer().ok();
                    }
                }
                Err(reason) => app.notice = Some(reason),
            }
            next_github = Instant::now() + GITHUB_TICK;
        }
        if Instant::now() >= next_records {
            let now = tx_core::events::now();
            // A store hiccup keeps the last frame; the next tick retries.
            if let Ok(agents) = agents(context.core) {
                app.refresh(board(&context.repository, &issues, &agents), now);
            }
            next_records = Instant::now() + RECORDS_TICK;
        }
        if Instant::now() >= next_github && !app.refreshing {
            fetcher.start(&context.repository);
            app.refreshing = true;
        }
        if app.quit
            && let Some(session) = context.resident
        {
            detach_session(tmux, session)?;
            app.quit = false;
        }
    }
    Ok(())
}

/// Carries a request out; `Ok(true)` when the issues should be fetched again.
fn perform(
    app: &mut App,
    request: Request,
    context: &Context<'_>,
    tmux: &Ui,
    terminal: &mut ratatui::DefaultTerminal,
    issues: &[Issue],
) -> io::Result<bool> {
    let repository = &context.repository;
    let outcome: Result<bool, String> = match request {
        Request::Refresh => Ok(true),
        Request::Browser(number) => github::open_in_browser(repository, number).map(|()| false),
        Request::Close(number) => github::close(repository, number)
            .map(|()| true)
            .inspect(|_| app.notice = Some(format!("#{number} closed"))),
        Request::Reopen(number) => github::reopen(repository, number)
            .map(|()| true)
            .inspect(|_| app.notice = Some(format!("#{number} reopened"))),
        Request::ToggleAssignee(number) => toggle_assignee(app, repository, number, issues),
        Request::Comment(number) => on_terminal(terminal, || {
            github::interactive(repository, &["issue", "comment", &number.to_string()])
        })?
        .map(|()| true),
        Request::NewIssue => on_terminal(terminal, || {
            github::interactive(repository, &["issue", "create"])
        })?
        .map(|()| true),
        Request::StartAgent(number) => start_agent(app, context, terminal, number, issues)?,
        Request::ShowAgent(show, name) => {
            show_agent(tmux, context.resident, show, &name).map(|()| false)
        }
    };
    match outcome {
        Ok(refetch) => Ok(refetch),
        Err(reason) => {
            app.notice = Some(reason);
            Ok(false)
        }
    }
}

fn toggle_assignee(
    app: &App,
    repository: &Repository,
    number: u64,
    issues: &[Issue],
) -> Result<bool, String> {
    let viewer = app
        .viewer
        .as_deref()
        .ok_or_else(|| "gh: who you are is not known yet".to_owned())?;
    let assigned = issues
        .iter()
        .find(|issue| issue.number == number)
        .is_some_and(|issue| issue.assigned_to(viewer));
    github::assign(repository, number, viewer, !assigned).map(|()| true)
}

/// Leaves the alternate screen for `action` (an editor, a prompt) and comes back.
fn on_terminal<T>(
    terminal: &mut ratatui::DefaultTerminal,
    action: impl FnOnce() -> T,
) -> io::Result<T> {
    ratatui::restore();
    let outcome = action();
    *terminal = ratatui::try_init()?;
    terminal.clear()?;
    Ok(outcome)
}

/// The prompt the agent `s` starts is given: short and one line, as `tx create` wants it.
pub fn agent_prompt(issue: &Issue) -> String {
    format!(
        "We are solving GitHub issue #{} ({}): read it with gh, then propose an approach before coding.",
        issue.number, issue.url
    )
}

/// `tx create issue-<n> --cwd <checkout> --tag issue-<n> --agent developer --prompt …`.
pub fn create_words(checkout: &Path, issue: &Issue) -> Vec<String> {
    let tag = model::issue_tag(issue.number);
    vec![
        "create".to_owned(),
        tag.clone(),
        "--cwd".to_owned(),
        checkout.to_string_lossy().into_owned(),
        "--tag".to_owned(),
        tag,
        "--agent".to_owned(),
        AGENT_DEFINITION.to_owned(),
        "--prompt".to_owned(),
        agent_prompt(issue),
    ]
}

fn start_agent(
    app: &mut App,
    context: &Context<'_>,
    terminal: &mut ratatui::DefaultTerminal,
    number: u64,
    issues: &[Issue],
) -> io::Result<Result<bool, String>> {
    let Some(checkout) = &context.checkout else {
        return Ok(Err(
            "no checkout: open the board from a pane inside a git checkout, or pass --cwd"
                .to_owned(),
        ));
    };
    let Some(issue) = issues.iter().find(|issue| issue.number == number) else {
        return Ok(Err(format!("#{number}: not on the board")));
    };
    app.notice = Some(format!("starting an agent on #{number}…"));
    terminal.draw(|frame| ui::render(app, frame))?;
    let words = create_words(checkout, issue);
    let output = std::process::Command::new(tx_plugin::sdk::tx_bin())
        .args(context.core.config().cli_arguments())
        .args(&words)
        .env_remove("TX_SESSION_ID")
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        app.notice = Some(format!(
            "#{number}: agent {} started (Enter shows it once it is up)",
            model::issue_tag(number)
        ));
        return Ok(Ok(false));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let reason = stderr.lines().last().unwrap_or("failed").trim().to_owned();
    Ok(Err(format!("tx create: {reason}")))
}

/// Shows agent `name` that way: through the popup's goto option when resident, else from the
/// pane this board runs in.
fn show_agent(tmux: &Ui, resident: Option<&str>, show: Show, name: &str) -> Result<(), String> {
    if show == Show::Focus && tmux.pane_for_session(name, "").is_none() {
        return Err(format!(
            "{name}: not attached to any pane (a attaches it, p pops it up)"
        ));
    }
    match resident {
        Some(session) => {
            tmux.set_session_option(session, GOTO_OPTION, &format!("{} {name}", show.name()))
                .map_err(|error| error.to_string())?;
            detach_session(tmux, session).map_err(|error| error.to_string())
        }
        None => {
            let client = tmux
                .display_message("#{client_name}", None)
                .ok_or_else(|| "not inside tmux".to_owned())?;
            let pane = tmux.current_pane_id();
            tmux.show_session(&client, show, name, pane.as_deref())
        }
    }
}

/// One frame at `columns` × `rows`, as plain text: no terminal and no `gh` needed.
pub fn snapshot(board: Board, now: f64, fetched_at: f64, columns: u16, rows: u16) -> String {
    let mut app = App::new(board, now);
    app.fetched_at = Some(fetched_at);
    let mut terminal = Terminal::new(TestBackend::new(columns, rows)).expect("test backend");
    terminal
        .draw(|frame| ui::render(&mut app, frame))
        .expect("test backend");
    ui::buffer_text(terminal.backend().buffer())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_create_command_tags_the_agent_with_the_issue() {
        let issue = Issue {
            number: 116,
            url: "https://github.com/o/n/issues/116".to_owned(),
            ..Issue::default()
        };
        let words = create_words(Path::new("/work/n"), &issue);
        assert_eq!(
            &words[..8],
            [
                "create",
                "issue-116",
                "--cwd",
                "/work/n",
                "--tag",
                "issue-116",
                "--agent",
                "developer"
            ]
        );
        assert_eq!(words[8], "--prompt");
        assert!(words[9].contains("#116 (https://github.com/o/n/issues/116)"));
        assert!(!words[9].contains('\n'));
    }
}
