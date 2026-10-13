//! The terminal and the event loop: raw mode + alternate screen + mouse capture (restored on
//! exit and on panic), keys / mouse / resize multiplexed with the refresh tick.
//!
//! Enter jumps to the cursor lane's session. Run in a pane, the loop focuses the hosting pane
//! itself. The resident (viewed through a popup) cannot reach the outer client, so it leaves the
//! session id in [`GOTO_OPTION`] on its own tmux session and detaches; `tx _timeline-attach`,
//! which runs on the outer client, picks it up and does the focusing.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use ratatui::crossterm::execute;

use super::app::App;
use super::clock;
use super::model::Builder;
use super::ui;
use crate::tmux::Tmux;

/// How often the store and transcripts are re-read while idle.
pub const TICK: Duration = Duration::from_secs(3);
/// Terminals report several wheel events per physical notch: Ctrl+wheel zoom steps at most
/// once per this interval.
pub const WHEEL_ZOOM_INTERVAL: Duration = Duration::from_millis(150);
/// Session option on the resident's tmux session: the session id Enter asked to jump to.
pub const GOTO_OPTION: &str = "@tx_timeline_goto";

/// Focus the pane hosting `session_id` for the terminal the caller is shown on. The caller may
/// itself sit in a nested session, so the client to move is the outer one (never the implicit
/// current client: switching a nested client onto its own host view collapses the view). A pane
/// in the outer client's session is selected in place; one in another plain session gets the
/// outer client switched to it; one in another view is reported, not switched to — a view is a
/// terminal's workspace, not a switch target. `Err` is the reason, for the user.
pub fn focus_session_pane(tmux: &Tmux, session_id: &str) -> Result<(), String> {
    let client = tmux
        .current_client_name()
        .ok_or_else(|| "not inside tmux".to_owned())?;
    let (outer, outer_session) = tmux
        .outer_client(&client)
        .ok_or_else(|| "no terminal client to move".to_owned())?;
    let pane = tmux
        .pane_for_session(session_id, &outer_session)
        .ok_or_else(|| "not attached to any pane".to_owned())?;
    let host = pane.split(':').next().unwrap_or_default();
    let focused = if host == outer_session {
        tmux.select_window(&pane) && tmux.select_pane(&pane)
    } else if tmux.is_view(host) {
        return Err(format!("attached in view {host}, not in {outer_session}"));
    } else {
        tmux.switch_client_of(&outer, &pane).is_ok()
    };
    if focused {
        Ok(())
    } else {
        Err(format!("tmux could not focus {pane}"))
    }
}

/// `resident` is the tmux session name of a popup-hosted timeline: q and Enter detach it
/// instead of quitting.
pub fn run(
    mut builder: Builder,
    days: f64,
    window: f64,
    tmux: &Tmux,
    resident: Option<&str>,
) -> io::Result<()> {
    let days = days.max(window / 1440.0);
    let payload = builder.build(days, clock::now());
    let mut app = App::new(payload, clock::now(), window);
    let mut terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = event_loop(&mut terminal, &mut app, &mut builder, days, tmux, resident);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    builder: &mut Builder,
    days: f64,
    tmux: &Tmux,
    resident: Option<&str>,
) -> io::Result<()> {
    terminal.draw(|frame| ui::render(app, frame))?;
    app.fit_initial();
    let mut next_tick = Instant::now() + TICK;
    let mut last_wheel_zoom: Option<Instant> = None;
    while !app.quit {
        terminal.draw(|frame| ui::render(app, frame))?;
        let wait = next_tick.saturating_duration_since(Instant::now());
        if event::poll(wait)? {
            dispatch(app, terminal, event::read()?, &mut last_wheel_zoom)?;
            // drain what arrived in the same burst (wheel notches, drag moves)
            while event::poll(Duration::ZERO)? {
                dispatch(app, terminal, event::read()?, &mut last_wheel_zoom)?;
            }
        }
        if let Some(index) = app.goto.take() {
            jump(app, index, tmux, resident)?;
        }
        if Instant::now() >= next_tick {
            let payload = builder.build(days, clock::now());
            app.refresh(payload, clock::now());
            next_tick = Instant::now() + TICK;
        }
        if app.quit && let Some(session) = resident {
            tmux.detach_session(session).map_err(io::Error::other)?;
            app.quit = false;
        }
    }
    Ok(())
}

/// Enter on lane `index`: focus its session's pane, or tell the user why not.
fn jump(app: &mut App, index: usize, tmux: &Tmux, resident: Option<&str>) -> io::Result<()> {
    let lane = &app.payload.lanes[index];
    let outcome = match resident {
        Some(session) => {
            if tmux.pane_for_session(&lane.sid, "").is_none() {
                Err("not attached to any pane".to_owned())
            } else {
                tmux.set_session_option(session, GOTO_OPTION, &lane.sid)
                    .and_then(|()| tmux.detach_session(session))
                    .map_err(|error| error.to_string())
            }
        }
        None => focus_session_pane(tmux, &lane.sid),
    };
    if let Err(reason) = outcome {
        app.notice = Some(format!("{}: {reason}", lane.name));
    }
    Ok(())
}

fn is_wheel_zoom(mouse: &ratatui::crossterm::event::MouseEvent) -> bool {
    matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) && mouse.modifiers.contains(KeyModifiers::CONTROL)
}

fn dispatch(
    app: &mut App,
    terminal: &mut ratatui::DefaultTerminal,
    event: Event,
    last_wheel_zoom: &mut Option<Instant>,
) -> io::Result<()> {
    match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => app.handle_key(key),
        Event::Mouse(mouse) if is_wheel_zoom(&mouse) => {
            let now = Instant::now();
            let throttled =
                last_wheel_zoom.is_some_and(|last| now.duration_since(last) < WHEEL_ZOOM_INTERVAL);
            if !throttled {
                *last_wheel_zoom = Some(now);
                app.handle_mouse(mouse);
            }
        }
        Event::Mouse(mouse) => app.handle_mouse(mouse),
        Event::Resize(_, _) => {
            // the geometry is only known after a draw; re-glue to now with the new width
            terminal.draw(|frame| ui::render(app, frame))?;
            app.resized();
        }
        _ => {}
    }
    Ok(())
}

/// One frame at `columns` × `rows`, as plain text — no terminal needed.
pub fn snapshot(
    mut builder: Builder,
    days: f64,
    window: f64,
    columns: u16,
    rows: u16,
) -> io::Result<String> {
    let days = days.max(window / 1440.0);
    let payload = builder.build(days, clock::now());
    let mut app = App::new(payload, clock::now(), window);
    let mut terminal = Terminal::new(TestBackend::new(columns, rows)).expect("test backend");
    terminal
        .draw(|frame| ui::render(&mut app, frame))
        .expect("test backend");
    app.fit_initial();
    terminal
        .draw(|frame| ui::render(&mut app, frame))
        .expect("test backend");
    Ok(ui::buffer_text(terminal.backend().buffer()))
}

pub fn print_snapshot(
    builder: Builder,
    days: f64,
    window: f64,
    columns: u16,
    rows: u16,
) -> io::Result<()> {
    let text = snapshot(builder, days, window, columns, rows)?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{text}")
}
