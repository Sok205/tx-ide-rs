//! The terminal and the event loop: raw mode + alternate screen + mouse capture (restored on
//! exit and on panic), keys / mouse / resize multiplexed with the refresh tick.
//!
//! Enter, a and p show the cursor lane's session through tx-client ([`Ui::show_session`]):
//! Enter focuses the pane already showing it, a lets the operator pick a pane to attach it into
//! (attach-to), p nest-attaches it in a popup. Run in a pane, the loop does it itself. The
//! resident (viewed through a popup) cannot reach the outer client, so it leaves `<mode> <name>`
//! in [`GOTO_OPTION`] on its own tmux session and detaches; `tx _timeline-attach`, which runs
//! on the outer client, picks it up. Every tmux call goes through `tx-client` against the core's
//! own server.

use std::io;
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyModifiers,
    MouseEventKind,
};
use ratatui::crossterm::execute;

use tx_client::show::Show;
use tx_client::tmux::Ui;
use tx_core::{CoreError, TxCore};

use super::app::App;
use super::model::{Builder, Payload, load_sessions, transcripts};
use super::ui;

/// How often the store and transcripts are re-read while idle.
pub const TICK: Duration = Duration::from_secs(3);
/// Terminals report several wheel events per physical notch: Ctrl+wheel zoom steps at most
/// once per this interval.
pub const WHEEL_ZOOM_INTERVAL: Duration = Duration::from_millis(150);
/// Session option on the resident's tmux session: `<mode> <session name>` a pick asked for.
pub const GOTO_OPTION: &str = "@tx_timeline_goto";
/// `detach-client -s`: every client viewing `session` (the popup's nested attach) lets go.
pub fn detach_session(tmux: &Ui, session: &str) -> io::Result<()> {
    tmux.run(&["detach-client", "-s", &format!("={session}")])
        .map(drop)
        .map_err(io::Error::other)
}

/// A payload over the current records; a store error becomes an I/O error for the loop.
pub fn payload(core: &TxCore, builder: &mut Builder, days: f64, now: f64) -> io::Result<Payload> {
    let sessions = load_sessions(core).map_err(|error: CoreError| io::Error::other(error))?;
    let transcripts = transcripts(core, &sessions);
    Ok(builder.build(&sessions, &transcripts, days, now))
}

/// `resident` is the tmux session name of a popup-hosted timeline: q and Enter detach it
/// instead of quitting.
pub fn run(
    core: &TxCore,
    mut builder: Builder,
    days: f64,
    window: f64,
    resident: Option<&str>,
) -> io::Result<()> {
    let tmux = Ui::from_core(core);
    let days = days.max(window / 1440.0);
    let now = tx_core::events::now();
    let mut app = App::new(payload(core, &mut builder, days, now)?, now, window);
    let mut terminal = ratatui::try_init()?;
    execute!(io::stdout(), EnableMouseCapture)?;
    let result = event_loop(
        &mut terminal,
        &mut app,
        Context {
            core,
            builder: &mut builder,
            days,
            tmux: &tmux,
            resident,
        },
    );
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

struct Context<'a> {
    core: &'a TxCore,
    builder: &'a mut Builder,
    days: f64,
    tmux: &'a Ui,
    resident: Option<&'a str>,
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    context: Context<'_>,
) -> io::Result<()> {
    let Context {
        core,
        builder,
        days,
        tmux,
        resident,
    } = context;
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
        if let Some(pick) = app.goto.take() {
            jump(app, pick, tmux, resident)?;
        }
        if Instant::now() >= next_tick {
            let now = tx_core::events::now();
            // A store hiccup keeps the last frame; the next tick retries.
            if let Ok(payload) = payload(core, builder, days, now) {
                app.refresh(payload, now);
            }
            next_tick = Instant::now() + TICK;
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

/// A pick on lane `index`: show its session that way, or tell the user why not. A focus on a
/// session no pane shows is declined here, with the timeline still open.
fn jump(
    app: &mut App,
    (show, index): (Show, usize),
    tmux: &Ui,
    resident: Option<&str>,
) -> io::Result<()> {
    let lane = &app.payload.lanes[index];
    let outcome = if !lane.live {
        Err("not running".to_owned())
    } else if show == Show::Focus && tmux.pane_for_session(&lane.name, "").is_none() {
        Err("not attached to any pane".to_owned())
    } else {
        match resident {
            Some(session) => tmux
                .set_session_option(
                    session,
                    GOTO_OPTION,
                    &format!("{} {}", show.name(), lane.name),
                )
                .map_err(|error| error.to_string())
                .and_then(|()| detach_session(tmux, session).map_err(|e| e.to_string())),
            None => show_here(tmux, show, &lane.name),
        }
    };
    if let Err(reason) = outcome {
        app.notice = Some(format!("{}: {reason}", lane.name));
    }
    Ok(())
}

/// Shows session `name` from the pane this timeline runs in.
fn show_here(tmux: &Ui, show: Show, name: &str) -> Result<(), String> {
    let client = tmux
        .display_message("#{client_name}", None)
        .ok_or_else(|| "not inside tmux".to_owned())?;
    let pane = tmux.current_pane_id();
    tmux.show_session(&client, show, name, pane.as_deref())
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

/// One frame at `columns` × `rows`, as plain text: no terminal needed.
pub fn snapshot(payload: Payload, now: f64, window: f64, columns: u16, rows: u16) -> String {
    let mut app = App::new(payload, now, window);
    let mut terminal = Terminal::new(TestBackend::new(columns, rows)).expect("test backend");
    terminal
        .draw(|frame| ui::render(&mut app, frame))
        .expect("test backend");
    app.fit_initial();
    terminal
        .draw(|frame| ui::render(&mut app, frame))
        .expect("test backend");
    ui::buffer_text(terminal.backend().buffer())
}
