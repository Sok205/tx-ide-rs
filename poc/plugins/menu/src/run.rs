//! The terminal loop: draw at the shimmer's [`FPS`], feed key presses to the [`Menu`], return
//! what it chose. Raw mode and the alternate screen are restored on every exit path.

use std::io;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::model::{Action, Menu, Outcome};
use crate::ui::{self, FPS};

pub fn run(menu: &mut Menu) -> io::Result<Option<Action>> {
    let mut terminal = ratatui::try_init()?;
    let result = event_loop(menu, &mut terminal);
    ratatui::restore();
    result
}

fn event_loop(
    menu: &mut Menu,
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
        match menu.key(key.code) {
            Outcome::Stay => {}
            Outcome::Close => return Ok(None),
            Outcome::Run(action) => return Ok(Some(action)),
        }
    }
}
