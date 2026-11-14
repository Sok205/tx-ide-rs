//! The terminal and the event loop: raw mode, the alternate screen and mouse capture, restored
//! on exit and on panic.
//!
//! Every [`REFRESH`] the loop re-reads the records, follows core's log and reads what the
//! transcripts appended; each new message starts a pulse. It draws at [`FAST_FRAME`] while a
//! pulse is in flight and at [`FRAME`] otherwise, enough for the breathing and the spinner.
//!
//! Enter jumps to the selected agent. Run in a pane, the loop focuses the agent's pane itself.
//! The resident (viewed through a popup) cannot reach the outer client, so it leaves the agent's
//! name in [`GOTO_OPTION`] on its own tmux session and detaches; `tx _graph-attach`, which runs
//! on the outer client, picks it up and does the focusing (the timeline's mechanism).

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

use crate::app::{App, Source};
use crate::model::{self, Filter, LogTail, NodeState, TranscriptReader};
use crate::ui;

/// The redraw pace with nothing travelling: enough for the spinner and the breathing.
pub const FRAME: Duration = Duration::from_millis(250);
/// The redraw pace while a pulse is in flight.
pub const FAST_FRAME: Duration = Duration::from_millis(33);
/// How often the records, the log and the transcripts are read again.
pub const REFRESH: Duration = Duration::from_secs(1);
/// Session option on the resident's tmux session: the agent Enter asked to jump to.
pub const GOTO_OPTION: &str = "@tx_graph_goto";

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

/// The source of one snapshot.
pub fn source(core: &TxCore, now: f64, window: f64) -> io::Result<Source> {
    Feed::new(core).source(now, window)
}

/// Focuses the pane hosting session `name` for the terminal the caller is shown on. `Err` is the
/// reason, for the operator.
pub fn focus_session_pane(tmux: &Ui, name: &str) -> Result<(), String> {
    let client = tmux
        .display_message("#{client_name}", None)
        .ok_or_else(|| "not inside tmux".to_owned())?;
    // The caller's client may be nested in a view's pane; the terminal is the one to move.
    let (client, _) = tmux
        .outer_client(&client)
        .ok_or_else(|| "no terminal client to move".to_owned())?;
    let pane = tmux
        .pane_for_session(name, "")
        .ok_or_else(|| "not attached to any pane".to_owned())?;
    tmux.run(&["switch-client", "-c", &client, "-t", &pane])
        .map_err(|error| error.to_string())?;
    tmux.select_window(&pane);
    tmux.select_pane(&pane);
    Ok(())
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
    let mut terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = event_loop(&mut terminal, &mut app, &mut feed, &tmux, resident);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    feed: &mut Feed<'_>,
    tmux: &Ui,
    resident: Option<&str>,
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
        if let Some(index) = app.goto.take() {
            jump(app, index, tmux, resident)?;
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
            && let Some(session) = resident
        {
            detach_session(tmux, session)?;
            app.quit = false;
        }
    }
    Ok(())
}

/// Enter on node `index`: focus its agent's pane, or tell the operator why not.
fn jump(app: &mut App, index: usize, tmux: &Ui, resident: Option<&str>) -> io::Result<()> {
    let node = &app.graph.nodes[index];
    let name = node.name.clone();
    let outcome = match node.state {
        NodeState::Operator => Err("that is you".to_owned()),
        NodeState::Ended => Err("ended".to_owned()),
        _ => match resident {
            Some(session) => {
                if tmux.pane_for_session(&name, "").is_none() {
                    Err("not attached to any pane".to_owned())
                } else {
                    tmux.set_session_option(session, GOTO_OPTION, &name)
                        .map_err(|error| error.to_string())
                        .and_then(|()| {
                            detach_session(tmux, session).map_err(|error| error.to_string())
                        })
                }
            }
            None => focus_session_pane(tmux, &name),
        },
    };
    if let Err(reason) = outcome {
        app.notice = Some(format!("{name}: {reason}"));
    }
    Ok(())
}

/// One frame at `columns` × `rows`, as plain text: no terminal needed.
pub fn snapshot(app: &mut App, columns: u16, rows: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(columns, rows)).expect("test backend");
    terminal
        .draw(|frame| ui::render(app, frame))
        .expect("test backend");
    ui::buffer_text(terminal.backend().buffer())
}
