//! The terminal and the event loop: raw mode, the alternate screen and mouse capture, restored
//! on exit and on panic.
//!
//! Every [`REFRESH`] the loop re-reads the records, follows core's log and reads what the
//! transcripts appended; each new message starts a pulse. It draws at [`FAST_FRAME`] while a
//! pulse is in flight and at [`FRAME`] otherwise, enough for the breathing and the spinner.
//!
//! Keys ask for [`Request`]s. Send, fork, stop and respawn run as `tx` verbs in the background
//! ([`crate::actions`]). Enter (focus the agent's pane) and `a` (the agent's artifact menu) need
//! the operator's terminal: run in a pane, the loop does them itself. The resident (viewed
//! through a popup) cannot, since a popup covers the terminal, so it leaves the request in
//! [`AFTER_OPTION`] on its own tmux session and detaches; `tx _graph-open`, which opened the
//! popup, carries it out once the popup has closed.

use std::io;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use tx_client::tmux::Ui;
use tx_core::core::LOG_FILE;
use tx_core::{Agent, LlmEngineState, TxCore};

use crate::actions::{self, Runner, Tx};
use crate::app::{App, Request, Source};
use crate::model::{self, Filter, LogTail, TranscriptReader};
use crate::ui;

/// The redraw pace with nothing travelling: enough for the spinner and the breathing.
pub const FRAME: Duration = Duration::from_millis(250);
/// The redraw pace while a pulse is in flight.
pub const FAST_FRAME: Duration = Duration::from_millis(33);
/// How often the records, the log and the transcripts are read again.
pub const REFRESH: Duration = Duration::from_secs(1);
/// Session option on the resident's tmux session: what to do once the popup closed,
/// `focus <agent name>` or `artifacts <agent id>`.
pub const AFTER_OPTION: &str = "@tx_graph_after";
/// What a pane runs when it can take a nested attach: a shell, or a nested client already.
const HOSTING_COMMANDS: [&str; 6] = ["zsh", "bash", "sh", "fish", "dash", "tmux"];

/// What a live view reads from: core, the log it follows and the transcripts it follows.
pub struct Feed<'a> {
    core: &'a TxCore,
    log: LogTail,
    transcripts: TranscriptReader,
}

impl<'a> Feed<'a> {
    pub fn new(core: &'a TxCore) -> Self {
        Self {
            core,
            log: LogTail::new(core.config().directories.data.join(LOG_FILE)),
            transcripts: TranscriptReader::default(),
        }
    }

    /// The records, the window's messages (following starts after them) and each agent's
    /// context.
    pub fn source(&mut self, now: f64, window: f64) -> io::Result<Source> {
        let messages = self.log.backfill(now - window, now)?;
        let mut source = Source {
            messages,
            ..Source::default()
        };
        self.reload(&mut source, now, window)?;
        Ok(source)
    }

    /// Reads the records and the contexts again; returns the messages appended to the log.
    pub fn refresh(
        &mut self,
        source: &mut Source,
        now: f64,
        window: f64,
    ) -> io::Result<Vec<model::Message>> {
        let messages = self.log.follow()?;
        self.reload(source, now, window)?;
        Ok(messages)
    }

    fn reload(&mut self, source: &mut Source, now: f64, window: f64) -> io::Result<()> {
        source.agents = model::load_agents(self.core).map_err(io::Error::other)?;
        let lively = |agent: &&Agent| {
            agent.state != LlmEngineState::Ended
                || agent.last_activity_at.unwrap_or(agent.last_modified_at) >= now - window
        };
        source.contexts.clear();
        for agent in source.agents.iter().filter(lively) {
            let Some(conversation) = self.core.conversation(&agent.id).ok().flatten() else {
                continue;
            };
            if let Some(context) = self.transcripts.context(&conversation.transcript_path) {
                source.contexts.insert(agent.id.clone(), context);
            }
        }
        Ok(())
    }
}

/// The header's brand: `TX_APPNAME`, as `tx` reads it (empty counts as unset), else
/// [`crate::app::DEFAULT_BRAND`].
pub fn brand() -> String {
    std::env::var("TX_APPNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| crate::app::DEFAULT_BRAND.to_owned())
}

/// The source of one snapshot.
pub fn source(core: &TxCore, now: f64, window: f64) -> io::Result<Source> {
    Feed::new(core).source(now, window)
}

/// Shows agent `name` on `client`'s terminal, as the picker opens a session: the pane already
/// showing it, else a nested attach in `pane` (where prefix+g was pressed) when that pane is a
/// shell of a view built to host nested attaches (the home base, the tutor), else the client
/// switched to the agent's session. `Err` is the reason, for the operator.
pub fn show_agent(tmux: &Ui, client: &str, pane: Option<&str>, name: &str) -> Result<(), String> {
    // The client may be nested in a view's pane; the terminal is the one to move.
    let terminal = || {
        tmux.outer_client(client)
            .map(|(client, _)| client)
            .ok_or_else(|| "no terminal client to move".to_owned())
    };
    if let Some(shown) = tmux.pane_for_session(name, "") {
        tmux.run(&["switch-client", "-c", &terminal()?, "-t", &shown])
            .map_err(|error| error.to_string())?;
        tmux.select_window(&shown);
        tmux.select_pane(&shown);
        return Ok(());
    }
    let target = format!("={name}");
    if let Some(pane) = pane.filter(|pane| hosts_nested_attach(tmux, pane, name)) {
        return tmux
            .respawn_pane(pane, &tmux.nest_attach_command(&target))
            .map_err(|error| error.to_string());
    }
    tmux.run(&["switch-client", "-c", &terminal()?, "-t", &target])
        .map(drop)
        .map_err(|error| error.to_string())
}

/// The picker's rule: `pane` takes a nested attach of `name` when its session is a view that
/// hosts nested attaches, `name` is not such a view, and the pane runs a shell (or a nested
/// client). Never an agent's own pane, never an editor.
fn hosts_nested_attach(tmux: &Ui, pane: &str, name: &str) -> bool {
    let hosting_view = tmux
        .display_message("#S", Some(pane))
        .is_some_and(|session| tmux.hosts_nested_attaches(&session));
    let command = tmux.display_message("#{pane_current_command}", Some(pane));
    hosting_view
        && !tmux.hosts_nested_attaches(name)
        && command.is_some_and(|command| HOSTING_COMMANDS.contains(&command.as_str()))
}

/// The agent's artifact menu on `client`, through `tx _artifact-menu`.
pub fn artifact_menu(tx: &Tx, id: &str, client: &str) -> Result<(), String> {
    let words = ["_artifact-menu", id, client].map(str::to_owned);
    let output = tx
        .command(&words)
        .output()
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&output.stderr)
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or("failed")
        .to_owned())
}

