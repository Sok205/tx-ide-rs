//! The terminal loop: draw at [`FPS`], feed key presses to the [`Home`], return what it chose.
//! Raw mode and the alternate screen are restored on every exit path. Keys pass [`accept`]
//! first: no chords, and a home opened by an attach starts with a [`GRACE`] period.

use std::io;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::model::{Action, GRACE, Home, Outcome, accept};
use crate::ui::{self, FPS};

/// `first`: opened by the first attach, so the [`GRACE`] period applies.
pub fn run(home: &mut Home, first: bool) -> io::Result<Option<Action>> {
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(home, first, &mut terminal);
    ratatui::restore();
    result
}

fn event_loop(
    home: &mut Home,
    first: bool,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<Option<Action>> {
    let started = Instant::now();
    let tick = Duration::from_millis(1000 / FPS);
    loop {
        terminal.draw(|f| ui::draw(home, f.area(), f.buffer_mut()))?;
        if !event::poll(tick)? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(None);
        }
        let in_grace = first && started.elapsed() < GRACE;
        let Some(code) = accept(key.code, key.modifiers, in_grace) else {
            continue;
        };
        match home.key(code) {
            Outcome::Stay => {}
            Outcome::Close => return Ok(None),
            Outcome::Run(action) => return Ok(Some(action)),
        }
    }
}
