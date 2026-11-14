//! The terminal and the event loop: raw mode, the alternate screen and mouse capture, restored
//! on exit and on panic. The source is read once at start.

use std::io;
use std::time::Duration;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind,
};
use ratatui::crossterm::execute;
use tx_core::core::LOG_FILE;
use tx_core::{LlmEngineState, TxCore};

use crate::app::{App, Source};
use crate::model::{self, Filter};
use crate::ui;

/// The redraw pace with nothing moving: enough for the spinner.
pub const FRAME: Duration = Duration::from_millis(250);

/// Records, the window's messages and the context of each agent that is not long ended.
pub fn source(core: &TxCore, now: f64, window: f64) -> io::Result<Source> {
    let agents = model::load_agents(core).map_err(io::Error::other)?;
    let log = core.config().directories.data.join(LOG_FILE);
    let messages = model::read_messages(&log, now - window, now)?;
    let contexts = agents
        .iter()
        .filter(|agent| {
            agent.state != LlmEngineState::Ended
                || agent.last_activity_at.unwrap_or(agent.last_modified_at) >= now - window
        })
        .filter_map(|agent| Some((agent.id.clone(), model::transcript_context(core, agent)?)))
        .collect();
    Ok(Source {
        agents,
        messages,
        contexts,
    })
}

pub fn run(core: &TxCore, filter: Filter, window: f64) -> io::Result<()> {
    let now = tx_core::events::now();
    let mut app = App::new(source(core, now, window)?, filter, now, window);
    let mut terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = event_loop(&mut terminal, &mut app);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> io::Result<()> {
    while !app.quit {
        terminal.draw(|frame| ui::render(app, frame))?;
        if event::poll(FRAME)? {
            match event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => app.handle_key(key),
                Event::Mouse(mouse) => app.handle_mouse(mouse),
                _ => {}
            }
        }
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
