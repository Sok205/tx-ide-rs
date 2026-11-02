//! The terminal loop: draw at the shimmer's [`FPS`], feed key presses to the [`Menu`], return
//! what it chose. Raw mode and the alternate screen are restored on every exit path. Keys pass
//! [`accept`] first: no chords, and a first-attach menu starts with a [`GRACE`] period.

use std::io;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::model::{Action, GRACE, Menu, Outcome, accept};
use crate::ui::{self, FPS};

/// `first`: opened by the first attach, so the [`GRACE`] period applies.
pub fn run(menu: &mut Menu, first: bool) -> io::Result<Option<Action>> {
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(menu, first, &mut terminal);
    ratatui::restore();
    result
}

fn event_loop(
    menu: &mut Menu,
    first: bool,
    terminal: &mut ratatui::DefaultTerminal,
) -> io::Result<Option<Action>> {
    let started = Instant::now();
    let tick = Duration::from_millis(1000 / FPS);
    loop {
        let frame = (started.elapsed().as_millis() * u128::from(FPS) / 1000) as usize;
        terminal.draw(|f| ui::draw(menu, frame, f.area(), f.buffer_mut()))?;
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
        match menu.key(code) {
            Outcome::Stay => {}
            Outcome::Close => return Ok(None),
            Outcome::Run(action) => return Ok(Some(action)),
        }
    }
}
