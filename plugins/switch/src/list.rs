//! The list prefix+P shows: every plugin with a box for on and off. j/k move, space flips,
//! Enter returns the flipped ones, q or Esc returns none.

use std::io;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{List, ListState};
use tx_core::PluginStatus;

/// The plugins whose box the user flipped, each with its new state.
pub fn run(plugins: &[(String, PluginStatus)]) -> io::Result<Vec<(String, bool)>> {
    let enabled: Vec<bool> = plugins
        .iter()
        .map(|(_, status)| *status != PluginStatus::Disabled)
        .collect();
    let mut wanted = enabled.clone();
    let mut state = ListState::default().with_selected(Some(0));
    let mut terminal = ratatui::try_init()?;
    let applied = loop {
        terminal.draw(|frame| {
            let [list, hint] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(frame.area());
            let rows = plugins.iter().enumerate().map(|(index, (id, status))| {
                let changed = if wanted[index] == enabled[index] {
                    ' '
                } else {
                    '*'
                };
                let tick = if wanted[index] { 'x' } else { ' ' };
                Line::from(format!("[{tick}]{changed} {id:<16} {}", status.as_str()))
            });
            let rows =
                List::new(rows).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
            frame.render_stateful_widget(rows, list, &mut state);
            frame.render_widget(Line::from("space switch · enter apply · q cancel"), hint);
        })?;
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => state.select_next(),
            KeyCode::Char('k') | KeyCode::Up => state.select_previous(),
            KeyCode::Char(' ') => {
                let index = state.selected().expect("a row is selected");
                wanted[index] = !wanted[index];
            }
            KeyCode::Enter => break true,
            KeyCode::Char('q') | KeyCode::Esc => break false,
            _ => {}
        }
    };
    ratatui::restore();
    if !applied {
        return Ok(Vec::new());
    }
    Ok(plugins
        .iter()
        .zip(wanted.iter().zip(&enabled))
        .filter(|(_, (wanted, enabled))| wanted != enabled)
        .map(|((id, _), (wanted, _))| (id.clone(), *wanted))
        .collect())
}