/// `detach-client -s`: every client viewing `session` (the popup's nested attach) lets go.
pub fn detach_session(tmux: &Ui, session: &str) -> io::Result<()> {
    tmux.run(&["detach-client", "-s", &format!("={session}")])
        .map(drop)
        .map_err(io::Error::other)
}

/// `resident` is the tmux session name of a popup-hosted graph: q and Enter detach it instead
/// of quitting.
pub fn run(core: &TxCore, filter: Filter, window: f64, resident: Option<&str>) -> io::Result<()> {
    let tmux = Ui::from_core(core);
    let mut feed = Feed::new(core);
    let now = tx_core::events::now();
    let mut app = App::new(feed.source(now, window)?, filter, now, window);
    app.brand = brand();
    let mut terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableMouseCapture)?;
    let tx = Tx::current(core)?;
    let runner = Runner::new(tx.clone());
    let result = event_loop(
        &mut terminal,
        &mut app,
        &mut feed,
        &Place {
            tmux: &tmux,
            tx: &tx,
            runner: &runner,
            resident,
        },
    );
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// Where requests are carried out.
struct Place<'a> {
    tmux: &'a Ui,
    tx: &'a Tx,
    runner: &'a Runner,
    /// The resident's tmux session, when the view is the popup's resident.
    resident: Option<&'a str>,
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    feed: &mut Feed<'_>,
    place: &Place<'_>,
) -> io::Result<()> {
    let started = Instant::now();
    let mut next_refresh = Instant::now() + REFRESH;
    while !app.quit {
        app.clock = started.elapsed().as_secs_f64();
        terminal.draw(|frame| ui::render(app, frame))?;
        let frame = if app.animating() { FAST_FRAME } else { FRAME };
        if event::poll(frame.min(next_refresh.saturating_duration_since(Instant::now())))? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => app.handle_key(key),
                Event::Mouse(mouse) => app.handle_mouse(mouse),
                _ => {}
            }
        }
        if let Some(request) = app.request.take() {
            perform(app, &request, place)?;
        }
        if let Some(line) = place.runner.finished() {
            app.notice = Some(line);
        }
        if Instant::now() >= next_refresh {
            app.now = tx_core::events::now();
            app.clock = started.elapsed().as_secs_f64();
            // A store or log hiccup keeps the last frame; the next refresh retries.
            if let Ok(messages) = feed.refresh(&mut app.source, app.now, app.window) {
                app.receive(messages);
                app.rebuild();
            }
            next_refresh = Instant::now() + REFRESH;
        }
        if app.quit
            && let Some(session) = place.resident
        {
            detach_session(place.tmux, session)?;
            app.quit = false;
        }
    }
    Ok(())
}

/// Carries out `request`; a reason it could not shows in the footer.
fn perform(app: &mut App, request: &Request, place: &Place<'_>) -> io::Result<()> {
    if let Some(words) = actions::words(request) {
        app.notice = Some(place.runner.start(request, words));
        return Ok(());
    }
    let (after, name) = match request {
        Request::Focus { name } => (format!("focus {name}"), name),
        Request::Artifacts { id, name } => (format!("artifacts {id}"), name),
        _ => unreachable!("every other request is a tx verb"),
    };
    let outcome = match place.resident {
        Some(session) => place
            .tmux
            .set_session_option(session, AFTER_OPTION, &after)
            .map_err(|error| error.to_string())
            .and_then(|()| detach_session(place.tmux, session).map_err(|error| error.to_string())),
        None => place
            .tmux
            .terminal_client()
            .map_err(|error| error.to_string())
            .and_then(|client| perform_after(place.tmux, place.tx, &client, None, &after)),
    };
    if let Err(reason) = outcome {
        app.notice = Some(format!("{name}: {reason}"));
    }
    Ok(())
}

/// Carries out an [`AFTER_OPTION`] value on `client`.
/// `pane` is where prefix+g was pressed, when known.
pub fn perform_after(
    tmux: &Ui,
    tx: &Tx,
    client: &str,
    pane: Option<&str>,
    after: &str,
) -> Result<(), String> {
    match after.split_once(' ') {
        Some(("focus", name)) => show_agent(tmux, client, pane, name),
        Some(("artifacts", id)) => artifact_menu(tx, id, client),
        _ => Err(format!("unknown request {after:?}")),
    }
}

/// One frame at `columns` × `rows`, as plain text: no terminal needed.
pub fn snapshot(app: &mut App, columns: u16, rows: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(columns, rows)).expect("test backend");
    terminal
        .draw(|frame| ui::render(app, frame))
        .expect("test backend");
    ui::buffer_text(terminal.backend().buffer())
}
